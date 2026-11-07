//! A pane under the built `nerd --pane-exec` and its real profiles: its folder, its sockets, its proxy.

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Pane {
    dir: PathBuf,
    socks: PathBuf,
    /// A Folder pane's own tmux folder, which its wrapper makes: `$TMPDIR/nm<wrapper pid>`.
    mux: Option<PathBuf>,
    wrapper: Child,
    /// A stand-in nerd.sock and msock, bound before the pane starts: a refusal is the profile's, not a missing file.
    _listeners: [std::os::unix::net::UnixListener; 2],
}

impl Pane {
    /// `nerd --pane-exec` with `flags` running `script` as `/bin/sh script <nerd.sock> <msock> <args>`; HOME is outside every temp tree.
    fn start(name: &str, flags: &[&str], script: &str, args: &[&str]) -> Pane {
        Pane::start_with(name, flags, script, args, |_| {})
    }

    /// As `start`, with `setup` given the test's folder (home, state, work) before the pane starts.
    fn start_with(name: &str, flags: &[&str], script: &str, args: &[&str], setup: impl FnOnce(&Path)) -> Pane {
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
        setup(&dir);
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
        let mux = flags.contains(&"folder").then(|| std::env::temp_dir().join(format!("nm{}", wrapper.id())));
        Pane { dir, socks, mux, wrapper, _listeners }
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
        if let Some(m) = &self.mux {
            let _ = std::fs::remove_dir_all(m);
        }
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

/// Each plugin's agent `data` is writable in a folder pane; a folder put in by hand and never let in adds nothing.
#[test]
fn a_plugin_folder_never_let_in_adds_nothing_to_a_folder_pane() {
    let setup = |dir: &Path| {
        for n in ["kept", "planted"] {
            let p = dir.join("state/plugins").join(n);
            std::fs::create_dir_all(&p).unwrap();
            let m = serde_json::json!({ "name": n, "title": n, "agent": { "names": [n], "data": [format!("~/.{n}/sessions")] } });
            std::fs::write(p.join("plugin.json"), m.to_string()).unwrap();
            std::fs::create_dir_all(dir.join("home").join(format!(".{n}/sessions"))).unwrap();
        }
        std::fs::write(dir.join("state/plugins-admitted.json"), r#"["kept"]"#).unwrap();
    };
    let script = "touch \"$HOME/.kept/sessions/x\" 2>/dev/null; echo \"kept $?\" > out\n\
         touch \"$HOME/.planted/sessions/x\" 2>/dev/null; echo \"planted $?\" >> out\n\
         echo > done\n";
    // Named by the server, and from an older server that names none: the file then.
    for (name, flags) in [("named", &["--files", "folder", "--network", "none", "--plugins", "kept"][..]), ("unnamed", &["--files", "folder", "--network", "none"][..])] {
        let pane = Pane::start_with(name, flags, script, &[], setup);
        pane.wait_for("done");
        assert_eq!(pane.wait_for("out"), "kept 0\nplanted 1\n", "{name}: {}", pane.read("stderr"));
    }
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

/// Removed when the test ends, failed or not.
struct Gone(PathBuf);

impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_folder_pane_with_open_network_has_a_tmux_folder_of_its_own() {
    // Another pane's folder: digits only, as the pattern denies, and longer than any real pid.
    let other = Gone(std::env::temp_dir().join(format!("nm{}{}", u32::MAX, std::process::id())));
    std::fs::create_dir_all(&other.0).unwrap();
    let _l = std::os::unix::net::UnixListener::bind(other.0.join("s")).unwrap();
    let script = format!(
        "echo $$ > pid\n\
         if [ \"$TMUX_TMPDIR\" = \"${{TMPDIR%/}}/nm$$\" ] && [ \"$ZELLIJ_SOCKET_DIR\" = \"$TMUX_TMPDIR\" ] && [ \"$SCREENDIR\" = \"$TMUX_TMPDIR/s\" ]; then echo vars; else echo \"novars $TMUX_TMPDIR $ZELLIJ_SOCKET_DIR $SCREENDIR\"; fi > out\n\
         ls -ld \"$TMUX_TMPDIR\" | cut -c1-10 >> out\n\
         ls -ld \"$SCREENDIR\" | cut -c1-10 >> out\n\
         /usr/bin/perl -MIO::Socket::UNIX -e '$l = IO::Socket::UNIX->new(Local => $ARGV[0], Listen => 1) or exit 1; IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 2' \"$TMUX_TMPDIR/x\"; echo \"own $?\" >> out\n\
         {CONNECT} \"$3/s\"; echo \"other $?\" >> out\n\
         echo x 2>/dev/null > \"$3/w\"; echo \"other-write $?\" >> out\n\
         echo > done\n"
    );
    let pane = Pane::start("mux-open", &["--files", "folder", "--network", "open"], &script, &[other.0.to_str().unwrap()]);
    pane.wait_for("done");
    assert_eq!(pane.wait_for("out"), "vars\ndrwx------\ndrwx------\nown 0\nother 1\nother-write 1\n", "stderr: {}", pane.read("stderr"));
    assert!(!other.0.join("w").exists());
}

#[test]
fn a_providers_folder_pane_names_its_tmux_folder_in_the_parent() {
    let script = "echo \"$TMUX_TMPDIR\" > out\n\
         ls -ld \"$TMUX_TMPDIR\" | cut -c1-10 >> out\n\
         echo x > \"$TMUX_TMPDIR/x\"; echo \"w $?\" >> out\n\
         echo > done\n";
    let pane = Pane::start("mux-provider", &["--files", "folder", "--network", "provider", "--hosts", "example.com;443"], script, &[]);
    pane.wait_for("done");
    let out = pane.wait_for("out");
    let mux = pane.mux.as_ref().unwrap().display().to_string();
    assert_eq!(out, format!("{mux}\ndrwx------\nw 0\n"), "named from the proxy's pid, before the fork; stderr: {}", pane.read("stderr"));
}

unsafe extern "C" {
    fn sandbox_check(pid: nix::libc::pid_t, operation: *const std::ffi::c_char, kind: std::ffi::c_int, ...) -> std::ffi::c_int;
}

/// `sandboxed lsopen job-creation appleevent-send user-preference-write shortcuts-xpc shortcuts-runner siriactionsd denied-file allowed-file` as this process's sandbox answers them; only asks, launches nothing.
fn launch_checks(denied: &Path, allowed: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    const NO_REPORT: std::ffi::c_int = 0x4000_0000;
    let pid = std::process::id() as nix::libc::pid_t;
    // SAFETY: sandbox_check reads only the NUL-terminated strings passed, which outlive each call.
    let mut got = vec![unsafe { sandbox_check(pid, std::ptr::null(), 0) }];
    for op in [c"lsopen", c"job-creation", c"appleevent-send", c"user-preference-write"] {
        got.push(unsafe { sandbox_check(pid, op.as_ptr(), NO_REPORT) });
    }
    for name in [c"com.apple.siri.VoiceShortcuts.xpc", c"com.apple.shortcuts.systemshortcutservice.runner", c"com.apple.siriactionsd.xpc"] {
        got.push(unsafe { sandbox_check(pid, c"mach-lookup".as_ptr(), 2 | NO_REPORT, name.as_ptr()) });
    }
    for f in [denied, allowed] {
        let path = std::ffi::CString::new(f.as_os_str().as_bytes()).unwrap();
        got.push(unsafe { sandbox_check(pid, c"file-write-data".as_ptr(), 1 | NO_REPORT, path.as_ptr()) });
    }
    got.iter().map(i32::to_string).collect::<Vec<_>>().join(" ")
}

/// Run by a pane (below) as this test binary: writes the pane's own `launch_checks` to `$NERD_LAUNCH_PROBE`.
#[test]
fn launch_probe_helper() {
    let Some(out) = std::env::var_os("NERD_LAUNCH_PROBE").map(PathBuf::from) else { return };
    let work = out.parent().unwrap();
    std::fs::write(work.join("allowed"), "").unwrap();
    std::fs::write(&out, launch_checks(&work.parent().unwrap().join("pane.sh"), &work.join("allowed")) + "\n").unwrap();
}

/// The pane's line, then this process's own on the same files, which exist by then.
fn launch_lines(name: &str, flags: &[&str]) -> (String, String) {
    let exe = std::env::current_exe().unwrap();
    let script = "NERD_LAUNCH_PROBE=$PWD/probe \"$3\" --exact launch_probe_helper --nocapture >/dev/null 2>&1\n";
    let pane = Pane::start(name, flags, script, &[exe.to_str().unwrap()]);
    let probe = pane.wait_for("probe");
    (probe.trim_end().to_string(), launch_checks(&pane.dir.join("pane.sh"), &pane.dir.join("work/allowed")))
}

#[test]
fn launching_is_refused_in_a_folder_pane() {
    let (pane, own) = launch_lines("launch-folder", &["--files", "folder", "--network", "none"]);
    // job-creation reads 1 under any Seatbelt profile, so its deny cannot be told from its absence here; the profile text test pins it.
    assert_eq!(pane, "1 1 1 1 1 1 1 1 1 0", "sandboxed lsopen job-creation appleevent-send user-preference-write shortcuts-xpc shortcuts-runner siriactionsd denied-file allowed-file");
    assert_eq!(own, "0 0 0 0 0 0 0 0 0 0", "this test process is not under Seatbelt, so the pane's answers are its profile's");
}

#[test]
fn an_account_pane_with_open_network_has_no_profile() {
    let (pane, own) = launch_lines("launch-account", &["--files", "everything", "--network", "open"]);
    assert_eq!(pane, "0 0 0 0 0 0 0 0 0 0", "no Seatbelt profile: setuid programs and an agent's own sandbox keep working");
    assert_eq!(own, "0 0 0 0 0 0 0 0 0 0");
}
