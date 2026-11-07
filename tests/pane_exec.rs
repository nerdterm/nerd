//! A pane under the built `nerd --pane-exec` and its real profiles: its folder, its sockets, its proxy.

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Pane {
    dir: PathBuf,
    socks: PathBuf,
    wrapper: Child,
    /// A stand-in nerd.sock and msock, bound before the pane starts: a refusal is the profile's, not a missing file.
    _listeners: [std::os::unix::net::UnixListener; 2],
}

impl Pane {
    /// `nerd --pane-exec` with `flags` running `script` as `/bin/sh script <nerd.sock> <msock> <args>`; HOME is outside every temp tree.
    fn start(name: &str, flags: &[&str], script: &str, args: &[&str]) -> Pane {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pane-exec-{name}-{}", std::process::id()));
        let socks = PathBuf::from(format!("/tmp/nt-s5-{}-{name}", std::process::id()));
        for d in [&dir, &socks] {
            let _ = std::fs::remove_dir_all(d);
        }
        for d in [dir.join("home/Library/Caches"), dir.join("home/.claude/projects/p/memory"), dir.join("home/.gemini"), dir.join("state"), dir.join("work"), socks.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("pane.sh"), script).unwrap();
        let (own, msock) = (socks.join("s.sock"), socks.join("m.sock"));
        let _listeners = [&own, &msock].map(|p| std::os::unix::net::UnixListener::bind(p).unwrap());
        let nerd = env!("CARGO_BIN_EXE_nerd");
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { nix::libc::geteuid() }.to_string();
        let work = dir.join("work");
        let mut cmd = Command::new(nerd);
        // As under nohup or a background job: the pane's program must still get the default dispositions.
        // SAFETY: only signal(2) between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nix::libc::signal(nix::libc::SIGINT, nix::libc::SIG_IGN);
                nix::libc::signal(nix::libc::SIGHUP, nix::libc::SIG_IGN);
                Ok(())
            });
        }
        cmd.arg("--pane-exec").args(["--pane", "7", "--cwd", work.to_str().unwrap(), "--msock", msock.to_str().unwrap(), "--msock-uid", &uid, "--bin", nerd]).args(flags);
        let wrapper = cmd
            .arg("--")
            .arg("/bin/sh")
            .arg(dir.join("pane.sh"))
            .arg(&own)
            .arg(&msock)
            .args(args)
            .current_dir(&work)
            .env("HOME", dir.join("home"))
            .env("NERD_STATE_DIR", dir.join("state"))
            .env("NERD_STATE", dir.join("state"))
            .env_remove("NERD_INSTANCE")
            .env_remove("NERD_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("stderr")).unwrap())
            .spawn()
            .unwrap();
        Pane { dir, socks, wrapper, _listeners }
    }

    fn wait_for(&self, file: &str) -> String {
        let at = self.dir.join("work").join(file);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(15) {
            if let Ok(s) = std::fs::read_to_string(&at)
                && s.ends_with('\n')
            {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the pane never wrote {file}; stderr: {}; log: {}", self.read("stderr"), self.read("state/daemon.log"));
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default()
    }

    fn exit_within(&mut self, d: Duration) -> Option<std::process::ExitStatus> {
        let t = Instant::now();
        while t.elapsed() < d {
            if let Some(s) = self.wrapper.try_wait().unwrap() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.dir.join("work/pid")) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid.trim().parse().unwrap_or(0)), nix::sys::signal::Signal::SIGKILL);
        }
        let _ = self.wrapper.kill();
        let _ = self.wrapper.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.socks);
    }
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

const CONNECT: &str = "/usr/bin/perl -MIO::Socket::UNIX -e 'IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 1'";

#[test]
fn folder_and_no_network_keep_the_pane_in_its_folder_and_off_every_socket_but_the_msock() {
    let script = format!(
        "touch \"$HOME/x\" 2>/dev/null; echo \"home $?\" > out\n\
         {CONNECT} \"$1\"; echo \"nerd.sock $?\" >> out\n\
         {CONNECT} \"$2\"; echo \"msock $?\" >> out\n\
         echo \"state ${{NERD_STATE-unset}} pane $NERD_PANE\" >> out\n\
         /usr/bin/nc -z -G 3 192.0.2.1 80 2>/dev/null; echo \"tcp $?\" >> out\n\
         for c in com.apple.python Homebrew other; do mkdir -p \"$HOME/Library/Caches/$c\" 2>/dev/null; echo x 2>/dev/null > \"$HOME/Library/Caches/$c/x\"; echo \"cache $c $?\" >> out; done\n\
         echo > done\n"
    );
    let pane = Pane::start("none", &["--files", "folder", "--network", "none"], &script, &[]);
    pane.wait_for("done");
    let out = pane.wait_for("out");
    assert!(out.starts_with("home 1\n"), "cannot create $HOME/x: {out}");
    assert!(out.contains("nerd.sock 1\n"), "cannot reach a stand-in nerd.sock: {out}");
    assert!(out.contains("msock 0\n"), "reaches the msock: {out}");
    assert!(out.contains("state unset pane 7\n"), "no NERD_STATE, its own NERD_PANE: {out}");
    assert!(!out.contains("tcp 0\n"), "no TCP: {out}");
    assert!(!out.contains("cache com.apple.python 0\n") && !out.contains("cache Homebrew 0\n"), "caches that run code later stay read-only: {out}");
    assert!(out.contains("cache other 0\n"), "other caches stay writable: {out}");
    assert!(!pane.dir.join("home/x").exists());
}

#[test]
fn folder_only_cannot_move_a_folder_above_a_denied_one_nor_link_one_in() {
    let script = "P=\"$HOME/.claude/projects\"\n\
         mkdir -p swap/memory; /usr/bin/perl -e 'syscall(488, -2, $ARGV[0], -2, $ARGV[1], 2) == 0 or exit 1' \"$P/p\" swap; echo \"swap project $?\" > out\n\
         mv \"$P/p\" moved 2>/dev/null; echo \"move project $?\" >> out\n\
         mv \"$HOME/Library/Caches\" caches 2>/dev/null; echo \"move caches $?\" >> out\n\
         mkdir -p planted/memory; ln -s \"$PWD/planted\" \"$P/q\" 2>/dev/null; echo \"link project $?\" >> out\n\
         mkdir \"$P/n\" && echo s > \"$P/n/s.jsonl\"; echo \"new project $?\" >> out\n\
         echo s > \"$P/p/s.jsonl\" && mv \"$P/p/s.jsonl\" \"$P/p/t.jsonl\" && rm \"$P/p/t.jsonl\"; echo \"session $?\" >> out\n\
         mkdir -p \"$HOME/Library/Caches/other\" && echo x > \"$HOME/Library/Caches/other/x\"; echo \"cache $?\" >> out\n\
         mkdir -p staged/bin; /usr/bin/perl -e 'rename($ARGV[0], $ARGV[1]) or exit 1' staged \"$HOME/.gemini/tmp\"; echo \"move agent folder in $?\" >> out\n\
         echo > done\n";
    let pane = Pane::start("rename", &["--files", "folder", "--network", "none"], script, &[]);
    pane.wait_for("done");
    let out = pane.wait_for("out");
    for no in ["swap project", "move project", "move caches", "link project", "move agent folder in"] {
        assert!(!out.contains(&format!("{no} 0\n")), "{no}: {out}");
    }
    for ok in ["new project", "session", "cache"] {
        assert!(out.contains(&format!("{ok} 0\n")), "{ok}: {out}");
    }
    assert!(pane.dir.join("home/.claude/projects/p/memory").is_dir() && pane.dir.join("home/Library/Caches").is_dir());
    assert!(std::fs::symlink_metadata(pane.dir.join("home/.claude/projects/q")).is_err());
    assert!(pane.dir.join("home/.gemini/tmp").is_dir() && !pane.dir.join("home/.gemini/tmp/bin").exists(), "an agent's missing folder above a denied one is made before the pane starts");
}

#[test]
fn folder_only_finds_the_session_folder_writable_and_leaves_nothing_in_it() {
    let pane = Pane::start("writecheck", &["--files", "folder", "--network", "none"], "echo > done\n", &[]);
    pane.wait_for("done");
    assert!(!pane.read("stderr").contains("not writable"), "{}", pane.read("stderr"));
    let left: Vec<_> = std::fs::read_dir(pane.dir.join("home/.claude/projects")).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n != "p").collect();
    assert!(left.is_empty(), "the write check leaves {left:?}");
}

#[test]
fn only_its_provider_survives_ctrl_c_and_goes_with_its_program() {
    let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = echo.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in echo.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut &c, &mut &c);
            });
        }
    });
    let script = "echo $$ > pid\n\
         P=${HTTPS_PROXY##*:}\n\
         { printf 'CONNECT 127.0.0.1:%s HTTP/1.1\\r\\n\\r\\n' \"$3\"; /bin/sleep 1; } | /usr/bin/nc 127.0.0.1 \"$P\" | /usr/bin/head -c 12 > tunnel; echo >> tunnel\n\
         /usr/bin/nc -z -G 2 127.0.0.1 \"$3\"; echo \"direct $?\" > direct\n\
         echo \"$NODE_USE_ENV_PROXY\" > ready\n\
         exec /bin/sleep 60\n";
    let hosts = format!("127.0.0.1;{port}");
    let mut pane = Pane::start("provider", &["--files", "everything", "--network", "provider", "--hosts", &hosts], script, &[&port.to_string()]);
    let pid: i32 = pane.wait_for("pid").trim().parse().unwrap();
    assert_eq!(pane.wait_for("ready"), "1\n");
    assert_eq!(pane.wait_for("tunnel"), "HTTP/1.1 200\n", "the proxy admits its pane's program: {}", pane.read("state/daemon.log"));
    assert_ne!(pane.wait_for("direct"), "direct 0\n", "the program reaches only the proxy");
    let log = pane.read("state/daemon.log");
    assert!(log.contains(&format!("pane 7 proxy: 127.0.0.1:{port} allowed (127.0.0.1)")), "{log}");
    assert_ne!(pid, pane.wrapper.id() as i32, "the program is forked off its proxy");

    // Ctrl+C reaches the terminal's front group, never the proxy.
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pane.wrapper.id() as i32), nix::sys::signal::Signal::SIGINT);
    assert!(pane.exit_within(Duration::from_millis(300)).is_none(), "the proxy ignores SIGINT");
    assert!(alive(pid));
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGINT);
    assert_eq!(pane.exit_within(Duration::from_secs(5)).and_then(|s| s.signal()), Some(2), "the proxy leaves as its program did");
    assert!(!alive(pid));
}

#[test]
fn a_hangup_reaches_the_program_behind_the_proxy() {
    let mut pane = Pane::start("hangup", &["--files", "everything", "--network", "provider", "--hosts", "example.com;443"], "echo $$ > pid\nexec /bin/sleep 60\n", &[]);
    let pid: i32 = pane.wait_for("pid").trim().parse().unwrap();
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pane.wrapper.id() as i32), nix::sys::signal::Signal::SIGHUP);
    assert_eq!(pane.exit_within(Duration::from_secs(5)).and_then(|s| s.signal()), Some(1), "closing the pane ends its program and the proxy");
    assert!(!alive(pid));
}

#[test]
fn a_shared_door_that_is_not_nerds_own_starts_nothing() {
    let nerd = env!("CARGO_BIN_EXE_nerd");
    let door = format!("/tmp/nerd-{}/main.msock", u32::MAX - 7);
    let out = Command::new(nerd)
        .args(["--pane-exec", "--pane", "1", "--files", "everything", "--network", "none", "--cwd", "/", "--msock", &door, "--msock-uid", &(u32::MAX - 7).to_string(), "--bin", nerd, "--", "/usr/bin/touch", "/tmp/never"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stderr), format!("nerd: /tmp/nerd-{} is not nerd's own; this pane does not start\n", u32::MAX - 7));
    assert!(!Path::new("/tmp/never").exists());
}
