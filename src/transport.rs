//! The daemon's side of the socket: accepting clients, reading their messages, writing their screen.

use std::io::{BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use crossterm::event::Event;

use crate::log::{error, warn};
use crate::proto::ClientMsg;

pub enum Inbound {
    Attach {
        id: u64,
        stream: UnixStream,
        cwd: std::path::PathBuf,
        cols: u16,
        rows: u16,
        host: Option<String>,
        program: Option<String>,
        caps: crate::proto::ClientCaps,
        /// The client runs inside one of this server's panes.
        inside: bool,
        /// Refused when another session shows the server.
        if_free: bool,
        /// Says which server a bare `nerd` came back to.
        reopened: bool,
    },
    Event(u64, Event),
    Remote(u64, String, Result<Vec<crate::proto::RemoteInstance>, String>, bool, bool),
    Appearance(u64, bool),
    CellHeight(u64, u16),
    /// How the socket ended and the state of the client's processes at that moment.
    Detach(u64, String),
    Shutdown,
    Update,
    /// `nerd --disconnect`: the summary goes back on `stream`.
    DisconnectAll { id: u64, stream: UnixStream },
    /// `Probe`: the answer goes back on `stream`.
    Probe { stream: UnixStream },
    /// A plugin proved who it is; `id` names this connection.
    PluginUp { name: String, id: u64, stream: UnixStream, events: Vec<String> },
    PluginSaid { name: String, id: u64, pane: usize, state: String },
    /// Slot messages, the latest per key, at most 20 batches a second.
    PluginSlots { name: String, id: u64, msgs: Vec<crate::proto::PluginMsg> },
    PluginDown { name: String, id: u64 },
    /// A message socket peer to identify: its pane and that pane's name, or why not.
    MailHello { pid: i32, uid: u32, started: Option<u64>, accepted: u64, reply: Sender<Result<(usize, Option<String>), String>> },
    /// A message socket request of a verified pane; its reply lines go back on `reply`, which the loop drops when done.
    Mail { pane: usize, uid: u32, req: crate::msock::Request, reply: Sender<String> },
}

/// Client messages for the loop; every send rings its bell.
#[derive(Clone)]
pub struct Inbox {
    pub tx: Sender<Inbound>,
    pub bell: crate::bell::Bell,
}

impl Inbox {
    pub fn send(&self, msg: Inbound) -> Result<(), ()> {
        let r = self.tx.send(msg).map_err(drop);
        self.bell.ring();
        r
    }
}

/// Sockets accepted but not attached yet, so disconnecting everyone reaches them too.
pub type Waiting = Arc<Mutex<std::collections::HashMap<u64, UnixStream>>>;

/// The attached client's only writer: the loop queues chunks and never waits on the socket.
pub struct Writer {
    tx: Sender<Vec<u8>>,
    backlog: Arc<AtomicUsize>,
    stream: UnixStream,
}

/// A backlog this big means a bug upstream; it is logged, never cut.
const TRIPWIRE: usize = 16 << 20;

impl Writer {
    /// `bell` rings when the backlog reaches zero, so the loop sends the frame it held back.
    pub fn spawn(stream: UnixStream, bell: crate::bell::Bell) -> std::io::Result<Writer> {
        // A socket carried over an update keeps the old build's timeout.
        stream.set_write_timeout(None)?;
        // Its own clone, CLOEXEC; this thread is the only code that writes to the stream, and it never locks the Sink.
        let mut out = stream.try_clone()?;
        let (tx, rx) = channel::<Vec<u8>>();
        let backlog = Arc::new(AtomicUsize::new(0));
        let left = backlog.clone();
        std::thread::Builder::new().spawn(move || {
            let gone = loop {
                let Ok(chunk) = rx.recv() else { break None };
                if let Err(e) = out.write_all(&chunk) {
                    break Some(e);
                }
                if left.fetch_sub(chunk.len(), Ordering::SeqCst) == chunk.len() {
                    bell.ring();
                }
            };
            // Receiver first, then zero: a send racing this either fails or is counted and zeroed.
            drop(rx);
            if let Some(e) = gone {
                warn(&format!("client gone: {e}"));
                left.store(0, Ordering::SeqCst);
                bell.ring();
            }
            // The server also holds a reading copy of the socket: without this the client never sees EOF.
            let _ = out.shutdown(std::net::Shutdown::Both);
        })?;
        Ok(Writer { tx, backlog, stream })
    }

    /// Never blocks; `BrokenPipe` once the writer has ended.
    pub fn send(&self, chunk: Vec<u8>) -> std::io::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let n = chunk.len();
        let before = self.backlog.fetch_add(n, Ordering::SeqCst);
        if self.tx.send(chunk).is_err() {
            self.backlog.store(0, Ordering::SeqCst);
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        if before < TRIPWIRE && before + n >= TRIPWIRE {
            warn(&format!("client backlog passed {} MiB", TRIPWIRE >> 20));
        }
        Ok(())
    }

    /// Bytes queued and not yet taken by the socket.
    pub fn backlog(&self) -> usize {
        self.backlog.load(Ordering::SeqCst)
    }

    /// Returns at once: the writer sends what is left and shuts the socket down, or the sleeper does after 2 s.
    pub fn retire(self) {
        let Writer { tx, stream, .. } = self;
        drop(tx);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
    }
}

struct Attached {
    writer: Option<Writer>,
    pending: Vec<u8>,
    bell: crate::bell::Bell,
}

/// Shared via Arc because ratatui does not expose its writer; with no client it draws into the void.
#[derive(Clone)]
pub struct Sink(Arc<Mutex<Attached>>);

impl Sink {
    pub fn new(bell: crate::bell::Bell) -> Sink {
        Sink(Arc::new(Mutex::new(Attached { writer: None, pending: Vec::new(), bell })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Attached> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, stream: Option<UnixStream>) {
        let cur = &mut *self.lock();
        let pending = std::mem::take(&mut cur.pending);
        if let Some(old) = cur.writer.take() {
            let _ = old.send(pending);
            old.retire();
        }
        let bell = cur.bell.clone();
        cur.writer = stream.and_then(|s| {
            Writer::spawn(s.try_clone().ok()?, bell)
                .inspect_err(|e| {
                    error(&format!("client writer did not start: {e}"));
                    let _ = s.shutdown(std::net::Shutdown::Both);
                })
                .ok()
        });
    }

    /// Appends and sends at once, in order behind anything already written.
    fn send_now(&self, bytes: &[u8]) {
        let cur = &mut *self.lock();
        if let Some(w) = cur.writer.as_ref() {
            let mut chunk = std::mem::take(&mut cur.pending);
            chunk.extend_from_slice(bytes);
            let _ = w.send(chunk);
        }
    }

    /// The last word to the client about to be dropped. It leaves the alternate screen first,
    /// so the line lands on the screen the human keeps rather than on the one that is about to
    /// be torn down - otherwise the session simply vanishes with nothing said.
    pub fn farewell(&self, text: &str) {
        self.send_now(farewell_text(text).as_bytes());
    }

    /// A marker for the attached client, outside any frame.
    pub fn say(&self, bytes: &[u8]) {
        self.send_now(bytes);
    }

    pub fn attached(&self) -> bool {
        self.lock().writer.is_some()
    }

    /// The attached client has closed its end, though its reader may not have said so yet.
    pub fn hung_up(&self) -> bool {
        use nix::poll::{PollFd, PollFlags, poll};
        use std::os::fd::AsFd as _;
        let g = self.lock();
        let Some(w) = g.writer.as_ref() else { return false };
        let mut fds = [PollFd::new(w.stream.as_fd(), PollFlags::POLLIN)];
        poll(&mut fds, 0u8).is_ok() && fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLHUP))
    }

    /// Bytes the attached client has not taken yet.
    pub fn backlog(&self) -> usize {
        self.lock().writer.as_ref().map_or(0, Writer::backlog)
    }

    /// Waits up to `limit` for the client to take what is queued: only before an exec or an exit.
    pub fn drain(&self, limit: std::time::Duration) {
        let deadline = std::time::Instant::now() + limit;
        while self.backlog() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub fn export_fd(&self) -> Option<i32> {
        let g = self.lock();
        let fd = g.writer.as_ref()?.stream.as_raw_fd();
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty())).ok()?;
        Some(fd)
    }
}

/// `write` only buffers; `flush` hands the writer one chunk, and ratatui flushes once per frame.
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut cur = self.lock();
        if cur.writer.is_some() {
            cur.pending.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let cur = &mut *self.lock();
        let chunk = std::mem::take(&mut cur.pending);
        match cur.writer.as_ref() {
            Some(w) => w.send(chunk),
            None => Ok(()),
        }
    }
}

pub fn accept_loop(listener: UnixListener, tx: Inbox, carried: u64, waiting: Waiting) {
    std::thread::spawn(move || {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { nix::libc::geteuid() };
        for (n, stream) in listener.incoming().flatten().enumerate() {
            let peer = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerCred).ok().map(|c| c.uid());
            if !same_user(peer, me) {
                warn(&format!("refused a connection from uid {peer:?}"));
                continue;
            }
            let tx = tx.clone();
            let id = carried + n as u64 + 1;
            if let Ok(copy) = stream.try_clone() {
                waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id, copy);
            }
            let waiting = waiting.clone();
            if std::thread::Builder::new().spawn(move || read_client(id, stream, tx, false, waiting, allow_from_panes)).is_err() {
                crate::log::warn_limited("client socket no thread", "refused a connection: no thread left for it");
            }
        }
    });
}

/// The socket's mode is set after bind and a custom path can sit in a shared folder, so each peer is checked.
pub(crate) fn same_user(peer: Option<u32>, me: u32) -> bool {
    peer == Some(me)
}

/// A client's connect: a socket someone else created first at the path is refused before anything is sent.
pub fn connect_own(path: &std::path::Path) -> std::io::Result<UnixStream> {
    let s = UnixStream::connect(path)?;
    let peer = nix::sys::socket::getsockopt(&s, nix::sys::socket::sockopt::LocalPeerCred).ok().map(|c| c.uid());
    // SAFETY: geteuid has no preconditions and cannot fail.
    refuse_stranger(peer, unsafe { nix::libc::geteuid() }, path)?;
    Ok(s)
}

fn refuse_stranger(peer: Option<u32>, me: u32, path: &std::path::Path) -> std::io::Result<()> {
    if same_user(peer, me) {
        return Ok(());
    }
    let who = peer.map_or("an unknown user".to_string(), |u| format!("uid {u}"));
    Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("the server at {} belongs to {who}, not to you; nothing was sent", path.display())))
}

/// The policy switch, read only when a peer inside a pane asks.
pub fn allow_from_panes() -> bool {
    crate::policy::current().allow_from_panes
}

/// How a `Detach` begins when the socket failed rather than closed: the session was lost, it did not leave.
pub const READ_ERROR: &str = "socket error";

/// `greeted`: the client already said Hello to the previous build (handover). `allow`: `allow_from_panes`.
pub fn read_client(id: u64, stream: UnixStream, tx: Inbox, greeted: bool, waiting: Waiting, allow: fn() -> bool) {
    let Ok(out) = stream.try_clone() else {
        waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        return;
    };
    // Before Hello: a client carried over an update never says it, and its detach still names its processes.
    let peer = Peer::of(&out);
    let mut reader = BufReader::new(stream);
    if !greeted {
        let first = ClientMsg::read_from(&mut reader);
        waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let what = match &first {
            Ok(Some(ClientMsg::Hello { .. })) => Some("attach"),
            Ok(Some(ClientMsg::Shutdown)) => Some("stop"),
            Ok(Some(ClientMsg::Update)) => Some("update"),
            Ok(Some(ClientMsg::DisconnectAll)) => Some("disconnect"),
            _ => None,
        };
        if let Some(what) = what
            && refused(&out, what, allow)
        {
            let line = format!("refused: {what} from inside a pane");
            let text = if what == "attach" { farewell_text(&line) } else { format!("{line}\n") };
            let _ = (&out).write_all(text.as_bytes());
            return;
        }
        match first {
            Ok(Some(hello @ ClientMsg::Hello { .. })) => {
                let (Some(caps), ClientMsg::Hello { cwd, cols, rows, host, if_free, reopened, .. }) = (crate::proto::ClientCaps::from_v1_hello(&hello), hello) else { return };
                let program = peer_program(&out);
                let inside = nix::sys::socket::getsockopt(&out, nix::sys::socket::sockopt::LocalPeerPid).is_ok_and(|pid| inside(pid, crate::broker::pid(), parent_pid));
                let attach = Inbound::Attach { id, stream: out, cwd, cols, rows, host, program, caps, inside, if_free, reopened };
                if tx.send(attach).is_err() {
                    return;
                }
            }
            // --stop/--update need neither a screen nor a folder, so they skip Hello.
            Ok(Some(ClientMsg::Shutdown)) => {
                let _ = tx.send(Inbound::Shutdown);
                return;
            }
            Ok(Some(ClientMsg::Update)) => {
                let _ = tx.send(Inbound::Update);
                return;
            }
            // The stream goes along: the main loop writes the summary and closes it.
            Ok(Some(ClientMsg::DisconnectAll)) => {
                let _ = tx.send(Inbound::DisconnectAll { id, stream: out });
                return;
            }
            Ok(Some(ClientMsg::Probe)) => {
                let _ = tx.send(Inbound::Probe { stream: out });
                return;
            }
            Ok(Some(ClientMsg::Connect { .. })) => {
                crate::plugins::refuse(&out, "a plugin connects on the plugin socket");
                return;
            }
            // Shut, not only dropped: a child spawned while the socket was not yet close-on-exec may hold a copy.
            _ => {
                let _ = out.shutdown(std::net::Shutdown::Both);
                return;
            }
        }
    }
    loop {
        match ClientMsg::read_from(&mut reader) {
            Ok(Some(ClientMsg::Event(ev))) => {
                if tx.send(Inbound::Event(id, ev)).is_err() {
                    return;
                }
            }
            // A client newer than this daemon sending an unknown message: log it, don't disconnect.
            Err(e) if e.kind() == std::io::ErrorKind::Other => {
                warn(&format!("client {id}: unrecognized message: {e}"));
            }
            // The screen has one writer, so a refusal here is only logged.
            Ok(Some(ClientMsg::Shutdown)) if refused(reader.get_ref(), "stop", allow) => {}
            Ok(Some(ClientMsg::Update)) if refused(reader.get_ref(), "update", allow) => {}
            Ok(Some(ClientMsg::Shutdown)) => {
                let _ = tx.send(Inbound::Shutdown);
                return;
            }
            Ok(Some(ClientMsg::Update)) => {
                let _ = tx.send(Inbound::Update);
            }
            Ok(Some(ClientMsg::Appearance(dark))) => {
                if tx.send(Inbound::Appearance(id, dark)).is_err() {
                    return;
                }
            }
            Ok(Some(ClientMsg::CellHeight(px))) => {
                if tx.send(Inbound::CellHeight(id, px)).is_err() {
                    return;
                }
            }
            Ok(Some(ClientMsg::Remote { name, result, window, ssh_failed })) => {
                if tx.send(Inbound::Remote(id, name, result, window, ssh_failed)).is_err() {
                    return;
                }
            }
            end @ (Ok(Some(ClientMsg::Detach)) | Ok(None) | Err(_)) => {
                let how = match end {
                    Ok(Some(_)) => "asked to detach".to_string(),
                    Ok(None) => "socket closed (EOF)".to_string(),
                    Err(e) => format!("{READ_ERROR}: {e}"),
                };
                let _ = tx.send(Inbound::Detach(id, format!("{how}; {}", peer.states())));
                return;
            }
            Ok(Some(ClientMsg::Hello { .. })) | Ok(Some(ClientMsg::DisconnectAll)) | Ok(Some(ClientMsg::Probe)) | Ok(Some(ClientMsg::Connect { .. })) | Ok(Some(ClientMsg::ReportState { .. })) => {}
        }
    }
}

/// Re-entered so the client's own ?1049l on exit restores the cursor below the line, not above it.
pub(crate) fn farewell_text(text: &str) -> String {
    format!("\x1b[?1049l\r\n{text}\r\n\x1b[?1049h")
}

/// Whether the peer runs inside one of this server's panes, with `allow_from_panes` off or the peer a plugin's; a refusal is logged.
fn refused(stream: &UnixStream, what: &str, allow: fn() -> bool) -> bool {
    let Ok(pid) = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid) else { return false };
    if !inside(pid, crate::broker::pid(), parent_pid) || let_in(allow(), crate::plugins::within_a_plugin(pid)) {
        return false;
    }
    warn(&format!("refused {what} from pid {pid}, inside a pane"));
    true
}

/// A peer inside: `allow_from_panes` lets a pane's program in, never a plugin's.
fn let_in(allow_from_panes: bool, plugin: bool) -> bool {
    allow_from_panes && !plugin
}

/// Whether `anchor` is among `pid`'s ancestors, read by `parent`, within 32 steps and short of launchd.
pub(crate) fn inside(pid: i32, anchor: i32, parent: impl Fn(i32) -> Option<i32>) -> bool {
    let mut pid = pid;
    for _ in 0..32 {
        let Some(p) = parent(pid).filter(|p| *p > 1) else { return false };
        if p == anchor {
            return true;
        }
        pid = p;
    }
    false
}

/// The peer's process name, or its `sshd*` ancestor's: a relay started over ssh is itself `nerd`.
pub(crate) fn peer_program(stream: &UnixStream) -> Option<String> {
    let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid).ok()?;
    let own = proc_name(pid)?;
    let parent = |p| {
        let pp = parent_pid(p)?;
        Some((pp, proc_name(pp)?))
    };
    Some(sshd_ancestor(pid, parent).unwrap_or(own))
}

pub(crate) fn proc_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 64];
    // SAFETY: proc_name writes up to 32 bytes into buf and refuses a buffer smaller than that.
    let n = unsafe { nix::libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// A client by its pid and the top ancestor below launchd: the terminal app, `sshd`, or `tmux`.
/// Each pid with its start time read at connect, so a pid reused by the detach reads `gone`.
pub(crate) struct Peer {
    pid: Option<i32>,
    started: Option<u64>,
    top: Option<(i32, String, Option<u64>)>,
}

impl Peer {
    pub(crate) fn of(stream: &UnixStream) -> Peer {
        let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid).ok();
        let top = pid.map(|p| top_ancestor(p, parent_pid)).filter(|t| Some(*t) != pid).map(|t| (t, proc_name(t).unwrap_or_else(|| "unknown".into()), start_time(t)));
        Peer { pid, started: pid.and_then(start_time), top }
    }

    /// `client pid 812 running, ghostty pid 700 gone`.
    fn states(&self) -> String {
        let Some(pid) = self.pid else { return "client pid unknown".into() };
        let mut s = format!("client pid {pid} {}", state_since(pid, self.started));
        if let Some((top, name, started)) = &self.top {
            s += &format!(", {name} pid {top} {}", state_since(*top, *started));
        }
        s
    }
}

/// `client pid 812 under ghostty pid 700`.
impl std::fmt::Display for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match (self.pid, &self.top) {
            (None, _) => write!(f, "client pid unknown"),
            (Some(pid), None) => write!(f, "client pid {pid}"),
            (Some(pid), Some((top, name, _))) => write!(f, "client pid {pid} under {name} pid {top}"),
        }
    }
}

/// The last ancestor before launchd, read by `parent`, within 32 steps; `pid` itself when its parent is launchd.
fn top_ancestor(pid: i32, parent: impl Fn(i32) -> Option<i32>) -> i32 {
    let mut pid = pid;
    for _ in 0..32 {
        match parent(pid) {
            Some(p) if p > 1 => pid = p,
            _ => break,
        }
    }
    pid
}

/// A process's start in microseconds since the epoch; None when it cannot be read (another account's process).
pub(crate) fn start_time(pid: i32) -> Option<u64> {
    // SAFETY: a zeroed proc_bsdinfo is a valid value, and proc_pidinfo writes at most its size.
    let mut info: nix::libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_bsdinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// `pid_state`, but `gone` for a running process that is not the one that started at `started`.
fn state_since(pid: i32, started: Option<u64>) -> &'static str {
    // Read first: a zombie has no start time, and must still read `exiting`.
    let now = start_time(pid);
    match pid_state(pid) {
        "running" if started.is_some() && now != started => "gone",
        state => state,
    }
}

/// `running`, `exiting` for a zombie nobody has reaped yet, or `gone`.
fn pid_state(pid: i32) -> &'static str {
    // SAFETY: a zeroed proc_bsdshortinfo is a valid value, and proc_pidinfo writes at most its size.
    let mut info: nix::libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_bsdshortinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size) };
    // A zombie has no info left to read, but a null signal still finds it.
    let exists = || !matches!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None), Err(nix::errno::Errno::ESRCH));
    match n == size {
        true if info.pbsi_status != nix::libc::SZOMB => "running",
        _ if exists() => "exiting",
        _ => "gone",
    }
}

/// The short form: the full one refuses a process of another user, such as root's `login` or `sudo` in the chain.
pub(crate) fn parent_pid(pid: i32) -> Option<i32> {
    // SAFETY: a zeroed proc_bsdshortinfo is a valid value, and proc_pidinfo writes at most its size.
    let mut info: nix::libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_bsdshortinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(info.pbsi_ppid as i32)
}

/// `proc_listpids` filter: the processes whose parent is `typeinfo`; nix's libc does not name it.
const PROC_PPID_ONLY: u32 = 6;

/// Every process under `root`, breadth-first, root excluded; at most `max`.
pub(crate) fn descendants(root: i32, max: usize) -> Vec<i32> {
    let mut out = Vec::new();
    let mut next = 0;
    let mut pid = root;
    loop {
        // SAFETY: a null buffer only asks for the size; the second call writes at most `bytes` into `kids`.
        let bytes = unsafe { nix::libc::proc_listpids(PROC_PPID_ONLY, pid as u32, std::ptr::null_mut(), 0) };
        if bytes > 0 {
            let mut kids = vec![0i32; bytes as usize / 4 + 16];
            let n = unsafe { nix::libc::proc_listpids(PROC_PPID_ONLY, pid as u32, kids.as_mut_ptr().cast(), (kids.len() * 4) as i32) };
            for &k in kids.iter().take(n.max(0) as usize / 4).filter(|k| **k > 0 && **k != root) {
                if out.len() == max {
                    return out;
                }
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        let Some(&p) = out.get(next) else { return out };
        pid = p;
        next += 1;
    }
}

/// The first `sshd*` ancestor's name, within 8 steps and short of launchd: `parent` gives (ppid, its name).
fn sshd_ancestor(pid: i32, parent: impl Fn(i32) -> Option<(i32, String)>) -> Option<String> {
    let mut pid = pid;
    for _ in 0..8 {
        let (ppid, name) = parent(pid)?;
        if ppid <= 1 {
            return None;
        }
        if name.starts_with("sshd") {
            return Some(name);
        }
        pid = ppid;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::time::{Duration, Instant};

    #[test]
    fn descendants_finds_a_grandchild() {
        let mut sh = std::process::Command::new("/bin/sh").args(["-c", "sleep 5 & wait"]).spawn().unwrap();
        let root = sh.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        let (found, names) = loop {
            let d = super::descendants(root, 16);
            let names: Vec<String> = d.iter().filter_map(|p| super::proc_name(*p)).collect();
            if names.iter().any(|n| n == "sleep") || Instant::now() > deadline {
                break (d, names);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        for p in &found {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(*p), nix::sys::signal::Signal::SIGKILL);
        }
        let _ = sh.kill();
        let _ = sh.wait();
        assert!(names.iter().any(|n| n == "sleep"), "{found:?} {names:?}");
        assert!(!found.contains(&root), "the root is not its own descendant");
    }

    #[test]
    fn the_top_ancestor_is_the_last_one_short_of_launchd() {
        // 40 under tmux 30 under launchd; 5 is itself a child of launchd; 9 cannot be read.
        let parent = |p| match p {
            40 => Some(35),
            35 => Some(30),
            30 => Some(1),
            5 => Some(1),
            _ => None,
        };
        assert_eq!(super::top_ancestor(40, parent), 30);
        assert_eq!(super::top_ancestor(5, parent), 5);
        assert_eq!(super::top_ancestor(9, parent), 9);
        assert_eq!(super::top_ancestor(2, |p| Some(p + 1)), 34, "a loop stops after 32 steps");
    }

    #[test]
    fn a_peer_is_named_with_its_top_ancestor_and_their_states() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = super::Peer::of(&a);
        let me = std::process::id();
        assert!(peer.to_string().starts_with(&format!("client pid {me}")), "{peer}");
        assert!(peer.states().starts_with(&format!("client pid {me} running")), "{}", peer.states());
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id() as i32;
        // Waits for the exit and leaves the zombie in place.
        let mut info: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { nix::libc::waitid(nix::libc::P_PID, pid as u32, &mut info, nix::libc::WEXITED | nix::libc::WNOWAIT) }, 0);
        assert_eq!(super::pid_state(pid), "exiting", "not reaped yet");
        assert_eq!(super::state_since(pid, Some(1)), "exiting", "a zombie is not mistaken for a reused pid");
        child.wait().unwrap();
        assert_eq!(super::pid_state(pid), "gone");
    }

    #[test]
    fn a_client_that_closed_its_end_reads_hung_up_before_its_reader_says_so() {
        let sink = super::Sink::new(crate::bell::new().0);
        assert!(!sink.hung_up(), "nobody attached");
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        sink.set(Some(ours));
        theirs.write_all(b"an event not read yet").unwrap();
        assert!(!sink.hung_up());
        drop(theirs);
        assert!(sink.hung_up());
    }

    #[test]
    fn a_pid_reused_since_the_connect_reads_gone() {
        let me = std::process::id() as i32;
        let started = super::start_time(me);
        let peer = super::Peer { pid: Some(me), started, top: Some((me, "ghostty".into(), Some(1))) };
        assert_eq!(peer.states(), format!("client pid {me} running, ghostty pid {me} gone"));
        assert_eq!(super::state_since(me, None), "running", "a start time that could not be read is not compared");
    }

    #[test]
    fn only_a_peer_of_the_same_user_is_let_in() {
        assert!(super::same_user(Some(501), 501));
        assert!(!super::same_user(Some(502), 501));
        assert!(!super::same_user(Some(0), 501), "not even root");
        assert!(!super::same_user(None, 501), "a peer whose uid cannot be read");
    }

    #[test]
    fn a_client_connects_to_a_server_of_its_own_user() {
        let path = std::env::temp_dir().join(format!("nerd-own-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(super::connect_own(&path).is_ok());
        let e = super::refuse_stranger(Some(502), 501, &path).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("belongs to uid 502, not to you"), "{e}");
        assert!(super::refuse_stranger(None, 501, &path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_writer_never_blocks_the_sender_and_ends_only_when_the_peer_is_gone() {
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        // As a socket carried over an update from a build that set a timeout.
        ours.set_write_timeout(Some(Duration::from_millis(1))).unwrap();
        let (bell, door) = crate::bell::new();
        let w = super::Writer::spawn(ours, bell).unwrap();
        let t = Instant::now();
        for _ in 0..16 {
            w.send(vec![b'x'; 64 << 10]).unwrap();
        }
        assert!(t.elapsed() < Duration::from_millis(100), "send waited on the socket: {:?}", t.elapsed());
        assert!(w.backlog() > 0, "a peer that never reads leaves a backlog");
        std::thread::sleep(Duration::from_millis(200));
        assert!(w.send(b"y".to_vec()).is_ok(), "the old build's timeout ended the writer");
        w.send(Vec::new()).unwrap();

        door.wait(Duration::ZERO);
        theirs.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let (mut got, mut buf) = (0, vec![0u8; 64 << 10]);
        while got < (16 << 16) + 1 {
            got += theirs.read(&mut buf).unwrap();
        }
        let t = Instant::now();
        door.wait(Duration::from_secs(2));
        assert!(t.elapsed() < Duration::from_secs(1), "no ring when the backlog reached zero");
        assert_eq!(w.backlog(), 0);

        drop(theirs);
        // The first write after the peer left can still succeed, so zero alone does not say the writer ended.
        let t = Instant::now();
        let e = loop {
            assert!(t.elapsed() < Duration::from_secs(5), "the writer never ended");
            if let Err(e) = w.send(b"z".to_vec()) {
                break e;
            }
            let t = Instant::now();
            while w.backlog() > 0 && t.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(w.backlog(), 0, "a dead writer leaves no backlog to hold frames back, before any later send");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_retired_writer_stuck_on_a_client_that_never_reads_ends_within_its_grace() {
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let w = super::Writer::spawn(ours, crate::bell::new().0).unwrap();
        w.send(vec![b'x'; 1 << 20]).unwrap();
        let left = w.backlog.clone();
        let t = Instant::now();
        w.retire();
        assert!(t.elapsed() < Duration::from_millis(50), "retire waited for the client");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(left.load(std::sync::atomic::Ordering::SeqCst) > 0, "cut before its grace");
        std::thread::sleep(Duration::from_millis(1000));
        assert_eq!(left.load(std::sync::atomic::Ordering::SeqCst), 0, "the shutdown did not free the blocked writer");
        // macOS refuses the option on a socket already shut down; the read then ends at once.
        let _ = theirs.set_read_timeout(Some(Duration::from_secs(1)));
        let mut rest = Vec::new();
        theirs.read_to_end(&mut rest).unwrap();
        assert!(rest.len() < 1 << 20, "the client sees EOF after what reached it");
    }

    #[test]
    fn the_peer_program_is_the_process_on_the_other_end() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy();
        let got = super::peer_program(&a).unwrap();
        // proc_name gives the kernel's short name, cut at 32 bytes.
        assert_eq!(got, name[..name.len().min(32)]);
    }

    #[test]
    fn an_sshd_ancestor_makes_the_client_one_over_ssh() {
        let chain = |links: &[(i32, i32, &str)]| {
            let map: std::collections::HashMap<i32, (i32, String)> =
                links.iter().map(|(pid, ppid, name)| (*pid, (*ppid, name.to_string()))).collect();
            move |pid: i32| map.get(&pid).cloned()
        };
        // relay 50 -> sh 40 -> sshd-session 30 -> sshd 20 -> launchd
        let ssh = chain(&[(50, 40, "sh"), (40, 30, "sshd-session"), (30, 20, "sshd"), (20, 1, "launchd")]);
        assert_eq!(super::sshd_ancestor(50, ssh).as_deref(), Some("sshd-session"));
        let local = chain(&[(50, 40, "zsh"), (40, 30, "login"), (30, 1, "launchd")]);
        assert_eq!(super::sshd_ancestor(50, local), None);
        let long: Vec<(i32, i32, &str)> = (0..20).map(|i| (100 - i, 99 - i, if i == 12 { "sshd" } else { "sh" })).collect();
        assert_eq!(super::sshd_ancestor(100, chain(&long)), None, "at most 8 steps up");
        let cycle = chain(&[(5, 6, "sh"), (6, 5, "sh")]);
        assert_eq!(super::sshd_ancestor(5, cycle), None, "a loop ends at the cap");
        let me = std::process::id() as i32;
        assert_eq!(super::parent_pid(me), Some(nix::unistd::getppid().as_raw()), "the walk reads real parents");
        assert_eq!(super::parent_pid(1), Some(0), "a root process is read too, as sudo or login in a pane's chain would be");
    }

    #[test]
    fn a_peer_is_inside_when_the_broker_is_among_its_ancestors() {
        let chain = |links: &[(i32, i32)]| {
            let map: std::collections::HashMap<i32, i32> = links.iter().copied().collect();
            move |pid: i32| map.get(&pid).copied()
        };
        // S1: tool shell 73876 -> claude 11908 -> zsh 9663 -> broker 9619 -> daemon 9618 -> launchd
        let pane = chain(&[(73876, 11908), (11908, 9663), (9663, 9619), (9619, 9618), (9618, 1)]);
        assert!(super::inside(73876, 9619, &pane), "an agent's tool shell");
        assert!(super::inside(9663, 9619, &pane), "the pane's shell");
        assert!(super::inside(73876, 9618, &pane), "a handover from a build that kept no broker pid falls back to the daemon's");
        assert!(!super::inside(9619, 9619, &pane), "the broker itself is not a client");
        // After an in-place update the daemon keeps its pid and the broker is adopted, so the carried pid still matches.
        let after = chain(&[(73876, 11908), (11908, 9663), (9663, 9619), (9619, 9618)]);
        assert!(super::inside(73876, 9619, after));
        let terminal = chain(&[(502, 501), (501, 500), (500, 1)]);
        assert!(!super::inside(502, 9619, &terminal), "a terminal's shell");
        assert!(!super::inside(502, 1, &terminal), "launchd is never an anchor");
        let detached = chain(&[(800, 1)]);
        assert!(!super::inside(800, 9619, detached), "a double fork escapes: the stated limit");
        let gone = chain(&[(900, 901)]);
        assert!(!super::inside(900, 9619, gone), "an ancestor that cannot be read ends the walk");
        let cycle = chain(&[(5, 6), (6, 5)]);
        assert!(!super::inside(5, 9619, cycle), "a loop ends at the cap");
        let long: Vec<(i32, i32)> = (0..40).map(|i| (1000 + i, 1001 + i)).collect();
        assert!(super::inside(1000, 1032, chain(&long)), "32 steps up");
        assert!(!super::inside(1000, 1033, chain(&long)), "not 33");
        assert_eq!(crate::broker::pid(), std::process::id() as i32, "with no broker the daemon's own pid is the anchor");
    }

    /// A child process connects, as a program in a pane does: its parent is this test, the anchor with no broker.
    fn from_a_child(msgs: &str, greeted: bool, allow: fn() -> bool) -> (String, Vec<super::Inbound>) {
        from_a_child_that(msgs, greeted, allow, |_| {})
    }

    /// `launched` gets the child's pid before its message is read, as the broker's answer would come.
    fn from_a_child_that(msgs: &str, greeted: bool, allow: fn() -> bool, launched: impl FnOnce(i32)) -> (String, Vec<super::Inbound>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("nerd-inside-{}-{n}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut nc = std::process::Command::new("/usr/bin/nc")
            .arg("-U")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        launched(nc.id() as i32);
        std::io::Write::write_all(nc.stdin.as_mut().unwrap(), msgs.as_bytes()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
        std::thread::spawn(move || super::read_client(1, stream, inbox, greeted, super::Waiting::default(), allow));
        let mut got = Vec::new();
        while let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) {
            got.push(m);
        }
        // nc ends by itself once the server closes; killed earlier, it may not have copied the line yet.
        let t = Instant::now();
        while nc.try_wait().unwrap().is_none() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = nc.kill();
        let mut out = String::new();
        let _ = nc.stdout.take().unwrap().read_to_string(&mut out);
        let _ = nc.wait();
        let _ = std::fs::remove_file(&path);
        (out, got)
    }

    fn acted(got: &[super::Inbound]) -> bool {
        got.iter().any(|m| matches!(m, super::Inbound::Shutdown | super::Inbound::Update | super::Inbound::DisconnectAll { .. } | super::Inbound::Attach { .. }))
    }

    #[test]
    fn a_program_inside_a_pane_is_refused_and_told_so() {
        for (msg, what) in [("\"Shutdown\"", "stop"), ("\"Update\"", "update"), ("\"DisconnectAll\"", "disconnect")] {
            let (out, got) = from_a_child(&format!("{msg}\n"), false, || false);
            assert_eq!(out, format!("refused: {what} from inside a pane\n"));
            assert!(!acted(&got), "{what} went through");
        }
        let (out, got) = from_a_child("{\"Hello\":{\"cwd\":\"/\",\"cols\":80,\"rows\":24}}\n", false, || false);
        assert_eq!(out, "\x1b[?1049l\r\nrefused: attach from inside a pane\r\n\x1b[?1049h", "on the screen the human keeps");
        assert!(!acted(&got), "the screen was taken");
    }

    #[test]
    fn after_hello_a_program_inside_a_pane_still_cannot_stop_or_update() {
        let (out, got) = from_a_child("\"Update\"\n\"Shutdown\"\n", true, || false);
        assert!(!acted(&got), "went through after Hello");
        assert_eq!(out, "", "the screen stream is not written to from here");
    }

    #[test]
    fn allow_from_panes_lets_a_program_inside_a_pane_through() {
        let (out, got) = from_a_child("\"Shutdown\"\n", false, || true);
        assert!(matches!(got.as_slice(), [super::Inbound::Shutdown]), "{out}");
        assert_eq!(out, "");
        let (_, got) = from_a_child("\"Update\"\n", true, || true);
        assert!(matches!(got.first(), Some(super::Inbound::Update)), "after Hello");
    }

    #[test]
    fn inside_client_is_a_window_attached_from_a_pane() {
        let hello = "{\"Hello\":{\"cwd\":\"/\",\"cols\":80,\"rows\":24}}\n";
        let (_, got) = from_a_child(hello, false, || true);
        assert!(matches!(got.as_slice(), [super::Inbound::Attach { inside: true, .. }]), "from a child of the anchor");
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
        std::io::Write::write_all(&mut theirs, hello.as_bytes()).unwrap();
        std::thread::spawn(move || super::read_client(1, ours, inbox, false, super::Waiting::default(), || false));
        let m = rx.recv_timeout(Duration::from_secs(2)).expect("no attach");
        assert!(matches!(m, super::Inbound::Attach { inside: false, .. }), "a terminal outside the panes");
    }

    #[test]
    fn allow_from_panes_never_lets_a_plugin_in() {
        assert!(super::let_in(true, false));
        assert!(!super::let_in(true, true));
        assert!(!super::let_in(false, false));
    }

    #[test]
    fn a_v1_daemon_cannot_read_connect_so_nothing_attaches() {
        // The v1 message set, as a daemon before protocol 2 parses it.
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        enum V1 {
            Hello { cwd: std::path::PathBuf, cols: u16, rows: u16 },
            Detach,
            Shutdown,
            Update,
            DisconnectAll,
        }
        let msg = crate::proto::ClientMsg::Connect { protocol: 2, role: "plugin".into(), plugin: Some("x".into()), events: vec![] };
        let line = serde_json::to_string(&msg).unwrap();
        assert!(serde_json::from_str::<V1>(&line).is_err());
        // What v1's reader does with a first message it cannot read: close, attach nothing.
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
        std::io::Write::write_all(&mut theirs, line.replace("Connect", "ConnectFromTheFuture").as_bytes()).unwrap();
        std::io::Write::write_all(&mut theirs, b"\n").unwrap();
        std::thread::spawn(move || super::read_client(1, ours, inbox, false, super::Waiting::default(), || false));
        let mut rest = Vec::new();
        theirs.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        theirs.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty() && rx.try_recv().is_err(), "a client that fails closed sees EOF and nothing else");
    }

    #[test]
    fn a_client_nerd_cannot_read_sees_eof_while_another_copy_of_its_socket_lives() {
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        // What a child spawned between socketpair and close-on-exec would hold.
        let _leaked = ours.try_clone().unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
        std::io::Write::write_all(&mut theirs, b"{\"FromTheFuture\":1}\n").unwrap();
        std::thread::spawn(move || super::read_client(1, ours, inbox, false, super::Waiting::default(), || false));
        let mut rest = Vec::new();
        theirs.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        theirs.read_to_end(&mut rest).expect("EOF, though a copy is open");
        assert!(rest.is_empty());
    }

    #[test]
    fn allow_from_panes_does_not_let_a_plugins_process_stop_or_attach() {
        let exec = 9_002;
        for (msg, what, greeted) in [("\"Shutdown\"\n", "stop", false), ("{\"Hello\":{\"cwd\":\"/\",\"cols\":80,\"rows\":24}}\n", "attach", false), ("\"Update\"\n", "update", true)] {
            let (out, got) = from_a_child_that(msg, greeted, || true, |pid| crate::plugins::launched_for_test(exec, "allow-test", pid));
            crate::plugins::exited(exec);
            assert!(!acted(&got), "{what} went through");
            assert!(greeted || out.contains(&format!("refused: {what} from inside a pane")), "{what}: {out:?}");
        }
    }

    #[test]
    fn a_peer_outside_the_panes_is_obeyed() {
        for (first, greeted) in [("\"Shutdown\"\n", false), ("\"Update\"\n", false), ("\"Update\"\n\"Shutdown\"\n", true)] {
            let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
            std::io::Write::write_all(&mut theirs, first.as_bytes()).unwrap();
            std::thread::spawn(move || super::read_client(1, ours, inbox, greeted, super::Waiting::default(), || false));
            let m = rx.recv_timeout(Duration::from_secs(2)).expect("nothing came through");
            assert!(matches!(m, super::Inbound::Shutdown | super::Inbound::Update), "{first}");
            let mut rest = Vec::new();
            // macOS refuses the option on a socket already shut down; the read then ends at once.
            let _ = theirs.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = theirs.read_to_end(&mut rest);
            assert!(!String::from_utf8_lossy(&rest).contains("refused"), "{first}");
        }
    }
}
