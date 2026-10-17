//! SIGTERM and SIGHUP, recorded with their sender: the server stops on them, a client leaves on them.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

/// The signal that asked us to stop, or 0. Reboot, logout and `launchctl
/// unload` send SIGTERM; the handler only sets this, the loop does the rest.
static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Who sent the stop signal: macOS records a signal's sender nowhere, so the handler looks it up before the sender can exit.
static CLAIMED: AtomicBool = AtomicBool::new(false);
static SENDER_PID: AtomicI32 = AtomicI32::new(0);
static SENDER_UID: AtomicU32 = AtomicU32::new(0);
static PARENT_PID: AtomicI32 = AtomicI32::new(0);
static SENDER_PATH: PathSlot = PathSlot::new();
static PARENT_PATH: PathSlot = PathSlot::new();

const PATH_MAX: usize = nix::libc::PROC_PIDPATHINFO_MAXSIZE as usize;

/// A process path written once by the handler; `len` is its length, or minus the errno of the lookup.
struct PathSlot {
    buf: std::cell::UnsafeCell<[u8; PATH_MAX]>,
    len: AtomicI32,
}

// SAFETY: only the handler that wins CLAIMED writes `buf`, and readers wait for STOP_SIGNAL (or own a local slot).
unsafe impl Sync for PathSlot {}

impl PathSlot {
    const fn new() -> PathSlot {
        PathSlot { buf: std::cell::UnsafeCell::new([0; PATH_MAX]), len: AtomicI32::new(0) }
    }

    /// Signal-safe: one syscall into the static buffer, no allocation.
    fn look_up(&self, pid: i32) {
        // SAFETY: the buffer is PATH_MAX bytes and nobody reads it until `len` is published.
        let n = unsafe { nix::libc::proc_pidpath(pid, self.buf.get().cast(), PATH_MAX as u32) };
        self.len.store(if n > 0 { n } else { -nix::errno::Errno::last_raw() }, Ordering::SeqCst);
    }

    fn text(&self) -> String {
        match self.len.load(Ordering::SeqCst) {
            // SAFETY: `len` is published after the buffer was written, and it is not written again.
            n if n > 0 => String::from_utf8_lossy(unsafe { &(&*self.buf.get())[..n as usize] }).into_owned(),
            n if -n == nix::libc::ESRCH => "gone".into(),
            _ => "unknown".into(),
        }
    }
}

/// Signal-safe: the parent pid, or 0 when the process cannot be read.
fn parent_of(pid: i32) -> i32 {
    // SAFETY: a plain struct filled by the kernel; the size passed is its own.
    let mut info: nix::libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_bsdshortinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size) };
    if n == size { info.pbsi_ppid as i32 } else { 0 }
}

extern "C" fn on_stop_signal(sig: libc_int, info: *mut nix::libc::siginfo_t, _: *mut nix::libc::c_void) {
    // A second signal keeps the first sender: it is the one that stops us.
    if CLAIMED.swap(true, Ordering::SeqCst) {
        return;
    }
    // The lookups set errno, and the interrupted code may be about to read its own.
    let errno = nix::errno::Errno::last_raw();
    if !info.is_null() {
        // SAFETY: the kernel hands a valid siginfo to an SA_SIGINFO handler.
        let (pid, uid) = unsafe { ((*info).si_pid(), (*info).si_uid()) };
        SENDER_PID.store(pid, Ordering::SeqCst);
        SENDER_UID.store(uid, Ordering::SeqCst);
        if pid > 0 {
            SENDER_PATH.look_up(pid);
            let parent = parent_of(pid);
            PARENT_PID.store(parent, Ordering::SeqCst);
            if parent > 0 {
                PARENT_PATH.look_up(parent);
            }
        }
    }
    STOP_SIGNAL.store(sig, Ordering::SeqCst);
    nix::errno::Errno::set_raw(errno);
}

#[allow(non_camel_case_types)]
type libc_int = i32;

pub fn install() {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    // SA_RESTART: without it, a signal landing on a pty reader thread ends its
    // read() with EINTR, which the reader would mistake for the pane closing.
    let action = SigAction::new(SigHandler::SigAction(on_stop_signal), SaFlags::SA_RESTART, SigSet::empty());
    for sig in [Signal::SIGTERM, Signal::SIGHUP] {
        // SAFETY: the handler only makes lookup syscalls into static buffers and stores to atomics, which is signal-safe.
        let _ = unsafe { sigaction(sig, &action) };
    }
}

/// The signal caught, or 0.
pub fn caught() -> i32 {
    STOP_SIGNAL.load(Ordering::SeqCst)
}

pub fn name(sig: i32) -> &'static str {
    if sig == nix::sys::signal::Signal::SIGHUP as i32 { "SIGHUP" } else { "SIGTERM" }
}

/// `from pid 812 uid 501 /bin/kill, parent 700 /bin/zsh`, from what the handler stored.
pub fn sender() -> String {
    let pid = SENDER_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return "from the kernel".into();
    }
    let mut s = format!("from pid {pid} uid {} {}", SENDER_UID.load(Ordering::SeqCst), SENDER_PATH.text());
    let parent = PARENT_PID.load(Ordering::SeqCst);
    if parent > 0 {
        s += &format!(", parent {parent} {}", PARENT_PATH.text());
    }
    s
}

/// A client's words for the signal caught: `SIGHUP from the kernel (the terminal hung up)`, `SIGTERM from pid …`.
pub fn reason() -> Option<String> {
    let sig = caught();
    if sig == 0 {
        return None;
    }
    let hung_up = sig == nix::sys::signal::Signal::SIGHUP as i32 && SENDER_PID.load(Ordering::SeqCst) == 0;
    Some(if hung_up { "SIGHUP from the kernel (the terminal hung up)".into() } else { format!("{} {}", name(sig), sender()) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_sender_is_named_by_its_path_and_a_dead_one_is_gone() {
        use std::os::unix::fs::MetadataExt;
        let me = super::PathSlot::new();
        me.look_up(std::process::id() as i32);
        // By inode: another test hard-links this binary, and the kernel may name either link.
        assert!(me.text().starts_with('/'), "{}", me.text());
        let ino = |p: &std::path::Path| std::fs::metadata(p).map(|m| (m.dev(), m.ino())).ok();
        assert_eq!(ino(std::path::Path::new(&me.text())), ino(&std::env::current_exe().unwrap()));
        assert_eq!(super::parent_of(std::process::id() as i32), nix::unistd::getppid().as_raw());
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap();
        let dead = super::PathSlot::new();
        dead.look_up(pid);
        assert_eq!(dead.text(), "gone");
    }
}
