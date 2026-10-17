//! A pane on the daemon's side: a pseudo-terminal master descriptor. The process itself is
//! launched by the broker (`broker.rs`), since Seatbelt's network ban is inherited by children.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::Instant;

use nix::fcntl::{fcntl, FcntlArg, FdFlag};

use crate::bell::Bell;

/// Chunks of output a pane may have waiting; a full queue stalls its reader, which throttles the program.
pub const QUEUE: usize = 64;

/// Writes a pane may have waiting; past that its program is not reading, and more are dropped.
pub const WRITES: usize = 256;

pub struct Pty {
    pub id: usize,
    /// None until the broker answers.
    master: Option<std::fs::File>,
    pub cols: u16,
    pub rows: u16,
    pub child_pid: Option<u32>,
    output: Receiver<Vec<u8>>,
    /// The reader has ended and every chunk it read was taken.
    pub ended: bool,
    input: SyncSender<Vec<u8>>,
    /// A key or a paste was dropped: the program is not reading its input.
    pub stalled: bool,
    replies_dropped: bool,
    starting: Option<Starting>,
    /// Started through `nerd --pane-exec`, maybe under sudo: the shell is further down. A handover carries it.
    pub wrapped: bool,
}

/// A pane the broker has not answered for yet: its threads' channel ends wait for the master.
struct Starting {
    output: SyncSender<Vec<u8>>,
    input: Receiver<Vec<u8>>,
    bell: Bell,
    until: Instant,
    exited: bool,
}

/// The reader drops its end when it stops, which is how the loop learns the pane's output is over.
fn pump(id: usize, mut reader: std::fs::File, tx: SyncSender<Vec<u8>>, bell: Bell) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Err(e) => {
                    // EIO on the master after the shell dies is the same end as EOF.
                    if e.raw_os_error() != Some(nix::libc::EIO) {
                        crate::log::error(&format!("pane {id}: reading it failed: {e}"));
                    }
                    break;
                }
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                    bell.ring();
                }
            }
        }
        drop(tx);
        bell.ring();
    });
}

/// Its own copy of the master: a program that stops reading blocks this thread, never the loop.
fn writer(id: usize, mut master: std::fs::File, rx: Receiver<Vec<u8>>) {
    std::thread::spawn(move || {
        for bytes in rx {
            if let Err(e) = master.write_all(&bytes) {
                // EIO once the program is gone is the normal end.
                if e.raw_os_error() != Some(nix::libc::EIO) {
                    crate::log::error(&format!("pane {id}: writing to it failed: {e}"));
                }
                break;
            }
        }
    });
}

/// The reader and writer on their own copies of the master.
fn start(id: usize, master: &std::fs::File, output: SyncSender<Vec<u8>>, input: Receiver<Vec<u8>>, bell: Bell) -> std::io::Result<()> {
    let (reader, w) = (master.try_clone()?, master.try_clone()?);
    pump(id, reader, output, bell);
    writer(id, w, input);
    Ok(())
}

/// Drop asks the broker to close the master (SIGHUP to the shell) and reap stragglers after a quarter second.
impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(b) = crate::broker::handle() {
            b.kill(self.id);
        }
    }
}

impl Pty {
    pub fn waiting(id: usize, cols: u16, rows: u16, bell: Bell) -> Self {
        let (out_tx, output) = sync_channel(QUEUE);
        let (input, in_rx) = sync_channel(WRITES);
        let until = Instant::now() + crate::broker::REPLY_TIMEOUT;
        let starting = Some(Starting { output: out_tx, input: in_rx, bell, until, exited: false });
        Self { id, master: None, cols, rows, child_pid: None, output, ended: false, input, stalled: false, replies_dropped: false, starting, wrapped: false }
    }

    /// Asks the broker for the process; the pane takes input at once and gets its master in `attach`.
    pub fn spawn(
        id: usize,
        argv: &[String],
        cwd: &std::path::Path,
        cols: u16,
        rows: u16,
        bell: Bell,
        iso: &crate::policy::Isolation,
    ) -> std::io::Result<Self> {
        let env = vec![
            // Without TERM, Claude Code falls back to a degraded mode.
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("TERM_PROGRAM".to_string(), "nerd".to_string()),
            // The hook reads these to know it's inside nerd and which pane; without them it exits silently.
            ("NERD_PANE".to_string(), id.to_string()),
            ("NERD_STATE".to_string(), crate::hook::state_dir().to_string_lossy().into_owned()),
            ("NERD_MSOCK".to_string(), crate::session::msock_path().to_string_lossy().into_owned()),
            ("NERD_MSOCK_UID".to_string(), crate::policy::owner_uid().to_string()),
        ];
        let mut env = env;
        env.extend(utf8_ctype(|k| std::env::var(k).ok()));
        let argv = crate::isolation::wrap(id, argv, cwd, iso, &mut env)?;
        let Some(broker) = crate::broker::handle() else {
            return Err(std::io::Error::other("broker not running"));
        };
        broker.spawn(id, &argv, cwd, cols, rows, env)?;
        let mut pty = Self::waiting(id, cols, rows, bell);
        pty.wrapped = !iso.is_open();
        Ok(pty)
    }

    /// The broker's master arrived: what was typed meanwhile goes out first. True when an exit notice waited for it.
    pub fn attach(&mut self, fd: OwnedFd, child_pid: Option<u32>) -> std::io::Result<bool> {
        let Some(s) = self.starting.take() else { return Err(std::io::Error::other("already attached")) };
        let master = std::fs::File::from(fd);
        // `Exited` comes from the broker (it waits on the process), not from EOF here:
        // a background job (`sleep 300 &`) can keep the slave held open past shell exit.
        start(self.id, &master, s.output, s.input, s.bell)?;
        self.master = Some(master);
        self.child_pid = child_pid;
        Ok(s.exited)
    }

    /// The pane's shell: the wrapper's only child for "only these providers", below sudo for another account.
    pub fn shell_pid(&self) -> Option<u32> {
        let mut pid = self.child_pid? as i32;
        // sudo may put a monitor of its own between itself and the wrapper: a few hops, not an exact count.
        for _ in 0..6 {
            if !self.wrapped || !hop(pid) {
                break;
            }
            let kids: Vec<i32> = crate::transport::descendants(pid, 16).into_iter().filter(|k| crate::transport::parent_pid(*k) == Some(pid)).collect();
            let [only] = kids[..] else { break };
            pid = only;
        }
        Some(pid as u32)
    }

    pub fn starting(&self) -> bool {
        self.starting.is_some()
    }

    /// When a pane still starting counts as unanswered.
    pub fn until(&self) -> Option<Instant> {
        self.starting.as_ref().map(|s| s.until)
    }

    /// The broker answered something, so it is alive: the wait starts over.
    pub fn heard(&mut self) {
        if let Some(s) = self.starting.as_mut() {
            s.until = Instant::now() + crate::broker::REPLY_TIMEOUT;
        }
    }

    #[cfg(test)]
    pub fn overdue(&mut self) {
        if let Some(s) = self.starting.as_mut() {
            s.until = Instant::now();
        }
    }

    /// An exit notice before the pid is known is decided at attach; false once attached.
    pub fn hold_exit(&mut self) -> bool {
        self.starting.as_mut().map(|s| s.exited = true).is_some()
    }

    /// Clears close-on-exec so the next daemon build can adopt this fd across its exec.
    pub fn export_fd(&self) -> std::io::Result<RawFd> {
        let fd = self.master.as_ref().ok_or_else(|| std::io::Error::other("still starting"))?.as_raw_fd();
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty()))?;
        Ok(fd)
    }

    pub fn adopt(
        id: usize,
        fd: RawFd,
        child_pid: Option<u32>,
        cols: u16,
        rows: u16,
        bell: Bell,
    ) -> std::io::Result<Self> {
        // Restore close-on-exec: only a handover should pass this fd on.
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let master = unsafe { std::fs::File::from_raw_fd(fd) };
        let (out_tx, output) = sync_channel(QUEUE);
        let (input, in_rx) = sync_channel(WRITES);
        start(id, &master, out_tx, in_rx, bell)?;
        Ok(Self { id, master: Some(master), cols, rows, child_pid, output, ended: false, input, stalled: false, replies_dropped: false, starting: None, wrapped: false })
    }

    /// A pane in a test: `master` takes what is written, and the sender plays the reader.
    #[cfg(test)]
    pub fn fake(id: usize, master: std::fs::File) -> (Self, std::sync::mpsc::SyncSender<Vec<u8>>) {
        let (tx, output) = sync_channel(QUEUE);
        let (input, in_rx) = sync_channel(WRITES);
        writer(id, master.try_clone().unwrap(), in_rx);
        (Self { id, master: Some(master), cols: 80, rows: 24, child_pid: None, output, ended: false, input, stalled: false, replies_dropped: false, starting: None, wrapped: false }, tx)
    }

    /// The next chunk of output, if one is waiting.
    pub fn next(&mut self) -> Option<Vec<u8>> {
        match self.output.try_recv() {
            Ok(bytes) => Some(bytes),
            Err(TryRecvError::Disconnected) => {
                self.ended = true;
                None
            }
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Keys and pastes, in order with the replies.
    pub fn write(&mut self, bytes: &[u8]) {
        if !self.queue(bytes) {
            self.stalled = true;
        }
    }

    /// Answers to the program's own queries; a program that asks and never reads loses them.
    pub fn reply(&mut self, bytes: &[u8]) {
        if !self.queue(bytes) && !std::mem::replace(&mut self.replies_dropped, true) {
            crate::log::warn(&format!("pane {}: not reading its input, replies dropped", self.id));
        }
    }

    fn queue(&self, bytes: &[u8]) -> bool {
        !matches!(self.input.try_send(bytes.to_vec()), Err(TrySendError::Full(_)))
    }

    /// TIOCSWINSZ is called by the broker, since it holds the master.
    /// The process group the terminal now serves: the shell's own, or the job it runs in front.
    pub fn foreground(&self) -> Option<i32> {
        nix::unistd::tcgetpgrp(self.master.as_ref()?).ok().map(|p| p.as_raw())
    }

    /// The terminal's foreground group is not the shell's and runs `agent`, named as detect names it; checked now,
    /// so a pager, an editor or a build in front, or the shell an agent just handed the terminal back to, is not it.
    pub fn agent_in_front(&self, agent: &str) -> bool {
        let (Some(front), Some(shell)) = (self.foreground(), self.shell_pid()) else { return false };
        front != shell as i32
            && group(front).into_iter().any(|pid| {
                // another account's process keeps its arguments from us: its short name is all there is
                let argv = args(pid).filter(|a| !a.is_empty()).or_else(|| comm(pid).map(|c| vec![c]));
                argv.and_then(|a| crate::detect::identify(&a[0], &a)).as_deref() == Some(agent)
            })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        if let Some(b) = crate::broker::handle() {
            b.resize(self.id, cols, rows);
        }
    }
}

/// sudo, or `nerd --pane-exec`: a link between a wrapped pane's broker and its shell. Another account's wrapper,
/// whose arguments this user cannot read, is known by its name.
fn hop(pid: i32) -> bool {
    is_hop(comm(pid).as_deref(), || args(pid))
}

fn is_hop(name: Option<&str>, args: impl FnOnce() -> Option<Vec<String>>) -> bool {
    match name {
        Some("sudo") => true,
        Some("nerd") => args().is_none_or(|a| a.get(1).is_some_and(|f| f == "--pane-exec")),
        _ => false,
    }
}

/// A process's arguments from the kernel (KERN_PROCARGS2); None for another user's.
fn args(pid: i32) -> Option<Vec<String>> {
    use nix::libc::{CTL_KERN, KERN_PROCARGS2, sysctl};
    let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
    let mut size = 0usize;
    // SAFETY: a null buffer asks for the size; then the buffer is as long as the size passed.
    if unsafe { sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe { sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
        return None;
    }
    buf.truncate(size);
    // argc, the executable's path, NUL padding, then argc arguments.
    let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?).max(0) as usize;
    let mut words = buf[4..].split(|b| *b == 0).filter(|w| !w.is_empty()).skip(1);
    Some((0..argc).map_while(|_| words.next()).map(|w| String::from_utf8_lossy(w).into_owned()).collect())
}

/// The processes of group `pgid`; `PROC_PGRP_ONLY` from libproc.h, which nix's libc does not name.
fn group(pgid: i32) -> Vec<i32> {
    const PROC_PGRP_ONLY: u32 = 2;
    // SAFETY: a null buffer only asks for the size; the second call writes at most the buffer's length.
    let bytes = unsafe { nix::libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; bytes as usize / 4 + 16];
    let n = unsafe { nix::libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    pids.truncate(n.max(0) as usize / 4);
    pids.retain(|p| *p > 0);
    pids
}

/// A process's short name; unlike `proc_name`, also another user's, such as root's sudo.
fn comm(pid: i32) -> Option<String> {
    // SAFETY: a zeroed proc_bsdshortinfo is a valid value, and proc_pidinfo writes at most its size.
    let mut info: nix::libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_bsdshortinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then(|| info.pbsi_comm.iter().take_while(|c| **c != 0).map(|c| *c as u8 as char).collect())
}

/// What makes the pane's character type UTF-8 when the inherited locale does not: in a single-byte locale,
/// bash's readline reads byte 0x85 as M-C-e (shell-expand-line), which runs `$(…)` inside a typed quoted path.
fn utf8_ctype(var: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let set = |k: &str| var(k).filter(|v| !v.is_empty());
    let current = set("LC_ALL").or_else(|| set("LC_CTYPE")).or_else(|| set("LANG")).unwrap_or_default().to_ascii_lowercase();
    if current.contains("utf-8") || current.contains("utf8") {
        return Vec::new();
    }
    let mut env = vec![("LC_CTYPE".to_string(), "UTF-8".to_string())];
    // An empty LC_ALL counts as unset, and a set one would override LC_CTYPE.
    if set("LC_ALL").is_some() {
        env.push(("LC_ALL".to_string(), String::new()));
    }
    env
}

#[cfg(test)]
mod locale_tests {
    use super::utf8_ctype;

    fn with<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn a_pane_gets_a_utf8_ctype_only_when_it_has_none() {
        let ctype = vec![("LC_CTYPE".to_string(), "UTF-8".to_string())];
        assert_eq!(utf8_ctype(with(&[])), ctype, "no locale at all");
        assert_eq!(utf8_ctype(with(&[("LANG", "C")])), ctype);
        assert_eq!(utf8_ctype(with(&[("LANG", ""), ("LC_CTYPE", "")])), ctype, "empty counts as unset");
        assert!(utf8_ctype(with(&[("LANG", "en_US.UTF-8")])).is_empty(), "the user's own UTF-8 locale stays");
        assert!(utf8_ctype(with(&[("LANG", "C"), ("LC_CTYPE", "ru_RU.utf8")])).is_empty());
        assert!(utf8_ctype(with(&[("LC_ALL", "de_DE.UTF-8"), ("LANG", "C")])).is_empty());
        assert_eq!(utf8_ctype(with(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")])), [ctype[0].clone(), ("LC_ALL".to_string(), String::new())], "a single-byte LC_ALL would win");
    }
}

#[cfg(test)]
mod shell_tests {
    use super::{args, comm, is_hop};

    #[test]
    fn the_walk_to_the_shell_passes_sudo_and_the_pane_wrapper_only() {
        let words = |w: &[&str]| Some(w.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(is_hop(Some("sudo"), || None));
        assert!(is_hop(Some("nerd"), || words(&["/tmp/nerd-501/nerd", "--pane-exec", "--pane", "3"])));
        assert!(is_hop(Some("nerd"), || None), "another account's wrapper, whose arguments are not readable");
        assert!(!is_hop(Some("nerd"), || words(&["/Users/u/.local/bin/nerd", "--resume", "1"])), "a nerd client in a pane outside tmux");
        assert!(!is_hop(Some("zsh"), || words(&["-zsh"])));
        assert!(!is_hop(None, || None));
        let mut child = std::process::Command::new("/bin/sleep").args(["30"]).spawn().unwrap();
        let pid = child.id() as i32;
        let got = (0..100).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            args(pid).filter(|a| a.len() == 2)
        });
        assert_eq!(got, words(&["/bin/sleep", "30"]), "the kernel's argv of a process of this user");
        assert_eq!(comm(pid).as_deref(), Some("sleep"));
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod starting_tests {
    use super::Pty;
    use std::io::Read;

    #[test]
    fn a_starting_pane_keeps_its_input_and_sends_it_in_order_once_attached() {
        let mut p = Pty::waiting(7, 80, 24, crate::bell::new().0);
        p.write(b"one ");
        p.reply(b"two ");
        p.write(b"three");
        assert_eq!(p.next(), None, "no output before a master");
        assert!(!p.ended && p.starting(), "starting is not ended");
        assert!(p.export_fd().is_err(), "nothing to hand over yet");
        assert!(p.hold_exit(), "an exit notice waits for the pid");
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(p.attach(ours.into(), Some(42)).unwrap(), "the held exit is handed back");
        assert!(!p.starting() && p.child_pid == Some(42));
        assert!(!p.hold_exit(), "attached, a notice is decided at once");
        p.write(b"!");
        theirs.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        while got.len() < 14 {
            let n = theirs.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, b"one two three!");
        assert!(p.attach(std::fs::File::open("/dev/null").unwrap().into(), None).is_err(), "attached once");
    }
}
