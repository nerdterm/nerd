//! Process-launch broker: the one piece of nerd outside the sandbox, started before the daemon applies Seatbelt so panes still get network; the master fd comes back over SCM_RIGHTS, so the broker itself never sees pane I/O.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, IoSlice, IoSliceMut, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};

use crate::bell::Bell;

/// Whoever doesn't leave after SIGHUP gets SIGKILL.
const GRACE: Duration = Duration::from_millis(250);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Its connection ended: a pane still starting will never be answered.
static GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long a pane waits for its spawn's answer: under heavy load a live broker's fork and exec take seconds.
pub fn spawn_wait() -> Duration {
    static WAIT: OnceLock<Duration> = OnceLock::new();
    // Test aid, like NERD_BROKER_DELAY_MS: a test of a silent broker waits less.
    *WAIT.get_or_init(|| std::env::var("NERD_BROKER_WAIT_MS").ok().and_then(|s| s.parse().ok()).map_or(Duration::from_secs(30), Duration::from_millis))
}

pub fn gone() -> bool {
    GONE.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Serialize, Deserialize)]
pub enum Req {
    Spawn {
        id: usize,
        argv: Vec<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
        env: Vec<(String, String)>,
    },
    Resize { id: usize, cols: u16, rows: u16 },
    Kill { id: usize },
    Ps,
    /// A plugin: no pty, its own session, its own ids and exit notice.
    Exec {
        id: usize,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        cwd: PathBuf,
    },
    StopExec { id: usize },
}

/// `Spawned` arrives together with the master descriptor in SCM_RIGHTS.
#[derive(Serialize, Deserialize)]
pub enum Rep {
    Spawned { id: usize, pid: u32 },
    Exited {
        id: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        how: Option<String>,
    },
    Error { id: usize, message: String },
    Ps { text: String },
    Started { id: usize, pid: u32 },
    ExecFailed { id: usize, message: String },
    ExecExited {
        id: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        how: Option<String>,
    },
}

/// What this build's broker understands; a handover carries the adopted broker's, 0 before `Exec` existed.
pub const PROTOCOL: u32 = 1;

/// What the broker says about a plugin it launched.
pub enum ExecNote {
    Started { id: usize, pid: i32 },
    Failed { id: usize, message: String },
    /// `how`: None from an older broker.
    Exited { id: usize, how: Option<String> },
}

/// A pane's process left: its id, and how (None from an older broker).
pub type Exit = (usize, Option<String>);

/// The broker's answer to `Req::Spawn` for a pane id: its pid (None for 0) and master, or why not.
pub type Spawned = (usize, Result<(Option<u32>, OwnedFd), String>);

pub struct Broker {
    req: Mutex<UnixStream>,
    ps_reply: Mutex<Receiver<String>>,
}

static BROKER: OnceLock<Arc<Broker>> = OnceLock::new();
/// A broker of this build beside an adopted one too old to launch plugins; it runs only plugins.
static EXEC: OnceLock<Arc<Broker>> = OnceLock::new();
static EXEC_COMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Its connection ended: plugins are off until the next update.
static EXEC_GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// What it runs now, so they can be told gone if it goes.
static EXEC_LIVE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// The helper being started, and its pid once the old broker says it (-1: it could not).
static HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HELPER_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Pane ids from here up are helpers the old broker runs for us, never panes.
const HELPER_IDS: usize = usize::MAX / 2;
/// How long the helper has to connect back.
const HELPER_WAIT: Duration = Duration::from_secs(10);
static PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static SPEAKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The running broker's protocol: this build's for one it started, the handover's for one it adopted.
pub fn protocol() -> u32 {
    SPEAKS.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn handle() -> Option<&'static Arc<Broker>> {
    BROKER.get()
}

/// Where plugins run: the broker when it is this build's kind, else the one started beside it.
pub fn exec_handle() -> Option<&'static Arc<Broker>> {
    if EXEC_GONE.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    EXEC.get().or_else(|| handle().filter(|_| protocol() >= 1))
}

/// The broker for plugins is still on its way.
pub fn exec_coming() -> bool {
    EXEC.get().is_none() && EXEC_COMING.load(std::sync::atomic::Ordering::SeqCst)
}

/// After a handover from a broker that cannot launch plugins: it opens a hidden pane running this
/// build as a second broker, which connects back once and then runs the plugins. Only the pid the old
/// broker reports is accepted: a plugin allowed the network could read the token from the helper's
/// environment and reach the socket, and whoever is accepted vouches for every plugin's pid.
pub fn start_beside(notes: Sender<ExecNote>, bell: Bell) -> std::io::Result<()> {
    let old = handle().ok_or_else(|| std::io::Error::other("no broker"))?;
    let path = crate::session::exec_socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let mut raw = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut raw)?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0);
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let argv = [exe, "--exec-broker".into(), path.to_string_lossy().into_owned()];
    EXEC_COMING.store(true, std::sync::atomic::Ordering::SeqCst);
    HELPER_PID.store(0, std::sync::atomic::Ordering::SeqCst);
    HELPER.store(HELPER_IDS + nanos, std::sync::atomic::Ordering::SeqCst);
    let cwd = crate::paths::home().unwrap_or_else(|| PathBuf::from("/"));
    if let Err(e) = old.spawn(HELPER_IDS + nanos, &argv, &cwd, 80, 24, vec![("NERD_EXEC_TOKEN".into(), token.clone())]) {
        EXEC_COMING.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    std::thread::spawn(move || {
        let helper = || HELPER_PID.load(std::sync::atomic::Ordering::SeqCst);
        let got = accept_helper(&listener, &token, helper, std::time::Instant::now() + HELPER_WAIT);
        let _ = std::fs::remove_file(&path);
        match got.map(|s| install_exec(s, notes, bell.clone())) {
            Some(Ok(())) => crate::log::info("plugins: a broker of this build runs them beside the older one"),
            Some(Err(e)) => crate::log::warn(&format!("plugins off: the broker for plugins did not start: {e}")),
            None => crate::log::warn(&format!("plugins off: the broker for plugins did not connect; {}", crate::plugins::OLD_BROKER)),
        }
        EXEC_COMING.store(false, std::sync::atomic::Ordering::SeqCst);
        bell.ring();
    });
    Ok(())
}

/// The one connection from the helper's pid that says the token; anything else is dropped unread.
fn accept_helper(listener: &std::os::unix::net::UnixListener, token: &str, helper: impl Fn() -> i32, deadline: std::time::Instant) -> Option<UnixStream> {
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((s, _)) => {
                let peer = nix::sys::socket::getsockopt(&s, nix::sys::socket::sockopt::LocalPeerPid).unwrap_or(0);
                // The old broker's answer can come a moment after the helper connects.
                while helper() == 0 && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if peer > 0 && peer == helper() && knows(&s, token) {
                    return Some(s);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if helper() < 0 {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
    None
}

/// The first line must be the token, within a second.
fn knows(s: &UnixStream, token: &str) -> bool {
    if s.set_nonblocking(false).is_err() || s.set_read_timeout(Some(Duration::from_secs(1))).is_err() {
        return false;
    }
    let mut line = String::new();
    let ok = BufReader::new(s).read_line(&mut line).is_ok() && line.trim_end() == token;
    ok && s.set_read_timeout(None).is_ok()
}

fn install_exec(sock: UnixStream, notes: Sender<ExecNote>, bell: Bell) -> std::io::Result<()> {
    let (spawns, _) = channel();
    let (exits, _) = channel();
    let (ps_tx, ps_rx) = channel();
    let b = Arc::new(Broker { req: Mutex::new(sock.try_clone()?), ps_reply: Mutex::new(ps_rx) });
    EXEC.set(b).map_err(|_| std::io::Error::other("already running"))?;
    std::thread::spawn(move || {
        replies(sock, spawns, ps_tx, exits, notes.clone(), bell.clone());
        EXEC_GONE.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::log::warn(&format!("plugins off: the broker for plugins ended; {}", crate::plugins::OLD_BROKER));
        let live: Vec<usize> = std::mem::take(&mut *EXEC_LIVE.lock().unwrap_or_else(|e| e.into_inner()));
        for id in live {
            crate::plugins::exited(id);
            let _ = notes.send(ExecNote::Exited { id, how: Some(crate::plugins::BROKER_ENDED.into()) });
        }
        bell.ring();
    });
    Ok(())
}

/// The broker's pid, or this process's own before a broker is known: panes descend from either.
pub fn pid() -> i32 {
    match PID.load(std::sync::atomic::Ordering::SeqCst) {
        0 => std::process::id() as i32,
        p => p,
    }
}

/// Must be called by the daemon before `sandbox::apply`.
/// Exit notices go to `exits` from the first one on, so none is lost before a pane is adopted.
pub fn start(exits: Sender<Exit>, notes: Sender<ExecNote>, spawns: Sender<Spawned>, bell: Bell) -> std::io::Result<()> {
    let (mine, theirs) = UnixStream::pair()?;
    let exe = std::env::current_exe()?;
    let fd = theirs.as_raw_fd();
    let mut cmd = crate::spawn::cmd(exe);
    cmd.arg("--broker")
        .env("NERD_BROKER_FD", "3")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::spawn::pass(&mut cmd, fd, 3);
    let mut child = cmd.spawn()?;
    PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    SPEAKS.store(PROTOCOL, std::sync::atomic::Ordering::SeqCst);
    drop(theirs);
    // Without wait(), a broker that dies early would stay a zombie.
    std::thread::spawn(move || match child.wait() {
        Ok(s) => crate::log::warn(&format!("broker exited ({})", std_how(s))),
        Err(e) => crate::log::warn(&format!("broker exited (not seen how: {e})")),
    });
    install(mine, exits, notes, spawns, bell)
}

// ceiling: nobody waits() on an adopted broker, so it zombies until the daemon exits.
/// `pid` 0: a handover from a build that did not record it.
pub fn adopt(fd: RawFd, pid: i32, protocol: u32, exits: Sender<Exit>, notes: Sender<ExecNote>, spawns: Sender<Spawned>, bell: Bell) -> std::io::Result<()> {
    PID.store(pid, std::sync::atomic::Ordering::SeqCst);
    SPEAKS.store(protocol, std::sync::atomic::Ordering::SeqCst);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
    let mine = unsafe { UnixStream::from_raw_fd(fd) };
    install(mine, exits, notes, spawns, bell)
}

fn install(mine: UnixStream, exits: Sender<Exit>, notes: Sender<ExecNote>, spawns: Sender<Spawned>, bell: Bell) -> std::io::Result<()> {
    let (ps_tx, ps_rx) = channel();
    let b = Arc::new(Broker { req: Mutex::new(mine.try_clone()?), ps_reply: Mutex::new(ps_rx) });
    std::thread::spawn(move || {
        // A clone, so `spawns` disconnects only after GONE is set.
        replies(mine, spawns.clone(), ps_tx, exits, notes, bell.clone());
        GONE.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::log::warn("broker ended; new panes cannot launch");
        bell.ring();
    });
    let _ = BROKER.set(b);
    Ok(())
}

/// `Spawned` is the only message carrying a descriptor, so pairing it FIFO with the fd queue is exact.
fn replies(sock: UnixStream, spawns: Sender<Spawned>, ps_tx: Sender<String>, exits: Sender<Exit>, notes: Sender<ExecNote>, bell: Bell) {
    let mut rx = FdReader::new(sock);
    while let Some((rep, fd)) = rx.next() {
        match rep {
            // A helper: its pane is the old broker's to keep; once it ends, the old broker forgets it.
            Rep::Spawned { id, pid } if id >= HELPER_IDS => {
                if id == HELPER.load(std::sync::atomic::Ordering::SeqCst) {
                    HELPER_PID.store(pid as i32, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Rep::Error { id, message } if id >= HELPER_IDS => {
                crate::log::warn(&format!("plugins off: the older broker could not start the broker for plugins: {message}"));
                if id == HELPER.load(std::sync::atomic::Ordering::SeqCst) {
                    HELPER_PID.store(-1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Rep::Exited { id, .. } if id >= HELPER_IDS => {
                // Gone before it connected: stop waiting, and never accept whoever gets its pid next.
                if id == HELPER.load(std::sync::atomic::Ordering::SeqCst) && EXEC.get().is_none() {
                    HELPER_PID.store(-1, std::sync::atomic::Ordering::SeqCst);
                }
                if let Some(b) = handle() {
                    b.kill(id);
                }
            }
            Rep::Spawned { id, pid } => {
                let got = fd.map(|fd| (Some(pid).filter(|p| *p != 0), fd)).ok_or_else(|| "master did not arrive".to_string());
                let _ = spawns.send((id, got));
                bell.ring();
            }
            Rep::Error { id, message } => {
                let _ = spawns.send((id, Err(message)));
                bell.ring();
            }
            Rep::Exited { id, how } => {
                let _ = exits.send((id, how));
                bell.ring();
            }
            Rep::Ps { text } => {
                let _ = ps_tx.send(text);
            }
            Rep::Started { id, pid } => {
                crate::plugins::started(id, pid as i32);
                let _ = notes.send(ExecNote::Started { id, pid: pid as i32 });
                bell.ring();
            }
            Rep::ExecFailed { id, message } => {
                EXEC_LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| *x != id);
                let _ = notes.send(ExecNote::Failed { id, message });
                bell.ring();
            }
            Rep::ExecExited { id, how } => {
                EXEC_LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| *x != id);
                crate::plugins::exited(id);
                let _ = notes.send(ExecNote::Exited { id, how });
                bell.ring();
            }
        }
    }
}

impl Broker {
    fn send(&self, req: &Req) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        let mut g = self.req.lock().unwrap_or_else(|e| e.into_inner());
        g.write_all(&line)?;
        g.flush()
    }

    /// Returns once the request is written; the answer comes back as a `Spawned`.
    pub fn spawn(&self, id: usize, argv: &[String], cwd: &std::path::Path, cols: u16, rows: u16, env: Vec<(String, String)>) -> std::io::Result<()> {
        self.send(&Req::Spawn { id, argv: argv.to_vec(), cwd: cwd.to_path_buf(), cols, rows, env })
    }

    pub fn export_fd(&self) -> std::io::Result<RawFd> {
        let fd = self.req.lock().unwrap_or_else(|e| e.into_inner()).as_raw_fd();
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()))?;
        Ok(fd)
    }

    pub fn resize(&self, id: usize, cols: u16, rows: u16) {
        let _ = self.send(&Req::Resize { id, cols, rows });
    }

    pub fn kill(&self, id: usize) {
        let _ = self.send(&Req::Kill { id });
    }

    /// Returns once the request is written; the answer comes back as an `ExecNote`.
    pub fn exec(&self, id: usize, argv: Vec<String>, env: Vec<(String, String)>, cwd: PathBuf) -> std::io::Result<()> {
        if EXEC.get().is_some_and(|b| std::ptr::eq(b.as_ref(), self)) {
            EXEC_LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(id);
        }
        self.send(&Req::Exec { id, argv, env, cwd })
    }

    pub fn stop_exec(&self, id: usize) {
        let _ = self.send(&Req::StopExec { id });
    }

    fn ps_via_broker(&self) -> Option<String> {
        let rx = self.ps_reply.lock().unwrap_or_else(|e| e.into_inner());
        while rx.try_recv().is_ok() {}
        self.send(&Req::Ps).ok()?;
        rx.recv_timeout(REPLY_TIMEOUT).ok().filter(|t| !t.is_empty())
    }
}

/// `/bin/ps` is setuid and the sandbox forbids setuid programs, so it runs through the broker; without one (tests) it runs directly.
pub fn ps() -> Option<String> {
    if let Some(b) = handle() {
        return b.ps_via_broker();
    }
    ps_local()
}

fn ps_local() -> Option<String> {
    let out = crate::spawn::cmd("ps")
        .args(["-axww", "-o", "pid=,ppid=,pgid=,args="])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

struct FdReader {
    sock: UnixStream,
    buf: Vec<u8>,
    fds: std::collections::VecDeque<OwnedFd>,
}

impl FdReader {
    fn new(sock: UnixStream) -> Self {
        Self { sock, buf: Vec::new(), fds: Default::default() }
    }

    fn next(&mut self) -> Option<(Rep, Option<OwnedFd>)> {
        loop {
            if let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                let Ok(rep) = serde_json::from_slice::<Rep>(&line[..line.len() - 1]) else {
                    continue;
                };
                let fd = matches!(rep, Rep::Spawned { .. }).then(|| self.fds.pop_front()).flatten();
                return Some((rep, fd));
            }
            let mut chunk = [0u8; 8192];
            let mut cmsg = nix::cmsg_space!(RawFd);
            let (bytes, got) = {
                let mut iov = [IoSliceMut::new(&mut chunk)];
                let msg = recvmsg::<()>(
                    self.sock.as_raw_fd(),
                    &mut iov,
                    Some(&mut cmsg),
                    MsgFlags::empty(),
                )
                .ok()?;
                let mut got: Vec<RawFd> = Vec::new();
                if let Ok(cmsgs) = msg.cmsgs() {
                    for c in cmsgs {
                        if let ControlMessageOwned::ScmRights(fds) = c {
                            got.extend(fds);
                        }
                    }
                }
                (msg.bytes, got)
            };
            for fd in got {
                self.fds.push_back(unsafe { OwnedFd::from_raw_fd(fd) });
            }
            if bytes == 0 {
                return None;
            }
            self.buf.extend_from_slice(&chunk[..bytes]);
        }
    }
}

struct Child {
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

/// `nerd --broker`: internal, started only by the daemon.
pub fn run() -> std::io::Result<()> {
    let fd: RawFd = std::env::var("NERD_BROKER_FD").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    // dup2 left it inheritable: a plugin holding it could speak for the broker to the daemon.
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
    serve(unsafe { UnixStream::from_raw_fd(fd) })
}

/// `nerd --exec-broker <socket>`: internal, the hidden pane `start_beside` asks an older broker for.
pub fn run_beside(path: &str) -> std::io::Result<()> {
    let token = std::env::var("NERD_EXEC_TOKEN").map_err(|_| std::io::Error::other("no token"))?;
    let mut sock = UnixStream::connect(path)?;
    sock.write_all(format!("{token}\n").as_bytes())?;
    // Its pty closes when the old broker goes; it must outlive that to stop its plugins itself.
    // SAFETY: setting a disposition installs no handler.
    unsafe { nix::sys::signal::signal(Signal::SIGHUP, nix::sys::signal::SigHandler::SigIgn) }.map_err(std::io::Error::from)?;
    serve(sock)
}

fn serve(sock: UnixStream) -> std::io::Result<()> {
    let out = Arc::new(Mutex::new(sock.try_clone()?));
    let kids: Arc<Mutex<HashMap<usize, Child>>> = Arc::new(Mutex::new(HashMap::new()));
    // Plugin process groups by id; a group leader's pid is also its group id.
    let groups: Arc<Mutex<HashMap<usize, i32>>> = Arc::new(Mutex::new(HashMap::new()));
    // Test aid, like NERD_PANIC_TEST: a broker slow to answer spawns.
    let delay = std::env::var("NERD_BROKER_DELAY_MS").ok().and_then(|s| s.parse().ok()).map(Duration::from_millis);

    let reader = BufReader::new(sock);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Req>(&line) else { continue };
        match req {
            Req::Spawn { id, argv, cwd, cols, rows, env } => {
                if let Some(d) = delay {
                    std::thread::sleep(d);
                }
                match open_pane(id, &argv, &cwd, cols, rows, &env, &out) {
                    Ok((child, pid, master_fd)) => {
                        kids.lock().unwrap_or_else(|e| e.into_inner()).insert(id, child);
                        reply(&out, &Rep::Spawned { id, pid }, Some(master_fd));
                    }
                    Err(e) => reply(&out, &Rep::Error { id, message: e.to_string() }, None),
                }
            }
            Req::Resize { id, cols, rows } => {
                if let Some(c) = kids.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
                    let _ = c.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                }
            }
            Req::Kill { id } => {
                let c = kids.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                if let Some(c) = c {
                    reap(c);
                }
            }
            Req::Ps => {
                let text = ps_local().unwrap_or_default();
                reply(&out, &Rep::Ps { text }, None);
            }
            Req::Exec { id, argv, env, cwd } => match exec(&argv, &env, &cwd) {
                Ok(mut child) => {
                    let pid = child.id() as i32;
                    groups.lock().unwrap_or_else(|e| e.into_inner()).insert(id, pid);
                    reply(&out, &Rep::Started { id, pid: pid as u32 }, None);
                    let (out, groups) = (out.clone(), groups.clone());
                    std::thread::spawn(move || {
                        let how = child.wait().ok().map(std_how);
                        // Whatever it left behind in its group goes with it.
                        let _ = killpg(pid, Signal::SIGKILL);
                        groups.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        reply(&out, &Rep::ExecExited { id, how }, None);
                    });
                }
                Err(e) => reply(&out, &Rep::ExecFailed { id, message: e.to_string() }, None),
            },
            Req::StopExec { id } => {
                if let Some(pg) = groups.lock().unwrap_or_else(|e| e.into_inner()).get(&id).copied() {
                    std::thread::spawn(move || {
                        let _ = killpg(pg, Signal::SIGTERM);
                        std::thread::sleep(GRACE);
                        let _ = killpg(pg, Signal::SIGKILL);
                    });
                }
            }
        }
    }

    let pgs: Vec<i32> = groups.lock().unwrap_or_else(|e| e.into_inner()).values().copied().collect();
    for pg in &pgs {
        let _ = killpg(*pg, Signal::SIGTERM);
    }

    // The daemon dropped off; take the panes down with it or the shells are left with no owner.
    let all: Vec<Child> = kids.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, c)| c).collect();
    let n = all.len();
    for c in all {
        reap(c);
    }
    if n > 0 || !pgs.is_empty() {
        std::thread::sleep(GRACE + Duration::from_millis(50));
    }
    for pg in pgs {
        let _ = killpg(pg, Signal::SIGKILL);
    }
    Ok(())
}

fn killpg(pg: i32, sig: Signal) -> nix::Result<()> {
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pg), sig)
}

/// Its own session and process group, so a stop reaches everything it started.
fn exec(argv: &[String], env: &[(String, String)], cwd: &std::path::Path) -> std::io::Result<std::process::Child> {
    let Some(first) = argv.first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let mut cmd = crate::spawn::cmd(first);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env_remove("NERD_EXEC_TOKEN")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        use std::os::unix::process::CommandExt;
        // A broker beside an older one ignores SIGHUP; its plugins must not inherit that.
        cmd.pre_exec(|| {
            nix::sys::signal::signal(Signal::SIGHUP, nix::sys::signal::SigHandler::SigDfl).map_err(std::io::Error::from)?;
            nix::unistd::setsid().map(drop).map_err(std::io::Error::from)
        });
    }
    cmd.spawn()
}

/// Closing the master and the killer both send SIGHUP only: the broker for plugins relies on ignoring it.
fn reap(c: Child) {
    drop(c.master);
    let mut killer = c.killer;
    std::thread::spawn(move || {
        std::thread::sleep(GRACE);
        let _ = killer.kill();
    });
}

type Opened = (Child, u32, RawFd);

fn open_pane(
    id: usize,
    argv: &[String],
    cwd: &std::path::Path,
    cols: u16,
    rows: u16,
    env: &[(String, String)],
    out: &Arc<Mutex<UnixStream>>,
) -> std::io::Result<Opened> {
    // An empty argv can arrive from an old snapshot; don't panic on it.
    let Some(first) = argv.first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let sys = NativePtySystem::default();
    let pair = sys
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let mut cmd = CommandBuilder::new(first);
    for a in &argv[1..] {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Strip tmux/screen vars inherited from the client: Claude Code uses them to
    // route terminal queries through a tmux shim and would hang waiting for a reply.
    for k in ["TMUX", "TMUX_PANE", "STY", "WINDOW"] {
        cmd.env_remove(k);
    }
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| std::io::Error::other(e.to_string()))?;
    let pid = child.process_id().unwrap_or(0);
    let killer = child.clone_killer();
    drop(pair.slave);

    let master_fd = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| std::io::Error::other("master has no descriptor"))?;

    // EOF on the master won't come while a backgrounded job still holds the slave open
    // after the shell itself exits, so wait() on the process instead.
    let out = out.clone();
    std::thread::spawn(move || {
        let how = child.wait().ok().map(pty_how);
        reply(&out, &Rep::Exited { id, how }, None);
    });

    Ok((Child { master: pair.master, killer }, pid, master_fd))
}

/// portable-pty keeps only strsignal's text, which on macOS ends in the number: "Killed: 9".
fn pty_how(s: portable_pty::ExitStatus) -> String {
    let sig = s.signal().map(|t| t.rsplit([' ', ':']).next().and_then(|n| n.parse().ok()).unwrap_or(0));
    exit_how(s.exit_code() as i32, sig)
}

fn std_how(s: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    exit_how(s.code().unwrap_or(-1), s.signal())
}

/// How a process left, worded the same for panes, plugins and the broker: `code N` or `signal N (SIGNAME)`.
fn exit_how(code: i32, signal: Option<i32>) -> String {
    match signal {
        Some(n) => format!("signal {n} ({})", Signal::try_from(n).map_or("unknown", |s| s.as_str())),
        None => format!("code {code}"),
    }
}

/// `fd` travels in SCM_RIGHTS on the same `sendmsg` as the line, so it's guaranteed to arrive no later.
fn reply(out: &Arc<Mutex<UnixStream>>, rep: &Rep, fd: Option<RawFd>) {
    let Ok(mut line) = serde_json::to_vec(rep) else { return };
    line.push(b'\n');
    let mut g = out.lock().unwrap_or_else(|e| e.into_inner());
    let raw = g.as_raw_fd();
    let fds = [fd.unwrap_or(-1)];
    let cmsgs: &[ControlMessage] =
        if fd.is_some() { &[ControlMessage::ScmRights(&fds)] } else { &[] };
    let sent = sendmsg::<()>(raw, &[IoSlice::new(&line)], cmsgs, MsgFlags::empty(), None);
    if let Ok(n) = sent
        && n < line.len()
    {
        let _ = g.write_all(&line[n..]);
    }
    let _ = g.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(name: &str) -> (std::os::unix::net::UnixListener, PathBuf) {
        let path = std::env::temp_dir().join(format!("nerd-xsock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        l.set_nonblocking(true).unwrap();
        (l, path)
    }

    #[test]
    fn an_exit_says_its_code_or_signal() {
        assert_eq!(exit_how(3, None), "code 3");
        assert_eq!(exit_how(1, Some(9)), "signal 9 (SIGKILL)");
        let st = std::process::Command::new("/bin/sh").args(["-c", "exit 3"]).status().unwrap();
        assert_eq!(std_how(st), "code 3");
        let st = std::process::Command::new("/bin/sh").args(["-c", "kill -TERM $$"]).status().unwrap();
        assert_eq!(std_how(st), "signal 15 (SIGTERM)");
        assert_eq!(pty_how(st.into()), "signal 15 (SIGTERM)", "through portable-pty's strsignal text");
        assert_eq!(pty_how(portable_pty::ExitStatus::with_exit_code(0)), "code 0");
    }

    #[test]
    fn an_older_brokers_exit_parses_and_a_new_one_round_trips() {
        let old: Rep = serde_json::from_str(r#"{"Exited":{"id":3}}"#).unwrap();
        assert!(matches!(old, Rep::Exited { id: 3, how: None }));
        let old: Rep = serde_json::from_str(r#"{"ExecExited":{"id":4}}"#).unwrap();
        assert!(matches!(old, Rep::ExecExited { id: 4, how: None }));
        let line = serde_json::to_string(&Rep::Exited { id: 3, how: Some("code 1".into()) }).unwrap();
        assert!(matches!(serde_json::from_str(&line).unwrap(), Rep::Exited { id: 3, how: Some(h) } if h == "code 1"));
    }

    #[test]
    fn only_the_helpers_pid_with_the_token_becomes_the_broker_for_plugins() {
        let me = std::process::id() as i32;
        let soon = || std::time::Instant::now() + Duration::from_millis(600);

        // Another pid with the right token, as a plugin that read it would send.
        let (l, path) = listener("pid");
        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(b"tok\n").unwrap();
        assert!(accept_helper(&l, "tok", || me + 1, soon()).is_none(), "another pid is refused");

        // Another pid that stays silent is dropped unread, not waited on.
        let _silent = UnixStream::connect(&path).unwrap();
        let t = std::time::Instant::now();
        assert!(accept_helper(&l, "tok", || me + 1, soon()).is_none());
        assert!(t.elapsed() < Duration::from_millis(900), "nothing was read from it: {:?}", t.elapsed());

        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(b"tok\n").unwrap();
        assert!(accept_helper(&l, "tok", || me, soon()).is_some(), "the helper's pid with the token");

        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(b"nope\n").unwrap();
        assert!(accept_helper(&l, "tok", || me, soon()).is_none(), "a wrong token is refused");

        // The old broker said it could not start the helper: no wait for the deadline.
        let t = std::time::Instant::now();
        assert!(accept_helper(&l, "tok", || -1, std::time::Instant::now() + Duration::from_secs(5)).is_none());
        assert!(t.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_file(&path);
    }
}
