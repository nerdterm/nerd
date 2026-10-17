//! Every child nerd starts: macOS has no atomic close-on-exec for pipe, socketpair or accept,
//! so a child forked by one thread can inherit what another thread opened a moment earlier.

use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt as _;
use std::process::Command;

use nix::libc;

/// Starts `cmd` and reaps it on a thread of its own: a child nobody waits for stays a zombie for as long as nerd runs.
pub fn detached(cmd: &mut Command) -> std::io::Result<u32> {
    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || child.wait());
    Ok(pid)
}

/// A `Command` whose child gets only stdin, stdout, stderr and what `pass` names.
pub fn cmd(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    let top = unsafe { libc::getdtablesize() };
    // SAFETY: only syscalls after the fork, no allocation. Marking, not closing, keeps std's exec-error pipe.
    unsafe {
        cmd.pre_exec(move || {
            mark_all(top);
            Ok(())
        });
    }
    cmd
}

/// `src` arrives in the child as `to`; `cmd`'s marking runs first, so the order is safe even when `src == to`.
pub fn pass(cmd: &mut Command, src: RawFd, to: RawFd) {
    // SAFETY: dup2 and fcntl are async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(src, to) < 0 || libc::fcntl(to, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn mark_all(top: i32) {
    const CAP: usize = 1024;
    let mut fds = [libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; CAP];
    let size = std::mem::size_of_val(&fds) as i32;
    let got = unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), size) };
    let n = got.max(0) as usize / std::mem::size_of::<libc::proc_fdinfo>();
    if got <= 0 || n == CAP {
        // The list failed or may be cut short: every possible fd, slower when the fd limit is high.
        for fd in 3..top {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        return;
    }
    for f in &fds[..n] {
        if f.proc_fd > 2 {
            unsafe { libc::fcntl(f.proc_fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::time::{Duration, Instant};

    #[test]
    fn a_detached_child_leaves_no_zombie() {
        let pid = detached(&mut cmd("/usr/bin/true")).unwrap() as i32;
        let deadline = Instant::now() + Duration::from_secs(5);
        // a zombie still answers kill(0); a reaped child is gone
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
            assert!(Instant::now() < deadline, "pid {pid} was never reaped");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_child_never_holds_another_threads_pipe() {
        // Alone in a fresh process: another test's child would inherit the write end too.
        if std::env::var_os("NERD_SPAWN_ALONE").is_none() {
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["spawn::tests::a_child_never_holds_another_threads_pipe", "--exact", "--test-threads=1"])
                .env("NERD_SPAWN_ALONE", "1")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&out.stderr));
            return;
        }
        let (mut r, w) = std::io::pipe().unwrap();
        // As a pipe another thread made just before its CLOEXEC was set.
        let w = w.into_raw_fd();
        unsafe { libc::fcntl(w, libc::F_SETFD, 0) };
        let mut child = cmd("/bin/sleep").arg("2").spawn().unwrap();
        unsafe { libc::close(w) };
        let start = Instant::now();
        assert_eq!(r.read(&mut [0u8; 1]).unwrap(), 0);
        let waited = start.elapsed();
        let _ = child.kill();
        let _ = child.wait();
        assert!(waited < Duration::from_secs(1), "EOF after {waited:?}: the child held the write end");
    }

    #[test]
    fn a_passed_fd_arrives_open() {
        let (mine, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut c = cmd("/bin/sh");
        c.args(["-c", "echo hi >&3"]);
        pass(&mut c, theirs.as_raw_fd(), 3);
        assert!(c.status().unwrap().success());
        drop(theirs);
        let mut got = String::new();
        (&mine).read_to_string(&mut got).unwrap();
        assert_eq!(got, "hi\n");
    }

    #[test]
    fn a_missing_program_is_still_an_error() {
        let e = cmd("/nonexistent/nerd-no-such-program").spawn().unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    }
}
