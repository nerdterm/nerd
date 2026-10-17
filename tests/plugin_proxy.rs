//! A plugin with hosts and ports under the built wrapper and the real profiles; the one allowed host is on this Mac.

use std::io::{Read as _, Write as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Plugin {
    dir: PathBuf,
    wrapper: Child,
}

impl Plugin {
    /// `nerd --plugin-exec <dir>/plugin.sh <dir>/out <args>` with `script` as the plugin, allowed `net`.
    fn start(name: &str, script: &str, net: &str, args: &[&str]) -> Plugin {
        Self::start_in(name, script, net, args, true)
    }

    /// `leader`: as the broker starts it, the leader of its own process group.
    fn start_in(name: &str, script: &str, net: &str, args: &[&str], leader: bool) -> Plugin {
        let dir = std::env::temp_dir().join(format!("nerd-it-proxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["home", "state", "out"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let dir = dir.canonicalize().unwrap();
        let program = dir.join("plugin.sh");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let d = dir.display();
        let profile = format!(
            "(version 1)\n(deny default)\n(allow process-fork)\n(allow process-exec (subpath \"/bin\") (subpath \"/usr/bin\") (subpath \"{d}\"))\n\
             (allow file-read*)\n(allow file-write* (literal \"/dev/null\") (subpath \"{d}/out\"))\n(allow sysctl-read)\n(allow signal (target same-sandbox))\n"
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
        if leader {
            cmd.process_group(0);
        }
        if !net.is_empty() {
            cmd.env("NERD_PLUGIN_NET", net);
        }
        let wrapper = cmd
            .arg("--plugin-exec")
            .arg(&program)
            .arg(dir.join("out"))
            .args(args)
            .env("HOME", dir.join("home"))
            .env("NERD_STATE_DIR", dir.join("state"))
            .env("NERD_PLUGIN", "probe")
            .env("NERD_PLUGIN_PROFILE", profile)
            .env_remove("NERD_INSTANCE")
            .env_remove("NERD_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Plugin { dir, wrapper }
    }

    fn wait_for(&self, file: &str) -> String {
        let at = self.dir.join("out").join(file);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(15) {
            if let Ok(s) = std::fs::read_to_string(&at)
                && s.ends_with('\n')
            {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the plugin never wrote {file}; log: {}", self.log());
    }

    fn pid(&self) -> i32 {
        self.wait_for("pid").trim().parse().unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("state/daemon.log")).unwrap_or_default()
    }

    fn group(&self, sig: nix::sys::signal::Signal) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(self.wrapper.id() as i32), sig);
    }

    fn exit_within(&mut self, d: Duration) -> Option<std::process::ExitStatus> {
        let t = Instant::now();
        while t.elapsed() < d {
            if let Some(s) = self.wrapper.try_wait().unwrap() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.group(nix::sys::signal::Signal::SIGKILL);
        let _ = self.wrapper.kill();
        let _ = self.wrapper.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

unsafe extern "C" {
    fn sandbox_check(pid: i32, operation: *const std::ffi::c_char, kind: i32, ...) -> i32;
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// A server that writes until the other side goes, as a download would.
fn flood() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            std::thread::spawn(move || while c.write_all(&[b'x'; 64 * 1024]).is_ok() {});
        }
    });
    port
}

#[test]
fn the_proxy_refuses_what_the_manifest_does_not_name_and_outlives_a_reset_peer() {
    let port = flood();
    let script = r#"echo $$ > "$1/pid"
echo "$HTTPS_PROXY" > "$1/proxy"
/usr/bin/curl -s -o /dev/null -w '%{http_connect} ' https://example.com/ > "$1/a"; echo "$?" >> "$1/a"
/usr/bin/curl -s -o /dev/null -w '%{http_connect} ' https://localhost/ > "$1/lookup"; echo "$?" >> "$1/lookup"
P=${HTTPS_PROXY##*:}
{ printf 'CONNECT 127.0.0.1:%s HTTP/1.1\r\n\r\n' "$2"; /bin/sleep 2; } | /usr/bin/nc 127.0.0.1 "$P" | /usr/bin/head -c 1000 > "$1/f1"
{ printf 'CONNECT 127.0.0.1:%s HTTP/1.1\r\n\r\n' "$2"; /bin/sleep 1; } | /usr/bin/nc 127.0.0.1 "$P" | /usr/bin/head -c 100 > "$1/f2"
echo > "$1/done"
exec /bin/sleep 60"#;
    let mut p = Plugin::start("refuse", script, &format!("127.0.0.1,localhost;{port},443"), &[&port.to_string()]);
    let pid = p.pid();

    // (a) A host the manifest does not name: refused by the proxy under its real profile, and logged.
    assert_eq!(p.wait_for("a"), "403 56\n", "{}", p.log());
    // A name is looked up under the proxy's profile, and an answer on this Mac is refused (the wrapper takes any name).
    assert_eq!(p.wait_for("lookup"), "403 56\n", "{}", p.log());
    // (f) A peer that goes mid-transfer leaves the proxy and the plugin alive, and the next tunnel works.
    p.wait_for("done");
    let out = |f: &str| std::fs::read(p.dir.join("out").join(f)).unwrap();
    assert!(out("f1").starts_with(b"HTTP/1.1 200 ") && out("f2").starts_with(b"HTTP/1.1 200 "), "{}", p.log());
    assert!(p.wrapper.try_wait().unwrap().is_none() && alive(pid), "{}", p.log());
    // SAFETY: no operation named: asks only whether the process is sandboxed.
    assert_eq!(unsafe { sandbox_check(p.wrapper.id() as i32, std::ptr::null(), 0) }, 1, "the proxy runs in its own sandbox");

    // (b) Anyone outside the plugin's process group is refused, before a word is read.
    let proxy = p.wait_for("proxy");
    let at = proxy.trim().trim_start_matches("http://");
    let mut c = std::net::TcpStream::connect(at).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut got = String::new();
    let _ = c.read_to_string(&mut got);
    assert!(got.starts_with("HTTP/1.1 403 "), "{got:?}");

    let log = p.log();
    assert!(log.contains("proxy[") && log.contains("plugin probe proxy: example.com:443 refused: not in the manifest"), "{log}");
    assert!(log.contains(&format!("plugin probe proxy: 127.0.0.1:{port} allowed (127.0.0.1)")), "{log}");
    assert!(log.contains(&format!("plugin probe proxy: 127.0.0.1:{port} closed, ")), "{log}");
    assert!(log.contains("refused: peer outside plugin probe"), "{log}");
    assert!(log.contains("plugin probe proxy: localhost:443 refused: non-public address (") && log.contains("127.0.0.1"), "{log}");
    assert!(!log.contains("sandbox not applied"), "{log}");
}

#[test]
fn the_plugin_and_its_proxy_go_together() {
    // (c) A plugin that ignores SIGTERM: the proxy waits for it, and the group kill ends both.
    let mut p = Plugin::start("group", "trap '' TERM\necho $$ > \"$1/pid\"\nwhile :; do /bin/sleep 1; done", "wttr.in;443", &[]);
    let pid = p.pid();
    p.group(nix::sys::signal::Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(250));
    assert!(p.wrapper.try_wait().unwrap().is_none(), "the proxy waits for its plugin");
    p.group(nix::sys::signal::Signal::SIGKILL);
    assert_eq!(p.exit_within(Duration::from_secs(5)).and_then(|s| s.signal()), Some(9));
    let t = Instant::now();
    while alive(pid) && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "the plugin went with the group");

    // (e) A plugin that ends on SIGTERM: the wrapper dies by it too, at once.
    let mut p = Plugin::start("term", "echo $$ > \"$1/pid\"\nwhile :; do /bin/sleep 0.1; done", "wttr.in;443", &[]);
    p.pid();
    p.group(nix::sys::signal::Signal::SIGTERM);
    assert_eq!(p.exit_within(Duration::from_millis(250)).and_then(|s| s.signal()), Some(15));

    // (d) The plugin's exit status and its killing signal are the wrapper's.
    let mut p = Plugin::start("exit", "exit 3", "wttr.in;443", &[]);
    assert_eq!(p.exit_within(Duration::from_secs(10)).and_then(|s| s.code()), Some(3), "{}", p.log());
    // SIGUSR1 rather than a crash signal: the same path, without a crash report for every test run.
    let mut p = Plugin::start("signal", "kill -USR1 $$", "wttr.in;443", &[]);
    assert_eq!(p.exit_within(Duration::from_secs(10)).and_then(|s| s.signal()), Some(nix::sys::signal::Signal::SIGUSR1 as i32), "{}", p.log());
}

/// The peer check trusts the process group, so a wrapper that does not lead its own stops its plugin and leaves.
#[test]
fn a_wrapper_outside_its_own_group_runs_no_plugin() {
    let mut p = Plugin::start_in("leader", "echo $$ > \"$1/pid\"\nexec /bin/sleep 60", "wttr.in;443", &[], false);
    assert_eq!(p.exit_within(Duration::from_secs(10)).and_then(|s| s.code()), Some(1), "{}", p.log());
    assert!(p.log().contains("plugin probe proxy: not the leader of its own process group, plugin stopped"), "{}", p.log());
    if let Ok(pid) = std::fs::read_to_string(p.dir.join("out/pid")) {
        let pid: i32 = pid.trim().parse().unwrap();
        let t = Instant::now();
        while alive(pid) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the plugin was stopped");
    }
}

/// The same wrapper without `NERD_PLUGIN_NET` stays the plugin itself: no fork, no proxy.
#[test]
fn without_hosts_the_wrapper_is_the_plugin() {
    let p = Plugin::start("plain", "echo $$ > \"$1/pid\"\nexec /bin/sleep 60", "", &[]);
    assert_eq!(p.pid(), p.wrapper.id() as i32, "{}", p.log());
}
