//! End-to-end test of the daemon<->client protocol: a real socket, a real child
//! `nerd --daemon` process, a real shell inside a pane.
//!
//! `nerd` is a binary crate with no lib target, so `proto::ClientMsg` isn't visible
//! here; this sends JSON of the same shape directly, matching proto.rs field-for-field.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde::Serialize;

#[derive(Serialize)]
enum ClientMsg {
    Hello { cwd: PathBuf, cols: u16, rows: u16 },
    Event(Event),
    Detach,
    Shutdown,
    Update,
    DisconnectAll,
    Probe,
    Connect { protocol: u32, role: String, plugin: Option<String>, events: Vec<String> },
}

impl ClientMsg {
    fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        serde_json::to_writer(&mut *w, self)?;
        w.write_all(b"\n")?;
        w.flush()
    }
}

/// Kills the daemon when the test exits, even if an assertion failed.
struct DaemonGuard(Child);

impl DaemonGuard {
    /// Kills the daemon and waits for the process to exit (for an explicit check in the test).
    fn kill_and_wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.kill()?;
        self.0.wait()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fresh, isolated HOME/NERD_STATE_DIR/NERD_SOCKET for each test.
struct Env {
    dir: PathBuf,
    socket: PathBuf,
}

fn setup_env(name: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("nerd-daemon-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Answered already: the first-start question would cover the screens these tests read.
    let nerd = dir.join("Library/Application Support/nerd");
    std::fs::create_dir_all(&nerd).unwrap();
    std::fs::write(nerd.join("update-check"), "off\n").unwrap();
    Env { socket: dir.join("nerd.sock"), dir }
}

fn spawn_daemon(env: &Env) -> DaemonGuard {
    spawn_daemon_with_term(env, "xterm-256color", true)
}

/// `probe`: connect once to know the daemon listens. It costs a client id, so a test about
/// ids passes false and lets `connect` retry instead.
fn spawn_daemon_with_term(env: &Env, term: &str, probe: bool) -> DaemonGuard {
    spawn_daemon_without(env, env!("CARGO_BIN_EXE_nerd").as_ref(), term, probe, &[])
}

/// `unset`: variables of this process the daemon must not inherit, `NERD_STATE_DIR` and `NERD_SOCKET` included.
fn spawn_daemon_without(env: &Env, exe: &std::path::Path, term: &str, probe: bool, unset: &[&str]) -> DaemonGuard {
    spawn_daemon_in(env, exe, term, probe, unset, &[])
}

/// `extra`: variables set after the usual ones, so they win.
fn spawn_daemon_in(env: &Env, exe: &std::path::Path, term: &str, probe: bool, unset: &[&str], extra: &[(&str, std::ffi::OsString)]) -> DaemonGuard {
    use std::os::unix::process::CommandExt as _;
    let mut cmd = Command::new(exe);
    // No controlling tty and no terminfo override, as the client launches it: otherwise a test run from a terminal hides tty-size bugs.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
    }
    for k in unset {
        cmd.env_remove(k);
    }
    let child = cmd
        .arg("--daemon")
        .env("TERM", term)
        .env_remove("TERMINFO")
        .env_remove("TERMINFO_DIRS")
        .env("HOME", &env.dir)
        .envs([("NERD_STATE_DIR", &env.dir), ("NERD_SOCKET", &env.socket)].into_iter().filter(|(k, _)| !unset.contains(k)))
        .env("SHELL", "/bin/sh")
        .envs(extra.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("nerd --daemon failed to start");
    let guard = DaemonGuard(child);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // The file can be left from the previous daemon before the new one listens.
        let up = if probe { UnixStream::connect(&env.socket).is_ok() } else { env.socket.exists() };
        if up {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("socket {} did not appear within 5s", env.socket.display());
}

fn connect(env: &Env, cols: u16, rows: u16) -> UnixStream {
    connect_in(env, &env.dir, cols, rows)
}

/// A client whose first space opens on `cwd`.
fn connect_in(env: &Env, cwd: &std::path::Path, cols: u16, rows: u16) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(&env.socket) {
            Ok(s) => break s,
            Err(e) if Instant::now() >= deadline => panic!("connecting to socket: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut w = stream.try_clone().unwrap();
    ClientMsg::Hello { cwd: cwd.to_path_buf(), cols, rows }.write_to(&mut w).unwrap();
    stream
}

fn send_key(w: &mut impl Write, c: char) {
    ClientMsg::Event(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
        .write_to(w)
        .unwrap();
}

fn send_enter(w: &mut impl Write) {
    ClientMsg::Event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
        .write_to(w)
        .unwrap();
}

/// Strips ANSI escapes (CSI `ESC [ ... final` and OSC `ESC ] ... BEL`) so the
/// needle substring isn't accidentally split by a code in the middle of a word.
/// Returns the text and how many bytes it used: an escape cut off at the end waits for the next read.
fn strip_ansi(bytes: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let Some(&kind) = bytes.get(i + 1) else { return (out, i) };
            let end = match kind {
                b'[' => bytes[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b)),
                b']' => bytes[i + 2..].iter().position(|&b| b == 0x07),
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                    continue;
                }
            };
            let Some(end) = end else { return (out, i) };
            i += end + 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    (out, i)
}

/// Reads frames until `needle` appears in the ANSI-stripped bytes, or the timeout expires.
/// Each read is stripped and searched once, so a long backlog costs no more than its length.
fn wait_for(stream: &mut UnixStream, needle: &str, timeout: Duration) -> bool {
    wait_text(stream, needle, timeout).0
}

/// `wait_for` that also hands back the text it read, for a failure to show.
fn wait_text(stream: &mut UnixStream, needle: &str, timeout: Duration) -> (bool, String) {
    let deadline = Instant::now() + timeout;
    let needle = needle.as_bytes();
    let (mut raw, mut text) = (Vec::new(), Vec::new());
    let mut buf = [0u8; 65536];
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => return (false, String::from_utf8_lossy(&text).into_owned()), // EOF -- connection closed
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                let (out, used) = strip_ansi(&raw);
                raw.drain(..used);
                let from = text.len().saturating_sub(needle.len());
                text.extend(out);
                if text[from..].windows(needle.len()).any(|w| w == needle) {
                    return (true, String::from_utf8_lossy(&text).into_owned());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return (false, String::from_utf8_lossy(&text).into_owned()),
        }
    }
    (false, String::from_utf8_lossy(&text).into_owned())
}

#[test]
fn full_session_hello_echo_switch_detach() {
    let env = setup_env("full");
    let mut daemon = spawn_daemon(&env);

    let mut client1 = connect(&env, 80, 24);
    assert!(
        wait_for(&mut client1, "spaces", Duration::from_secs(5)),
        "sidebar with the spaces header did not appear"
    );

    for c in "echo NERD-E2E-42".chars() {
        send_key(&mut client1, c);
    }
    send_enter(&mut client1);
    assert!(
        wait_for(&mut client1, "NERD-E2E-42", Duration::from_secs(5)),
        "echo output did not appear in the first client's frames"
    );

    // A second client takes over the screen: the first gets EOF, and the pane survives the switch.
    let mut client2 = connect(&env, 80, 24);

    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got_eof = false;
    while Instant::now() < deadline {
        match client1.read(&mut buf) {
            Ok(0) => {
                got_eof = true;
                break;
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    assert!(got_eof, "first client did not get EOF after the second one connected");

    assert!(
        wait_for(&mut client2, "NERD-E2E-42", Duration::from_secs(5)),
        "second client did not see the pane's prior output"
    );

    ClientMsg::Detach.write_to(&mut client2).unwrap();
    std::thread::sleep(Duration::from_millis(100));

    // The shell already responded, so the broker brought it up via SCM_RIGHTS while the
    // daemon was sandboxed; if Seatbelt ever chokes its own domain socket, this fails first.
    let log = std::fs::read_to_string(env.dir.join("daemon.log")).expect("daemon.log is not readable");
    assert!(log.contains("sandbox: enabled"), "daemon did not enter the sandbox: {log}");
    assert_eq!(std::fs::read_to_string(env.dir.join("sandbox")).unwrap(), "enabled, network denied to the daemon");

    // Nothing cleans up the socket on SIGKILL, so its absence isn't required.
    let status = daemon.kill_and_wait().expect("daemon did not exit after kill()");
    assert!(!env.socket.exists() || !status.success(), "daemon should either remove the socket or just exit");

    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Without a UTF-8 character type, bash's readline turns byte 0x85 of a typed path into shell-expand-line.
#[test]
fn a_pane_without_a_locale_gets_a_utf8_ctype() {
    let env = setup_env("locale");
    let mut daemon = spawn_daemon_without(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &["LANG", "LC_ALL", "LC_CTYPE"]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no first frame");
    for c in "echo CT=$LC_CTYPE=".chars() {
        send_key(&mut client, c);
    }
    send_enter(&mut client);
    assert!(wait_for(&mut client, "CT=UTF-8=", Duration::from_secs(5)), "the pane's LC_CTYPE is not UTF-8");
    daemon.kill_and_wait().unwrap();
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A server started with a bare environment still gives a pane what the login profile sets, as a terminal does.
#[test]
fn a_pane_shell_reads_the_login_profile() {
    let env = setup_env("login");
    std::fs::write(env.dir.join(".profile"), "NERD_LOGIN_MARK=profile-read\n").unwrap();
    let mut daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no first frame");
    for c in "echo LM=$NERD_LOGIN_MARK=".chars() {
        send_key(&mut client, c);
    }
    send_enter(&mut client);
    assert!(wait_for(&mut client, "LM=profile-read=", Duration::from_secs(5)), "the pane's shell did not read ~/.profile");
    daemon.kill_and_wait().unwrap();
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Types a marker into the pane and waits until the shell echoes it.
fn type_marker(env: &Env, marker: &str) {
    let mut client = connect(env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no first frame");
    for c in format!("echo {marker}").chars() {
        send_key(&mut client, c);
    }
    send_enter(&mut client);
    assert!(wait_for(&mut client, marker, Duration::from_secs(5)), "marker never echoed");
    ClientMsg::Detach.write_to(&mut client).unwrap();
    std::thread::sleep(Duration::from_millis(100));
}

/// `wait` with a deadline: a regression must fail the test, not hang the suite.
fn wait_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "the daemon did not exit within {timeout:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The daemon's own exit through `--stop`: the socket message, then the process ends.
fn stop_and_wait(env: &Env, daemon: &mut DaemonGuard) {
    let mut s = UnixStream::connect(&env.socket).unwrap();
    ClientMsg::Shutdown.write_to(&mut s).unwrap();
    let status = wait_exit(&mut daemon.0, Duration::from_secs(10));
    assert!(status.success(), "clean exit expected, got {status}");
}

fn screens_dir(env: &Env) -> PathBuf {
    env.dir.join("screens.noindex")
}

fn screen_files(env: &Env) -> usize {
    std::fs::read_dir(screens_dir(env)).map(|rd| rd.count()).unwrap_or(0)
}

#[test]
fn a_server_reaps_the_finished_children_it_inherits() {
    let env = setup_env("reap");
    // As an update in place: the process that becomes the server already has a child, finished by then.
    // perl, unlike sh, leaves its finished child unreaped across the exec.
    let mut child = Command::new("/usr/bin/perl")
        .args(["-e", "if (fork() == 0) { exit 0 } select(undef, undef, undef, 0.3); exec @ARGV or die", env!("CARGO_BIN_EXE_nerd"), "--daemon"])
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .env("SHELL", "/bin/sh")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !daemon_log(&env).contains("reaped") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let zombies = Command::new("/bin/ps").args(["-axo", "ppid=,stat="]).output().unwrap();
    let mine = String::from_utf8_lossy(&zombies.stdout).lines().filter(|l| l.split_whitespace().next() == Some(&child.id().to_string()) && l.contains('Z')).count();
    let log = daemon_log(&env);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&env.dir);
    assert!(log.contains("reaped 1 finished child processes left from before"), "{log}");
    assert_eq!(mine, 0, "no zombie under the server");
}

fn daemon_log(env: &Env) -> String {
    std::fs::read_to_string(env.dir.join("daemon.log")).unwrap_or_default()
}

#[test]
fn nerd_without_a_terminal_starts_no_server() {
    let env = setup_env("no-terminal");
    // An agent's tool shell: no terminal on either side.
    let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains("not a terminal"), "{stderr}");
    assert!(!env.socket.exists(), "no server was started");
    let log = daemon_log(&env);
    assert_eq!(log.matches("left: ").count(), 1, "one left line: {log}");
    assert!(log.contains("left: error: not a terminal"), "{log}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_client_attaching_after_an_update_is_not_dropped() {
    let env = setup_env("update-second-client");
    // No probe: the first client must own id 1, as a real user's first client does.
    let _daemon = spawn_daemon_with_term(&env, "xterm-256color", false);
    let mut first = connect(&env, 80, 24);
    assert!(wait_for(&mut first, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));

    ClientMsg::Update.write_to(&mut first).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !daemon_log(&env).contains("carried over") {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(daemon_log(&env).contains("carried over"), "no handover: {}", daemon_log(&env));

    // The new client takes the screen; the replaced one's Detach must not shut it down.
    let mut second = connect(&env, 80, 24);
    assert!(wait_for(&mut second, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let mut buf = [0u8; 4096];
    second.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match second.read(&mut buf) {
            Ok(0) => panic!("the second client was dropped: {}", daemon_log(&env)),
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

#[test]
fn a_sandbox_that_failed_is_noticed_on_attach_and_after_an_update() {
    let env = setup_env("sandbox-notice");
    let _daemon = spawn_daemon(&env);
    let mut first = connect(&env, 80, 24);
    assert!(wait_for(&mut first, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // As the daemon writes it when sandbox_init fails, which a test cannot make happen.
    std::fs::write(env.dir.join("sandbox"), "did NOT enable (test reason), running without it").unwrap();
    // The notice card: a gap between words may arrive as a cursor move or as a space, so the needle is one word
    // found only in the notice (the space's name holds "sandbox").
    let notice = "network";
    ClientMsg::Update.write_to(&mut first).unwrap();
    assert!(wait_for(&mut first, notice, Duration::from_secs(5)), "no notice after the update: {}", daemon_log(&env));
    // A key clears the notice; its echo says the key was taken.
    for c in "QZ7".chars() {
        send_key(&mut first, c);
    }
    assert!(wait_for(&mut first, "QZ7", Duration::from_secs(5)), "no echo: {}", daemon_log(&env));
    let mut second = connect(&env, 80, 24);
    assert!(wait_for(&mut second, notice, Duration::from_secs(5)), "no notice on attach: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn an_update_does_not_invent_a_graphics_answer() {
    let env = setup_env("update-graphics");
    let _daemon = spawn_daemon_with_term(&env, "xterm-256color", true);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));

    // This client says nothing about graphics, as a client older than the query does.
    ClientMsg::Update.write_to(&mut client).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !daemon_log(&env).contains("carried over") {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = daemon_log(&env);
    assert!(log.contains("carried over"), "no handover: {log}");
    assert!(!log.contains("graphics=true"), "the update invented an answer: {log}");
}

#[test]
fn a_term_without_terminfo_does_not_stop_the_server() {
    let env = setup_env("unknown-term");
    let _daemon = spawn_daemon_with_term(&env, "xterm-kitty", true);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(!daemon_log(&env).contains("stopped by"), "{}", daemon_log(&env));
}

#[test]
fn screens_come_back_after_stop() {
    let env = setup_env("screens-stop");
    let mut daemon = spawn_daemon(&env);
    type_marker(&env, "NERD-SCREEN-STOP");
    stop_and_wait(&env, &mut daemon);
    assert_eq!(screen_files(&env), 1, "one pane, one text file after --stop");
    assert!(daemon_log(&env).contains("stopped by --stop"), "{}", daemon_log(&env));

    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(
        wait_for(&mut client, "NERD-SCREEN-STOP", Duration::from_secs(5)),
        "the marker did not come back after the restart"
    );
    assert_eq!(screen_files(&env), 0, "the text file is consumed on restore");
    assert!(daemon_log(&env).contains("screens: 1 restored"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn screens_come_back_after_sigterm() {
    let env = setup_env("screens-term");
    let mut daemon = spawn_daemon(&env);
    type_marker(&env, "NERD-SCREEN-TERM");
    let pid = daemon.0.id();
    assert!(Command::new("kill").args(["-TERM", &pid.to_string()]).status().unwrap().success());
    let status = wait_exit(&mut daemon.0, Duration::from_secs(10));
    assert!(status.success(), "a handled SIGTERM is a clean exit, got {status}");
    assert!(daemon_log(&env).contains("stopped by SIGTERM"), "{}", daemon_log(&env));
    assert_eq!(screen_files(&env), 1);

    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(
        wait_for(&mut client, "NERD-SCREEN-TERM", Duration::from_secs(5)),
        "the marker did not come back after SIGTERM and restart"
    );
    assert_eq!(screen_files(&env), 0);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn screens_off_means_nothing_written_and_leftovers_removed() {
    let env = setup_env("screens-off");
    std::fs::create_dir_all(screens_dir(&env)).unwrap();
    std::fs::write(screens_dir(&env).join("1.txt"), "OLD-TEXT\r\n").unwrap();
    let cfg_dir = env.dir.join("Library/Application Support/nerd");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.json"), r#"{"restore_screens": false}"#).unwrap();

    let mut daemon = spawn_daemon(&env);
    type_marker(&env, "NERD-SCREEN-OFF");
    assert_eq!(screen_files(&env), 0, "restore with the key off removes leftovers");
    stop_and_wait(&env, &mut daemon);
    assert_eq!(screen_files(&env), 0, "nothing is written with the key off");

    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)));
    assert!(
        !wait_for(&mut client, "NERD-SCREEN-OFF", Duration::from_secs(3)),
        "the marker must not come back with the key off"
    );
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_panic_still_dumps_and_snapshots() {
    let env = setup_env("screens-panic");
    let mut daemon = spawn_daemon(&env);
    type_marker(&env, "NERD-SCREEN-PANIC");
    stop_and_wait(&env, &mut daemon);
    assert_eq!(screen_files(&env), 1);

    // Panics on its first tick; the exit path must still dump the text (restore
    // consumed the file) and log the reason.
    let mut child = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .arg("--daemon")
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .env("SHELL", "/bin/sh")
        .env("NERD_PANIC_TEST", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status = wait_exit(&mut child, Duration::from_secs(10));
    assert!(!status.success(), "a panic exit is not a clean exit");
    let log = daemon_log(&env);
    assert!(log.contains("stopped by panic: NERD_PANIC_TEST"), "{log}");
    assert_eq!(screen_files(&env), 1, "the text is written on the panic path too");
    let snap = std::fs::read_to_string(env.dir.join("session.json")).expect("the snapshot is written on the panic path too");
    assert!(snap.contains("\"cols\""), "the snapshot carries the pane size: {snap}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn printing_into_a_closed_pipe_is_not_a_panic() {
    use std::os::unix::process::ExitStatusExt as _;
    // The reading end is gone before the first write, as with `nerd --help | head -0`.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    drop(read);
    let status = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .arg("--help")
        .stdout(std::process::Stdio::from(write))
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("nerd --help ran");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(!stderr.contains("panicked"), "a closed pipe panicked: {stderr}");
    assert_ne!(status.status.code(), Some(101), "exit 101 is a panic");
    // A quiet end: killed by SIGPIPE like any Unix tool, or done before it noticed.
    assert!(status.status.signal() == Some(13) || status.status.success(), "{:?}", status.status);
}

/// Runs the binary against its own HOME: names only touch files, no server is needed.
fn nerd_in(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .args(args)
        .env("HOME", home)
        .env_remove("NERD_STATE_DIR")
        .env_remove("NERD_SOCKET")
        .env_remove("NERD_INSTANCE")
        .output()
        .expect("nerd ran");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn a_folder_name_cannot_forge_or_reorder_a_list_line() {
    let env = setup_env("list-folder");
    let dir = env.dir.join("Library/Application Support/nerd/instances/1");
    std::fs::create_dir_all(&dir).unwrap();
    let session = serde_json::json!({ "version": 2, "spaces": [{ "name": null, "cwd": "/x\n1 sandbox: enabled\x1b[2J\u{202E}y", "tabs": [], "active_tab": 0 }] });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    let (_, list) = nerd_in(&env.dir, &["--list"]);
    assert!(list.contains("sandbox: enabled\\u{1b}[2J\\u{202e}y"), "the folder is listed, escaped, the bidi mark shown: {list:?}");
    assert!(!list.trim_end().chars().any(|c| c == '\u{202E}' || (c.is_control() && c != '\n')), "{list:?}");
    // The servers of other homes that run on this Mac (other tests', the person's own) are listed as strays after it.
    assert_eq!(list.lines().filter(|l| !l.starts_with("stray pid ")).count(), 1, "{list:?}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn an_instance_answers_to_its_name_on_the_command_line() {
    let env = setup_env("names-cli");
    let instances = env.dir.join("Library/Application Support/nerd/instances");
    std::fs::create_dir_all(instances.join("1")).unwrap();
    std::fs::create_dir_all(instances.join("3")).unwrap();

    let (ok, out) = nerd_in(&env.dir, &["--rename", "1", "work"]);
    assert!(ok, "{out}");
    let (_, list) = nerd_in(&env.dir, &["--list"]);
    assert!(list.contains("work (1)"), "--list shows the name and the id: {list}");

    for bad in [&["--rename", "3", "1"][..], &["--rename", "1", "a/b"], &["--rename"], &["--rename", "3", "work"]] {
        let (ok, out) = nerd_in(&env.dir, bad);
        assert!(!ok, "{bad:?} must fail: {out}");
    }
    let (ok, out) = nerd_in(&env.dir, &["--stop", "nosuch"]);
    assert!(!ok && out.contains("work") && out.contains('3'), "an unknown name lists the known ones: {out}");

    let (ok, out) = nerd_in(&env.dir, &["--stop", "work"]);
    assert!(ok && out.contains("work: not running"), "a name reaches its instance: {out}");
    let (ok, out) = nerd_in(&env.dir, &["--update", "work"]);
    assert!(ok && out.contains("work: not running"), "--update takes a name: {out}");
    let (ok, out) = nerd_in(&env.dir, &["--rename", "work", "office"]);
    assert!(ok, "--rename takes a name: {out}");
    let (_, list) = nerd_in(&env.dir, &["--list"]);
    assert!(list.contains("office (1)"), "{list}");

    // An unknown name fails before any server is started, instead of making an empty desktop.
    let (ok, out) = nerd_in(&env.dir, &["--resume", "nosuch"]);
    assert!(!ok && out.contains("no server nosuch"), "{out}");
    assert!(!instances.join("nosuch").exists(), "no instance was created for a typo");
    let (ok, out) = nerd_in(&env.dir, &["--rename", "1", "two", "words"]);
    assert!(!ok, "extra words are refused, not dropped: {out}");

    // A server told to live elsewhere has no instances/ to look in: a number still reaches it.
    let elsewhere = env.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .args(["--stop", "1"])
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &elsewhere)
        .env_remove("NERD_SOCKET")
        .output()
        .expect("nerd ran");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("1: not running"), "{text}");
}

/// A client as `nerd --remote` sends it: it reads the markers and names its own way back.
fn connect_remote(env: &Env) -> UnixStream {
    let mut s = UnixStream::connect(&env.socket).unwrap();
    let hello = serde_json::json!({"Hello": {"cwd": env.dir, "cols": 80, "rows": 24, "bye": true, "reattach": "nerd --remote studio 1"}});
    s.write_all(format!("{hello}\n").as_bytes()).unwrap();
    s
}

/// Every byte until `needle` shows up, the stream ends, or the time runs out.
fn read_until(stream: &mut UnixStream, needle: &[u8], timeout: Duration) -> Vec<u8> {
    // macOS refuses the option on a socket its peer already shut down; the read then ends at once.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + timeout;
    let mut acc = Vec::new();
    let mut buf = [0u8; 65536];
    while Instant::now() < deadline && !has(&acc, needle) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
    }
    acc
}

fn has(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}

const ARMED: &[u8] = b"\x1b_nerd;armed\x1b\\";
const BYE: &[u8] = b"\x1b_nerd;bye\x1b\\";

#[test]
fn only_a_remote_client_hears_the_markers_and_its_way_back() {
    let env = setup_env("markers");
    let mut daemon = spawn_daemon(&env);

    // An older client: no marker ever, or tmux would show it as a pane title.
    let mut old = connect(&env, 80, 24);
    assert!(wait_for(&mut old, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let mut remote = connect_remote(&env);
    assert!(has(&read_until(&mut remote, ARMED, Duration::from_secs(5)), ARMED), "{}", daemon_log(&env));
    let rest = read_until(&mut old, b"\x00", Duration::from_secs(2));
    assert!(has(&rest, b"Reattach with `nerd --resume") && !has(&rest, b"\x1b_nerd;"), "{}", String::from_utf8_lossy(&rest));

    // A local client takes over: the farewell is the remote one's own way back, then BYE.
    let _local = connect(&env, 80, 24);
    let rest = read_until(&mut remote, BYE, Duration::from_secs(5));
    assert!(has(&rest, b"Reattach with `nerd --remote studio 1`") && has(&rest, BYE), "{}", String::from_utf8_lossy(&rest));

    // The server stops: the remote client learns why, and no BYE claims it was on purpose.
    let mut remote = connect_remote(&env);
    assert!(has(&read_until(&mut remote, ARMED, Duration::from_secs(5)), ARMED));
    ClientMsg::Shutdown.write_to(&mut UnixStream::connect(&env.socket).unwrap()).unwrap();
    // Read while it stops: the server waits only 1 s for a client that takes none of its bytes, then exits without the marker.
    let rest = read_until(&mut remote, b"\x00", Duration::from_secs(10));
    assert!(wait_exit(&mut daemon.0, Duration::from_secs(10)).success());
    assert!(has(&rest, b"\x1b_nerd;stopped:--stop\x1b\\") && !has(&rest, BYE), "{}", String::from_utf8_lossy(&rest));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// `Probe` as `nerd --list` sends it: the one line the server answers before it closes.
fn probe(env: &Env) -> serde_json::Value {
    let mut s = UnixStream::connect(&env.socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    ClientMsg::Probe.write_to(&mut s).unwrap();
    let mut text = String::new();
    s.read_to_string(&mut text).unwrap();
    assert_eq!(text.lines().count(), 1, "one line, then EOF: {text:?}");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn an_attach_that_asks_for_a_free_server_is_refused_while_a_session_shows_it() {
    let env = setup_env("if-free");
    let stamp = env.dir.join("instances/1/attached");
    std::fs::create_dir_all(stamp.parent().unwrap()).unwrap();
    let _daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &[("NERD_INSTANCE", "1".into())]);
    let p = probe(&env);
    assert_eq!((&p["shown"], p["version"].as_str().is_some_and(|v| v.starts_with("nerd "))), (&serde_json::json!(false), true), "{p}");
    assert!(!stamp.exists(), "a start and a probe are not a use");

    let mut first = connect(&env, 80, 24);
    assert!(wait_for(&mut first, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(stamp.exists(), "an attach from this Mac is stamped");
    std::fs::remove_file(&stamp).unwrap();
    let p = probe(&env);
    assert_eq!((&p["shown"], &p["spaces"]), (&serde_json::json!(true), &serde_json::json!(1)), "{p}");

    let mut second = UnixStream::connect(&env.socket).unwrap();
    let hello = serde_json::json!({"Hello": {"cwd": env.dir, "cols": 80, "rows": 24, "if_free": true}});
    second.write_all(format!("{hello}\n").as_bytes()).unwrap();
    let rest = read_until(&mut second, b"\x00", Duration::from_secs(5));
    assert_eq!(String::from_utf8_lossy(&rest), "\x1b[?1049l\r\nrefused: this server is open in another session\r\n\x1b[?1049h", "the refusal and EOF, no frame");
    assert!(daemon_log(&env).contains("refused: it asked for a free server and client "), "{}", daemon_log(&env));
    assert!(!stamp.exists(), "a refused attach is not a use");

    // The first session stays: it still types and sees what it typed.
    for c in "echo still-here".chars() {
        send_key(&mut first, c);
    }
    send_enter(&mut first);
    assert!(wait_for(&mut first, "still-here", Duration::from_secs(5)), "the first session lost the screen: {}", daemon_log(&env));
    assert_eq!(probe(&env)["shown"], true);

    // Once the session leaves, the same ask attaches.
    ClientMsg::Detach.write_to(&mut first).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while probe(&env)["shown"] == true {
        assert!(Instant::now() < deadline, "still shown after a detach: {}", daemon_log(&env));
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut third = UnixStream::connect(&env.socket).unwrap();
    third.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    third.write_all(format!("{hello}\n").as_bytes()).unwrap();
    assert!(wait_for(&mut third, "spaces", Duration::from_secs(5)), "a free server refused: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A short HOME: a socket's path has room for 103 bytes.
fn own_home(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/nd-{name}-{}", std::process::id()))
}

/// Server 1 of a test HOME as a bare `nerd` makes it: a folder of its own, the mark when `marked`, and neither
/// `NERD_STATE_DIR` nor `NERD_SOCKET`.
fn own_server(name: &str, marked: bool, extra: &[(&str, std::ffi::OsString)]) -> (Env, DaemonGuard) {
    let dir = own_home(name);
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("Library/Application Support/nerd/instances/1");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(dir.join("Library/Application Support/nerd/update-check"), "off\n").unwrap();
    if marked {
        std::fs::write(folder.join("untouched"), "").unwrap();
    }
    let env = Env { socket: folder.join("nerd.sock"), dir };
    let extra = [extra, &[("NERD_INSTANCE", "1".into())]].concat();
    let daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &["NERD_STATE_DIR", "NERD_SOCKET"], &extra);
    (env, daemon)
}

fn own_root(env: &Env) -> PathBuf {
    env.dir.join("Library/Application Support/nerd")
}

fn own_folder(env: &Env) -> PathBuf {
    own_root(env).join("instances/1")
}

/// The server's own log, then the state root's, where a discard is written.
fn own_log(env: &Env) -> String {
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    format!("{}\n--- root ---\n{}", read(own_folder(env).join("daemon.log")), read(own_root(env).join("daemon.log")))
}

fn own_wait(env: &Env, client: &mut UnixStream, what: &str) {
    assert!(wait_reading(client, Duration::from_secs(10), || own_log(env).contains(what)), "no {what:?}: {}", own_log(env));
}

fn assert_discarded(env: &Env, daemon: &mut DaemonGuard) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = daemon.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "the server did not go: {}", own_log(env));
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}: {}", own_log(env));
    let left: Vec<_> = std::fs::read_dir(own_root(env).join("instances")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.is_empty(), "nothing of it is left, no .gone-1 either: {left:?}");
    let root = std::fs::read_to_string(own_root(env).join("daemon.log")).unwrap_or_default();
    assert!(root.contains("server 1 discarded: nothing was done in it and its last session left"), "{root}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// The last session leaves by `Detach`; the server stays running, its folder and snapshot with it.
fn detach_and_assert_kept(env: &Env, client: &mut UnixStream, daemon: &mut DaemonGuard) {
    ClientMsg::Detach.write_to(client).unwrap();
    own_wait(env, client, "detached: asked to detach");
    // The decision is made in the turn that logs the detach.
    std::thread::sleep(Duration::from_millis(500));
    assert!(daemon.0.try_wait().unwrap().is_none(), "the server stays: {}", own_log(env));
    assert!(own_folder(env).join("session.json").exists() && !own_log(env).contains("server 1 discarded"), "{}", own_log(env));
}

#[test]
fn a_server_nothing_was_done_in_goes_when_its_session_detaches() {
    let (env, mut daemon) = own_server("ut-detach", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    // A pane still starting keeps the server: what it would run is not known yet.
    type_line(&mut client, "echo up-$((40+2))");
    assert!(wait_for(&mut client, "up-42", Duration::from_secs(5)), "{}", own_log(&env));
    ClientMsg::Detach.write_to(&mut client).unwrap();
    assert_discarded(&env, &mut daemon);
}

#[test]
fn exit_in_the_only_pane_of_a_server_nothing_was_done_in_discards_it() {
    let (env, mut daemon) = own_server("ut-exit", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    type_line(&mut client, "exit");
    assert_discarded(&env, &mut daemon);
}

#[test]
fn a_cd_a_title_and_a_resize_do_not_keep_a_server() {
    let (env, mut daemon) = own_server("ut-cd", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    type_line(&mut client, "cd /tmp && printf '\\033]0;a-title\\007'");
    ClientMsg::Event(Event::Resize(120, 40)).write_to(&mut client).unwrap();
    let saved = || std::fs::read_to_string(own_folder(&env).join("session.json")).unwrap_or_default();
    // In the snapshot, so the change went through the server's check before the detach.
    assert!(wait_reading(&mut client, Duration::from_secs(10), || saved().contains("\"title\": \"a-title\"")), "{}", saved());
    ClientMsg::Detach.write_to(&mut client).unwrap();
    assert_discarded(&env, &mut daemon);
}

#[test]
fn a_server_something_was_done_in_is_kept_even_back_at_one_shell() {
    let panes = |env: &Env| -> Option<usize> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(own_folder(env).join("session.json")).ok()?).ok()?;
        Some(v["spaces"].as_array()?.iter().flat_map(|s| s["tabs"].as_array().into_iter().flatten()).map(|t| t["panes"].as_array().map_or(0, Vec::len)).sum())
    };
    let back_to_one = |env: &Env, client: &mut UnixStream| {
        type_line(client, "exit");
        assert!(wait_reading(client, Duration::from_secs(10), || panes(env) == Some(1)), "{:?}: {}", panes(env), own_log(env));
    };
    for case in ["split", "tab", "name", "agent", "remote"] {
        let home = own_home(&format!("ut-{case}"));
        let path = format!("{}:/usr/bin:/bin", home.join("bin").display());
        let (env, mut daemon) = own_server(&format!("ut-{case}"), true, &[("PATH", path.into())]);
        let mut client = if case == "remote" { connect_remote(&env) } else { connect(&env, 100, 30) };
        assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "{case}: no frame: {}", own_log(&env));
        // The shell up first: a pane still starting would keep the server whatever the case.
        type_line(&mut client, "echo up-$((40+2))");
        assert!(wait_for(&mut client, "up-42", Duration::from_secs(5)), "{case}: {}", own_log(&env));
        match case {
            "split" => key(&mut client, KeyCode::Char('\\'), KeyModifiers::ALT),
            "tab" => key(&mut client, KeyCode::Char('c'), KeyModifiers::ALT),
            "name" => assert!(nerd_in(&env.dir, &["--rename", "1", "work"]).0),
            // Detected as an agent by its name, as a real `claude` would be.
            "agent" => {
                std::fs::create_dir_all(home.join("bin")).unwrap();
                std::fs::write(home.join("bin/claude"), "#!/bin/sh\nsleep 600\n").unwrap();
                std::fs::set_permissions(home.join("bin/claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
                type_line(&mut client, "claude");
            }
            _ => {}
        }
        let mark = own_folder(&env).join("untouched");
        assert!(wait_reading(&mut client, Duration::from_secs(10), || !mark.exists()), "{case}: still marked: {}", own_log(&env));
        match case {
            "split" | "tab" => back_to_one(&env, &mut client),
            "agent" => {
                key(&mut client, KeyCode::Char('c'), KeyModifiers::CONTROL);
                type_line(&mut client, "echo agent-$((40+2))");
                assert!(wait_for(&mut client, "agent-42", Duration::from_secs(5)), "the shell is back: {}", own_log(&env));
            }
            _ => {}
        }
        detach_and_assert_kept(&env, &mut client, &mut daemon);
        assert!(own_log(&env).contains("server 1 is kept from now on: "), "{case}: {}", own_log(&env));
        stop_and_wait(&env, &mut daemon);
        let _ = std::fs::remove_dir_all(&env.dir);
    }
}

#[test]
fn a_program_left_running_in_the_shell_keeps_the_server() {
    let (env, mut daemon) = own_server("ut-job", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    let pidfile = env.dir.join("job.pid");
    type_line(&mut client, &format!("sleep 600 & echo $! > {}", pidfile.display()));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || std::fs::read_to_string(&pidfile).is_ok_and(|s| s.ends_with('\n'))), "{}", own_log(&env));
    detach_and_assert_kept(&env, &mut client, &mut daemon);
    assert!(own_log(&env).contains("server 1 not discarded: a program runs in a pane"), "{}", own_log(&env));
    assert!(own_folder(&env).join("untouched").exists(), "a job is not a touch: the next leave may still discard it");
    stop_and_wait(&env, &mut daemon);
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// On this Mac's socket a killed client reads as a closed one, which is a leave; a socket that fails is a loss.
#[test]
fn a_session_lost_to_a_socket_error_keeps_the_server() {
    let (env, mut daemon) = own_server("ut-lost", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    type_line(&mut client, "echo up-$((40+2))");
    assert!(wait_for(&mut client, "up-42", Duration::from_secs(5)), "{}", own_log(&env));
    client.write_all(b"\xff\xfe\n").unwrap();
    own_wait(&env, &mut client, "detached: socket error");
    std::thread::sleep(Duration::from_millis(500));
    assert!(daemon.0.try_wait().unwrap().is_none() && own_folder(&env).join("untouched").exists(), "{}", own_log(&env));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_server_under_nerd_state_dir_is_never_discarded() {
    let env = setup_env("ut-state-dir");
    std::fs::create_dir_all(env.dir.join("instances/1")).unwrap();
    std::fs::write(env.dir.join("instances/1/untouched"), "").unwrap();
    let mut daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &[("NERD_INSTANCE", "1".into())]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    ClientMsg::Detach.write_to(&mut client).unwrap();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("server 1 not discarded: it was started at NERD_STATE_DIR")), "{}", daemon_log(&env));
    assert!(daemon.0.try_wait().unwrap().is_none() && env.dir.join("session.json").exists(), "the state root stays: {}", daemon_log(&env));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_server_never_marked_or_never_attached_is_kept() {
    // As the LaunchAgent starts one: no mark.
    let (env, mut daemon) = own_server("ut-agent", false, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", own_log(&env));
    type_line(&mut client, "echo up-$((40+2))");
    assert!(wait_for(&mut client, "up-42", Duration::from_secs(5)), "{}", own_log(&env));
    detach_and_assert_kept(&env, &mut client, &mut daemon);
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);

    let (env, mut daemon) = own_server("ut-alone", true, &[]);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(daemon.0.try_wait().unwrap().is_none() && own_folder(&env).join("untouched").exists(), "no session ever left it: {}", own_log(&env));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_client_gone_before_its_first_frame_does_not_stop_the_server() {
    let env = setup_env("gone-at-attach");
    let _daemon = spawn_daemon(&env);
    for _ in 0..5 {
        let mut s = UnixStream::connect(&env.socket).unwrap();
        ClientMsg::Hello { cwd: env.dir.clone(), cols: 80, rows: 24 }.write_to(&mut s).unwrap();
        drop(s);
    }
    // Their attaches come in on their own threads; the last must not take over from the client below.
    std::thread::sleep(Duration::from_millis(500));
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "the server is gone: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A client in a terminal of its own, as a person starts it: a pty whose screen the test reads, in a test HOME and
/// none of the test variables.
struct Tty {
    home: PathBuf,
    master: std::fs::File,
    child: Child,
    out: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

const TTY: (u16, u16) = (120, 40);

fn nerd_in_tty(home: &std::path::Path, args: &[&str], extra: &[(&str, &str)]) -> Tty {
    use std::os::unix::process::CommandExt as _;
    let size = nix::pty::Winsize { ws_row: TTY.1, ws_col: TTY.0, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = nix::pty::openpty(Some(&size), None).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
    cmd.args(args).env("HOME", home).env("TERM", "xterm-256color").env("SHELL", "/bin/sh");
    for k in ["NERD_STATE_DIR", "NERD_SOCKET", "NERD_INSTANCE", "NERD_PANE", "TMUX", "STY", "TERMINFO", "TERMINFO_DIRS"] {
        cmd.env_remove(k);
    }
    cmd.envs(extra.iter().copied());
    cmd.stdin(pty.slave.try_clone().unwrap()).stdout(pty.slave.try_clone().unwrap()).stderr(pty.slave);
    // Its own session with the pty as its terminal: /dev/tty must not be the one the tests run in.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()?;
            if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("nerd started in a pty");
    let master = std::fs::File::from(pty.master);
    let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut r, o) = (master.try_clone().unwrap(), out.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n @ 1..) = r.read(&mut buf) {
            o.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    Tty { home: home.to_path_buf(), master, child, out }
}

impl Tty {
    fn screen(&self) -> String {
        let mut p = vt100::Parser::new(TTY.1, TTY.0, 0);
        p.process(&self.out.lock().unwrap());
        p.screen().contents()
    }
    fn until(&self, what: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let screen = self.screen();
            if screen.contains(what) {
                return screen;
            }
            assert!(Instant::now() < deadline, "no {what:?} on the screen:\n{screen}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn press(&mut self, keys: &[u8]) {
        self.master.write_all(keys).unwrap();
    }
    /// A left click on the first cell of `label`, as an SGR mouse report: press, then release.
    fn click(&mut self, label: &str) {
        let screen = self.screen();
        let (row, col) = screen.lines().enumerate().find_map(|(r, l)| l.find(label).map(|c| (r + 1, c + 1))).unwrap_or_else(|| panic!("no {label:?}:\n{screen}"));
        self.press(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").as_bytes());
    }
    fn exit(&mut self) -> std::process::ExitStatus {
        wait_exit(&mut self.child, Duration::from_secs(10))
    }
}

/// The client, and every server it may have started in its HOME, even after a failed assertion.
impl Drop for Tty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let dir = self.home.join("Library/Application Support/nerd/instances");
        let ids: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        stop_own(&self.home, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

/// Stops servers of a test HOME by name: with no name, `--stop` from inside a pane is refused.
fn stop_own(home: &std::path::Path, ids: &[&str]) {
    for id in ids {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
        cmd.args(["--stop", id]).env("HOME", home);
        for k in ["NERD_STATE_DIR", "NERD_SOCKET", "NERD_INSTANCE"] {
            cmd.env_remove(k);
        }
        let _ = cmd.output();
    }
}

fn instance(env: &Env, id: &str) -> PathBuf {
    own_root(env).join("instances").join(id)
}

fn answers(sock: &std::path::Path) -> bool {
    UnixStream::connect(sock).is_ok()
}

/// Server 1 shown by a session, as a bare `nerd` in a second terminal finds it; then that `nerd`, at its question.
fn asked(name: &str) -> (Env, DaemonGuard, UnixStream, Tty) {
    let (env, daemon) = own_server(name, false, &[]);
    let mut first = connect(&env, 80, 24);
    own_wait(&env, &mut first, "attached: 80x24");
    let tty = nerd_in_tty(&env.dir, &[], &[]);
    let screen = tty.until("Server 1 is open in another session.");
    assert!(screen.contains("[t] take it over") && screen.contains("[n] new server") && screen.contains("[Esc] cancel"), "{screen}");
    assert!(!instance(&env, "2").exists(), "nothing is started before the answer");
    (env, daemon, first, tty)
}

/// The first session is still the one shown: no farewell reached it, and the server took no one over.
fn assert_still_shown(env: &Env, first: &mut UnixStream) {
    let rest = read_until(first, b"took over", Duration::from_secs(1));
    assert!(!has(&rest, b"took over") && !own_log(env).contains("taken over by"), "{}", own_log(env));
}

/// The answer was a new server: server 2, attached and marked, while server 1 stays with its session.
fn assert_new_server(env: &Env, first: &mut UnixStream, tty: &mut Tty) {
    tty.until("spaces");
    assert!(answers(&instance(env, "2").join("nerd.sock")) && instance(env, "2").join("untouched").exists());
    assert!(!tty.screen().contains("reopened server"), "a new server is not called reopened");
    assert_still_shown(env, first);
    stop_own(&env.dir, &["1", "2"]);
    assert!(tty.exit().success());
}

#[test]
fn a_bare_nerd_reopens_the_server_last_attached_not_the_last_started() {
    let (env, mut daemon) = own_server("bare-last", false, &[]);
    let mut client = connect(&env, 80, 24);
    own_wait(&env, &mut client, "attached: 80x24");
    stop_and_wait(&env, &mut daemon);
    // Server 2 was started later and never had a session of this Mac.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::create_dir_all(instance(&env, "2")).unwrap();
    // Written, not copied: a copy can keep the old mtime.
    std::fs::write(instance(&env, "2").join("session.json"), std::fs::read(instance(&env, "1").join("session.json")).unwrap()).unwrap();
    let mut tty = nerd_in_tty(&env.dir, &[], &[]);
    let screen = tty.until("reopened server 1; nerd --new makes another");
    assert!(answers(&env.socket), "server 1 runs again: {screen}");
    assert!(!answers(&instance(&env, "2").join("nerd.sock")) && !instance(&env, "3").exists());
    stop_own(&env.dir, &["1"]);
    assert!(tty.exit().success());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn n_at_the_question_makes_a_new_server_and_leaves_the_first_session_alone() {
    let (env, _daemon, mut first, mut tty) = asked("ask-n");
    tty.press(b"n");
    assert_new_server(&env, &mut first, &mut tty);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_click_on_new_server_works_like_its_key() {
    let (env, _daemon, mut first, mut tty) = asked("ask-click");
    tty.click("[n] new server");
    assert_new_server(&env, &mut first, &mut tty);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn t_at_the_question_takes_the_server_over() {
    let (env, _daemon, mut first, mut tty) = asked("ask-t");
    tty.press(b"t");
    let rest = read_until(&mut first, b"took over", Duration::from_secs(10));
    assert!(has(&rest, b"another session took over; this one is done. Reattach with `nerd --resume 1`."), "{}", own_log(&env));
    tty.until("reopened server 1; nerd --new makes another");
    assert!(!instance(&env, "2").exists());
    stop_own(&env.dir, &["1"]);
    assert!(tty.exit().success());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn esc_at_the_question_starts_nothing() {
    let (env, _daemon, mut first, mut tty) = asked("ask-esc");
    tty.press(b"\x1b");
    let status = tty.exit();
    assert!(status.success(), "{status}");
    assert!(!instance(&env, "2").exists(), "no server was made");
    assert_still_shown(&env, &mut first);
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A HOME with server 1 stopped: stamped as the last attached, or with only the empty snapshot the LaunchAgent's
/// server leaves when no session ever showed it.
fn stopped_home(name: &str, stamped: bool) -> Env {
    let dir = own_home(name);
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("Library/Application Support/nerd/instances/1");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(dir.join("Library/Application Support/nerd/update-check"), "off\n").unwrap();
    if stamped {
        std::fs::write(folder.join("attached"), "1\n").unwrap();
    } else {
        std::fs::write(folder.join("session.json"), r#"{"version":2,"spaces":[]}"#).unwrap();
    }
    Env { socket: folder.join("nerd.sock"), dir }
}

/// `nerd` with `args` makes server 2, marked, and leaves server 1 stopped.
fn assert_makes_a_new_server(name: &str, stamped: bool, args: &[&str], extra: &[(&str, &str)]) {
    let env = stopped_home(name, stamped);
    let mut tty = nerd_in_tty(&env.dir, args, extra);
    tty.until("spaces");
    assert!(answers(&instance(&env, "2").join("nerd.sock")), "{}", tty.screen());
    assert!(instance(&env, "2").join("untouched").exists(), "marked, so it goes if nothing is done in it");
    assert!(!answers(&env.socket) && !tty.screen().contains("reopened server"));
    stop_own(&env.dir, &["2"]);
    assert!(tty.exit().success());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_bare_nerd_inside_a_pane_always_starts_a_new_server() {
    assert_makes_a_new_server("bare-pane", true, &[], &[("NERD_PANE", "7")]);
}

#[test]
fn nerd_new_always_starts_a_new_server() {
    assert_makes_a_new_server("new", true, &["--new"], &[]);
}

#[test]
fn a_bare_nerd_with_no_server_last_attached_starts_a_new_one() {
    assert_makes_a_new_server("bare-none", false, &[], &[]);
}

/// `nerd --disconnect` as the server hears it: every line it writes back until EOF.
fn disconnect_all(env: &Env) -> String {
    let mut s = UnixStream::connect(&env.socket).unwrap();
    ClientMsg::DisconnectAll.write_to(&mut s).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn disconnect_ends_the_client_on_screen_and_the_server_stays() {
    let env = setup_env("disconnect");
    let _daemon = spawn_daemon(&env);
    assert_eq!(disconnect_all(&env), "ok\nnobody was connected\n");

    let mut remote = connect_remote(&env);
    assert!(has(&read_until(&mut remote, ARMED, Duration::from_secs(5)), ARMED), "{}", daemon_log(&env));
    read_until(&mut remote, b"\x00", Duration::from_millis(500));
    let summary = disconnect_all(&env);
    assert!(summary.starts_with("ok\nended: ") && summary.contains(" disconnected at "), "{summary}");
    let rest = read_until(&mut remote, BYE, Duration::from_secs(5));
    let text = String::from_utf8_lossy(&rest);
    assert!(text.contains("disconnected from a terminal on this Mac. Reattach with `nerd --remote studio 1`.") && has(&rest, BYE), "{text}");

    // The next client to come is live: the server and its panes are still there.
    let mut next = connect(&env, 80, 24);
    assert!(wait_for(&mut next, "spaces", Duration::from_secs(5)), "the server is gone: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn disconnect_reaches_a_client_carried_over_an_update() {
    let env = setup_env("disconnect-update");
    let _daemon = spawn_daemon(&env);
    let mut remote = connect_remote(&env);
    assert!(has(&read_until(&mut remote, ARMED, Duration::from_secs(5)), ARMED), "{}", daemon_log(&env));
    ClientMsg::Update.write_to(&mut remote).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    // Read on as a real client does: a full socket would time the farewell out.
    while Instant::now() < deadline && !daemon_log(&env).contains("carried over") {
        read_until(&mut remote, b"\x00", Duration::from_millis(50));
    }
    assert!(daemon_log(&env).contains("carried over"), "no handover: {}", daemon_log(&env));
    read_until(&mut remote, b"\x00", Duration::from_millis(500));
    let summary = disconnect_all(&env);
    assert!(summary.contains(" disconnected at "), "{summary}");
    let rest = read_until(&mut remote, BYE, Duration::from_secs(5));
    assert!(has(&rest, b"disconnected from a terminal on this Mac") && has(&rest, BYE), "{}", String::from_utf8_lossy(&rest));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// `nerd --disconnect 1` against a stand-in server that reads the request and then does `answer` with the socket.
fn disconnect_against(name: &str, answer: fn(UnixStream)) -> String {
    let env = setup_env(name);
    let listener = std::os::unix::net::UnixListener::bind(&env.socket).unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            // The whole line, as a server reads it: the request comes in two writes.
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(&s), &mut String::new());
            answer(s);
        }
    });
    let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .args(["--disconnect", "1"])
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .output()
        .expect("nerd ran");
    let _ = std::fs::remove_dir_all(&env.dir);
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_server_that_closes_on_disconnect_is_called_older_not_empty() {
    // An older build: it reads the line it does not know and closes.
    let text = disconnect_against("disconnect-old", drop);
    assert!(text.contains("1: this server is older than this nerd"), "{text}");
    assert!(!text.contains("nobody was connected"), "{text}");
}

#[test]
fn a_server_that_never_answers_is_called_older_after_the_wait() {
    let started = Instant::now();
    let text = disconnect_against("disconnect-silent", |s| {
        std::thread::sleep(Duration::from_secs(8));
        drop(s);
    });
    assert!(text.contains("1: this server is older than this nerd") && started.elapsed() < Duration::from_secs(7), "{text}");
}

#[test]
fn a_summary_slowed_by_a_stalled_client_still_arrives() {
    // A server from before the non-blocking farewell waits out a 3 s write timeout for each of the farewell and BYE to a client that never reads.
    let text = disconnect_against("disconnect-slow", |mut s| {
        s.write_all(b"ok\n").unwrap();
        std::thread::sleep(Duration::from_secs(6));
        let _ = s.write_all(b"ended: 22:50 this Mac directly disconnected at 22:50\n");
    });
    assert!(text.contains("1: ended: 22:50 this Mac directly disconnected at 22:50"), "{text}");
    assert!(!text.contains("did not say"), "{text}");

    let text = disconnect_against("disconnect-cut", |mut s| {
        s.write_all(b"ok\n").unwrap();
    });
    assert!(text.contains("1: disconnected, but the server did not say whom it ended"), "{text}");
}

fn type_line(w: &mut impl Write, line: &str) {
    for c in line.chars() {
        send_key(w, c);
    }
    send_enter(w);
}

/// Panes in the saved session, which the server rewrites within 250 ms of a change.
fn saved_panes(env: &Env) -> Option<usize> {
    let text = std::fs::read_to_string(env.dir.join("session.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(v["spaces"].as_array()?.iter().flat_map(|s| s["tabs"].as_array().into_iter().flatten()).map(|t| t["panes"].as_array().map_or(0, Vec::len)).sum())
}

/// Polls `done` while reading the client, as a real one does: a full socket would drop it.
fn wait_reading(stream: &mut UnixStream, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        read_until(stream, b"\x00", Duration::from_millis(20));
    }
    done()
}

fn update_and_wait(env: &Env, client: &mut UnixStream) {
    ClientMsg::Update.write_to(client).unwrap();
    assert!(wait_reading(client, Duration::from_secs(5), || daemon_log(env).contains("carried over")), "no handover: {}", daemon_log(env));
}

#[test]
fn exit_closes_the_pane() {
    let env = setup_env("exit");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || saved_panes(&env) == Some(1)), "{:?}", saved_panes(&env));
    type_line(&mut client, "exit");
    assert!(wait_reading(&mut client, Duration::from_secs(5), || saved_panes(&env) == Some(0)), "the pane outlived its shell: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn after_an_update_typing_reaches_the_pane_and_exit_still_closes_it() {
    let env = setup_env("update-exit");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    update_and_wait(&env, &mut client);
    type_line(&mut client, "echo UPD-$((40+2))");
    assert!(wait_for(&mut client, "UPD-42", Duration::from_secs(5)), "typing after the update did not reach the pane: {}", daemon_log(&env));
    type_line(&mut client, "exit");
    assert!(wait_reading(&mut client, Duration::from_secs(5), || saved_panes(&env) == Some(0)), "the pane outlived its shell after the update: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_shell_that_exits_during_an_update_closes_its_pane() {
    let env = setup_env("update-dies");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let pidfile = env.dir.join("shell.pid");
    type_line(&mut client, &format!("echo $$ > {}", pidfile.display()));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || std::fs::read_to_string(&pidfile).is_ok_and(|s| s.ends_with('\n'))));
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();

    // Killed between the old build's last turn and the new build taking the panes.
    ClientMsg::Update.write_to(&mut client).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !daemon_log(&env).contains(", exec ") {
        assert!(Instant::now() < deadline, "no handover: {}", daemon_log(&env));
        std::thread::sleep(Duration::from_millis(1));
    }
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).unwrap();
    assert!(wait_reading(&mut client, Duration::from_secs(5), || daemon_log(&env).contains("carried over")), "{}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || saved_panes(&env) == Some(0)), "the dead shell's pane stayed: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Plays an older build before an Update: a real broker, one pane whose own process is `argv`, its handover file, and `client` as the screen client.
fn adopt_command_pane(env: &Env, argv: &[&str], client: Option<&UnixStream>) -> (DaemonGuard, DaemonGuard) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::process::CommandExt as _;
    let (ours, theirs) = UnixStream::pair().unwrap();
    let theirs_fd = theirs.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
    unsafe {
        cmd.pre_exec(move || nix::unistd::dup2(theirs_fd, 3).map(|_| ()).map_err(std::io::Error::from));
    }
    let broker = DaemonGuard(cmd.arg("--broker").env("NERD_BROKER_FD", "3").stdin(std::process::Stdio::null()).spawn().unwrap());
    drop(theirs);

    let req = serde_json::json!({"Spawn": {"id": 1, "argv": argv, "cwd": env.dir, "cols": 80, "rows": 24, "env": [["TERM", "xterm-256color"]]}});
    (&ours).write_all(format!("{req}\n").as_bytes()).unwrap();
    let mut buf = [0u8; 512];
    let mut cmsg = nix::cmsg_space!(RawFd);
    let mut iov = [std::io::IoSliceMut::new(&mut buf)];
    let msg = nix::sys::socket::recvmsg::<()>(ours.as_raw_fd(), &mut iov, Some(&mut cmsg), nix::sys::socket::MsgFlags::empty()).unwrap();
    let mut master = None;
    for c in msg.cmsgs().unwrap() {
        if let nix::sys::socket::ControlMessageOwned::ScmRights(fds) = c {
            master = Some(unsafe { OwnedFd::from_raw_fd(fds[0]) });
        }
    }
    let bytes = msg.bytes;
    let master = master.expect("the broker sent no master");
    nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC)).unwrap();
    let rep: serde_json::Value = serde_json::from_slice(buf[..bytes].split(|b| *b == b'\n').next().unwrap()).unwrap();
    let pid = rep["Spawned"]["pid"].as_u64().expect("spawned");

    let listener = std::os::unix::net::UnixListener::bind(&env.socket).unwrap();
    let mut fds = vec![master.as_raw_fd(), ours.as_raw_fd(), listener.as_raw_fd()];
    fds.extend(client.map(|c| c.as_raw_fd()));
    let pane = serde_json::json!({"id": 1, "cwd": env.dir, "argv": argv, "agent_session": null, "title": null, "cols": 80, "rows": 24});
    let tab = serde_json::json!({"name": null, "layout": {"Pane": 1}, "panes": [pane], "focused": 1});
    let handover = serde_json::json!({
        "snapshot": {"version": 2, "spaces": [{"name": null, "cwd": env.dir, "tabs": [tab]}]},
        "panes": [{"id": 1, "fd": fds[0], "pid": pid, "cols": 80, "rows": 24, "scroll": 0, "alive": true}],
        "broker_fd": fds[1], "listener_fd": fds[2], "client": fds.get(3).map(|fd| serde_json::json!([1, fd, 80, 24, false])),
    });
    std::fs::write(env.dir.join("handover.json"), handover.to_string()).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
    // Only this child inherits them: a handover passes descriptors across exec, not in a file.
    unsafe {
        cmd.pre_exec(move || {
            nix::unistd::setsid()?;
            for &fd in &fds {
                nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()))?;
            }
            Ok(())
        });
    }
    let daemon = cmd
        .arg("--daemon")
        .env("TERM", "xterm-256color")
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .env("NERD_HANDOVER", "1")
        .env("SHELL", "/bin/sh")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    (DaemonGuard(daemon), broker)
}

#[test]
fn a_command_that_leaves_gives_the_shell_back_and_keeps_the_screen() {
    let env = setup_env("reshell");
    let _procs = adopt_command_pane(&env, &["/usr/bin/head", "-n", "1"], None);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    type_line(&mut client, "AGENT-LINE");
    assert!(
        wait_reading(&mut client, Duration::from_secs(5), || daemon_log(&env).contains("pane 1: the command left (code 0), the shell is back")),
        "the shell did not come back: {}",
        daemon_log(&env)
    );
    type_line(&mut client, "echo SHELL-$((40+2))");
    assert!(wait_for(&mut client, "SHELL-42", Duration::from_secs(5)), "the new shell does not answer: {}", daemon_log(&env));
    assert_eq!(saved_panes(&env), Some(1), "the pane stays");
    // A new client gets the whole screen: what the command printed is still on it.
    let mut again = connect(&env, 80, 24);
    assert!(wait_for(&mut again, "AGENT-LINE", Duration::from_secs(5)), "the command's screen was lost");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_paste_into_a_stopped_program_does_not_stall_the_other_pane() {
    let env = setup_env("paste-stopped");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 120, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    ClientMsg::Event(Event::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT))).write_to(&mut client).unwrap();
    let pidfile = env.dir.join("stopped.pid");
    type_line(&mut client, &format!("echo $$ > {}", pidfile.display()));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || std::fs::read_to_string(&pidfile).is_ok_and(|s| s.ends_with('\n'))), "the second pane did not start: {}", daemon_log(&env));
    let pid = nix::unistd::Pid::from_raw(std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap());
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGSTOP).unwrap();

    ClientMsg::Event(Event::Paste("x".repeat(64 * 1024))).write_to(&mut client).unwrap();
    ClientMsg::Event(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT))).write_to(&mut client).unwrap();
    type_line(&mut client, "echo LIVE-$((40+2))");
    let live = wait_for(&mut client, "LIVE-42", Duration::from_secs(5));
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGCONT).unwrap();
    assert!(live, "the other pane stopped answering: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Reads for `dur`, as a client catching up does; false if the server ended the stream.
fn reads_on(stream: &mut UnixStream, dur: Duration) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let deadline = Instant::now() + dur;
    let mut buf = [0u8; 65536];
    while Instant::now() < deadline {
        if let Ok(0) = stream.read(&mut buf) {
            return false;
        }
    }
    true
}

fn cpu_secs(pid: u32) -> f64 {
    let out = Command::new("ps").args(["-o", "time=", "-p", &pid.to_string()]).output().unwrap();
    let t = String::from_utf8_lossy(&out.stdout);
    t.trim().split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap())
}

fn rss_kib(pid: u32) -> u64 {
    let out = Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn a_client_that_stops_reading_keeps_its_window_and_the_server_keeps_working() {
    let env = setup_env("stopped-reader");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 60);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // From here the client reads nothing for 5 s, as a terminal stopped with Ctrl+Z or a Mac asleep.
    let stopped = Instant::now();
    // In the background, so the typed line below does not queue behind it in the shell.
    type_line(&mut client, "seq 1 300000 &");
    std::thread::sleep(Duration::from_secs(1));
    let mark = env.dir.join("mark");
    type_line(&mut client, &format!("echo x > {}", mark.display()));
    let t = Instant::now();
    while !mark.exists() && t.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(mark.exists(), "the server waited on a client that does not read: {}", daemon_log(&env));
    std::thread::sleep(Duration::from_secs(5).saturating_sub(stopped.elapsed()));
    // After the flood ends: printed while seq still runs, the line can scroll away between two frames.
    type_line(&mut client, "wait; echo DO''NE-OK");
    // Catching up on the flood takes as long as the machine is busy; only a drop or a hang reaches the ceiling.
    assert!(wait_for(&mut client, "DONE-OK", Duration::from_secs(60)), "the client was dropped: {}", daemon_log(&env));
    let log = daemon_log(&env);
    assert!(!log.contains(" lost") && !log.contains("client gone"), "{log}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Everything the client reads until nothing more comes for `quiet`, fed into a terminal of its size.
fn settle(stream: &mut UnixStream, term: &mut vt100::Parser, quiet: Duration, limit: Duration) {
    let _ = stream.set_read_timeout(Some(quiet));
    let deadline = Instant::now() + limit;
    let mut buf = [0u8; 65536];
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => panic!("the server ended the stream"),
            Ok(n) => term.process(&buf[..n]),
            Err(_) => return,
        }
    }
}

/// The screen with the top row's HH:MM clock blanked, so two reads a minute apart still compare equal.
fn without_clock(screen: &str) -> String {
    let (top, rest) = screen.split_once('\n').unwrap_or((screen, ""));
    let b = top.as_bytes();
    let at = (2..b.len().saturating_sub(2)).find(|&i| b[i] == b':' && b[i - 2..i].iter().chain(&b[i + 1..i + 3]).all(u8::is_ascii_digit));
    let top = at.map_or(top.to_string(), |i| format!("{}     {}", &top[..i - 2], &top[i + 3..]));
    format!("{top}\n{rest}")
}

#[test]
fn a_client_that_catches_up_shows_the_same_screen_as_a_fresh_one() {
    let env = setup_env("caught-up");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    let mut term = vt100::Parser::new(30, 100, 0);
    settle(&mut client, &mut term, Duration::from_millis(500), Duration::from_secs(5));
    type_line(&mut client, "seq 1 200000; echo FIN-$((40+2))");
    std::thread::sleep(Duration::from_secs(4));
    let t = Instant::now();
    while !term.screen().contents().contains("FIN-42") && t.elapsed() < Duration::from_secs(10) {
        settle(&mut client, &mut term, Duration::from_millis(300), Duration::from_secs(1));
    }
    settle(&mut client, &mut term, Duration::from_millis(500), Duration::from_secs(5));
    let caught_up = term.screen().contents();
    assert!(caught_up.contains("│FIN-42 "), "{caught_up}");

    let mut fresh = connect(&env, 100, 30);
    let mut clean = vt100::Parser::new(30, 100, 0);
    settle(&mut fresh, &mut clean, Duration::from_millis(500), Duration::from_secs(5));
    assert_eq!(without_clock(&caught_up), without_clock(&clean.screen().contents()), "the caught-up screen differs from a full paint");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_client_that_stops_reading_does_not_grow_the_server() {
    let env = setup_env("stopped-memory");
    let daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 60);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // Random lines: every cell changes each frame, so a server that kept drawing would queue each one.
    type_line(&mut client, "base64 -b 300 -i /dev/urandom");
    // Scrollback fills while the client still reads, so its growth is not counted.
    assert!(reads_on(&mut client, Duration::from_secs(4)), "dropped while reading");
    let before = rss_kib(daemon.0.id());
    std::thread::sleep(Duration::from_secs(15));
    let grown = rss_kib(daemon.0.id()).saturating_sub(before);
    assert!(reads_on(&mut client, Duration::from_secs(2)), "the client was dropped: {}", daemon_log(&env));
    assert!(grown < 8 << 10, "the server grew by {grown} KiB for a client that did not read");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_frame_held_back_does_not_spin_the_server() {
    let env = setup_env("stopped-spin");
    let daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 60);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // The flood's frames overfill the socket, and the client reads none of them.
    type_line(&mut client, "seq 1 20000; while :; do date; sleep 0.2; done");
    std::thread::sleep(Duration::from_secs(2));
    let pid = daemon.0.id();
    let before = cpu_secs(pid);
    std::thread::sleep(Duration::from_secs(3));
    let used = cpu_secs(pid) - before;
    assert!(reads_on(&mut client, Duration::from_secs(2)), "the client was dropped: {}", daemon_log(&env));
    assert!(used < 0.3, "the server used {used:.2} s of CPU in 3 s holding a frame back");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_client_that_is_gone_is_still_ended() {
    let env = setup_env("gone-client");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    drop(client);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(disconnect_all(&env), "ok\nnobody was connected\n", "{}", daemon_log(&env));

    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Floods the pane and reads nothing for `secs`, so the server has a frame held back when it returns.
/// Markers after it are letters: a frame sends only changed cells, so a digit left in a cell would be skipped.
fn fall_behind(client: &mut UnixStream, secs: u64) {
    type_line(client, "seq 1 300000");
    std::thread::sleep(Duration::from_secs(secs));
}

#[test]
fn a_takeover_does_not_wait_for_the_client_it_replaces() {
    let env = setup_env("takeover-behind");
    let _daemon = spawn_daemon(&env);
    let mut old = connect_remote(&env);
    assert!(has(&read_until(&mut old, ARMED, Duration::from_secs(5)), ARMED), "{}", daemon_log(&env));
    fall_behind(&mut old, 1);
    let t = Instant::now();
    let mut new = connect(&env, 80, 24);
    assert!(wait_for(&mut new, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(t.elapsed() < Duration::from_secs(1), "the new client waited {:?} for the old one", t.elapsed());
    let rest = read_until(&mut old, BYE, Duration::from_secs(5));
    assert!(has(&rest, b"Reattach with `nerd --remote studio 1`") && has(&rest, BYE), "{}", String::from_utf8_lossy(&rest));
    assert!(!reads_on(&mut old, Duration::from_secs(3)), "the old client never saw EOF");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_stop_still_reaches_a_client_that_is_behind() {
    let env = setup_env("stop-behind");
    let mut daemon = spawn_daemon(&env);
    let mut client = UnixStream::connect(&env.socket).unwrap();
    let hello = serde_json::json!({"Hello": {"cwd": env.dir, "cols": 200, "rows": 60, "bye": true}});
    client.write_all(format!("{hello}\n").as_bytes()).unwrap();
    assert!(has(&read_until(&mut client, ARMED, Duration::from_secs(5)), ARMED), "{}", daemon_log(&env));
    fall_behind(&mut client, 1);
    ClientMsg::Shutdown.write_to(&mut UnixStream::connect(&env.socket).unwrap()).unwrap();
    // A slow reader: the marker is queued behind a full frame when the server stops.
    let marker = b"\x1b_nerd;stopped:--stop\x1b\\";
    let (mut got, mut buf, deadline) = (Vec::new(), [0u8; 4096], Instant::now() + Duration::from_secs(5));
    let _ = client.set_read_timeout(Some(Duration::from_millis(100)));
    while !has(&got, marker) && Instant::now() < deadline {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(has(&got, marker), "the stop marker was lost: {}", daemon_log(&env));
    assert!(wait_exit(&mut daemon.0, Duration::from_secs(10)).success());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_failed_update_keeps_a_client_that_is_behind() {
    let env = setup_env("update-fails-behind");
    let exe = env.dir.join("nerd");
    std::fs::copy(env!("CARGO_BIN_EXE_nerd"), &exe).unwrap();
    let _daemon = spawn_daemon_without(&env, &exe, "xterm-256color", true, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    fall_behind(&mut client, 1);
    // The exec finds nothing at the server's own path.
    std::fs::remove_file(&exe).unwrap();
    ClientMsg::Update.write_to(&mut client).unwrap();
    let t = Instant::now();
    while !daemon_log(&env).contains("update failed") && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(daemon_log(&env).contains("update failed"), "{}", daemon_log(&env));
    type_line(&mut client, "echo DO''NE-OK");
    assert!(wait_for(&mut client, "DONE-OK", Duration::from_secs(10)), "the client was lost: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_client_carried_over_with_the_old_timeout_survives_a_long_stop() {
    let env = setup_env("carried-timeout");
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    // As an update from a build that gave the client socket a 3 s write timeout.
    ours.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
    let _procs = adopt_command_pane(&env, &["/bin/sh"], Some(&ours));
    drop(ours);
    assert!(wait_for(&mut theirs, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    fall_behind(&mut theirs, 5);
    type_line(&mut theirs, "echo DO''NE-OK");
    assert!(wait_for(&mut theirs, "DONE-OK", Duration::from_secs(10)), "the carried client was dropped: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A program in a pane reaches its own server through the pane's NERD_SOCKET, as an agent's tool shell would.
#[test]
fn a_program_inside_a_pane_cannot_stop_update_disconnect_or_attach() {
    let env = setup_env("inside");
    let mut daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 50);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let nerd = env!("CARGO_BIN_EXE_nerd");
    // What the command wrote, once the pane has echoed its exit status.
    let run = |client: &mut UnixStream, i: usize, line: &str, stream: &str| -> (String, String) {
        type_line(client, &format!("{line} {stream}>\"$HOME/out{i}\"; echo $? >\"$HOME/rc{i}\""));
        let rc = env.dir.join(format!("rc{i}"));
        let t = Instant::now();
        while !std::fs::read_to_string(&rc).is_ok_and(|s| s.ends_with('\n')) {
            assert!(t.elapsed() < Duration::from_secs(10), "{line} did not finish: {}", daemon_log(&env));
            std::thread::sleep(Duration::from_millis(50));
        }
        (std::fs::read_to_string(&rc).unwrap().trim().to_string(), std::fs::read_to_string(env.dir.join(format!("out{i}"))).unwrap_or_default())
    };
    for (i, (flag, what)) in [("--stop", "stop"), ("--update", "update"), ("--disconnect", "disconnect")].into_iter().enumerate() {
        let (code, err) = run(&mut client, i, &format!("'{nerd}' {flag} 1"), "2");
        assert_eq!(err, format!("nerd: refused: {what} from inside a pane\n"), "{flag} exit {code}: {}", daemon_log(&env));
        assert_eq!(code, "1", "{flag} exits non-zero");
    }
    // With no name and no explicit target, the client refuses before reaching any server.
    let (code, err) = run(&mut client, 5, &format!("env -u NERD_SOCKET -u NERD_STATE_DIR '{nerd}' --stop"), "2");
    assert!(err.starts_with("nerd: refused: nerd --stop with no name from inside a pane acts on every server; name one: nerd --stop <server>"), "{err}");
    assert_eq!(code, "1");
    let (code, err) = run(&mut client, 6, &format!("'{nerd}' --resume 1"), "2");
    assert_eq!(err, "nerd: refused: attach from inside a pane\n", "a pane took the screen");
    assert_eq!(code, "1");

    let log = daemon_log(&env);
    for what in ["stop", "update", "disconnect", "attach"] {
        assert!(log.contains(&format!("WARN  server[{}] refused {what} from pid ", daemon.0.id())), "no log line for {what}: {log}");
    }
    assert!(daemon.0.try_wait().unwrap().is_none(), "the server stopped: {log}");
    assert!(!log.contains("handover:"), "the server updated: {log}");

    // The same command from outside the panes stops it.
    let out = Command::new(nerd)
        .arg("--stop")
        .env("HOME", &env.dir)
        .env("NERD_STATE_DIR", &env.dir)
        .env("NERD_SOCKET", &env.socket)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(wait_exit(&mut daemon.0, Duration::from_secs(10)).success());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn allow_from_panes_lets_a_pane_stop_the_server() {
    let env = setup_env("inside-allowed");
    std::fs::write(env.dir.join("policy.json"), ALLOW_FROM_PANES).unwrap();
    let mut daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 50);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // The switch lifts the no-name refusal too; this HOME has no instances, so nothing is stopped.
    type_line(&mut client, &format!("env -u NERD_SOCKET -u NERD_STATE_DIR '{}' --stop >\"$HOME/out\" 2>&1; echo $? >\"$HOME/rc\"", env!("CARGO_BIN_EXE_nerd")));
    let t = Instant::now();
    while !std::fs::read_to_string(env.dir.join("rc")).is_ok_and(|s| s.ends_with('\n')) {
        assert!(t.elapsed() < Duration::from_secs(10), "the command did not finish: {}", daemon_log(&env));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(env.dir.join("out")).unwrap(), "no servers\n");
    assert_eq!(std::fs::read_to_string(env.dir.join("rc")).unwrap(), "0\n");
    type_line(&mut client, &format!("'{}' --stop", env!("CARGO_BIN_EXE_nerd")));
    assert!(wait_exit(&mut daemon.0, Duration::from_secs(10)).success(), "{}", daemon_log(&env));
    assert!(!daemon_log(&env).contains("refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// `policy.json` with `allow_from_panes` on, in the server's state folder.
const ALLOW_FROM_PANES: &str = r#"{"version":1,"on":true,"mode":"open","routes":[],"accounts":[],"allow_from_panes":true}"#;

/// Each pane's shell first sources `$HOME/p<pane>.sh`, where `msg <file> <request>…` speaks the message socket.
const MSG_SHELL: &str = r#"#!/bin/sh
msg() {
  out=$1; shift
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    { printf '%s\n' '{"Hello":{"protocol":1}}'; printf '%s\n' "$@"; } | /usr/bin/nc -w 1 -U "$NERD_MSOCK" > "$out.tmp"
    grep -q 'still starting' "$out.tmp" || break
    sleep 0.3
  done
  mv "$out.tmp" "$out"
}
[ -f "$HOME/p$NERD_PANE.sh" ] && . "$HOME/p$NERD_PANE.sh"
exec /bin/sh
"#;

/// A server restored with one tab per pane, labelled as given, each running its script; `policy`: policy.json.
fn msg_server(name: &str, labels: &[Option<&str>], scripts: &[&str], policy: Option<&str>) -> (Env, DaemonGuard) {
    use std::os::unix::fs::PermissionsExt as _;
    let env = setup_env(name);
    let shell = env.dir.join("sh");
    std::fs::write(&shell, MSG_SHELL).unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (i, script) in scripts.iter().enumerate() {
        std::fs::write(env.dir.join(format!("p{}.sh", i + 1)), script).unwrap();
    }
    let tabs: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let id = i + 1;
            let pane = serde_json::json!({"id": id, "cwd": env.dir, "argv": ["/bin/sh"], "agent_session": null, "label": label, "title": null, "cols": 80, "rows": 24});
            serde_json::json!({"name": null, "layout": {"Pane": id}, "panes": [pane], "focused": id})
        })
        .collect();
    let snap = serde_json::json!({"version": 2, "active_space": 0, "spaces": [{"name": null, "cwd": env.dir, "tabs": tabs, "active_tab": 0}]});
    std::fs::write(env.dir.join("session.json"), snap.to_string()).unwrap();
    if let Some(p) = policy {
        std::fs::write(env.dir.join("policy.json"), p).unwrap();
    }
    let daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &[("SHELL", shell.into_os_string())]);
    (env, daemon)
}

/// The JSON lines a pane's `msg` wrote to `file`, once it holds `needle`.
fn msg_lines(env: &Env, file: &str, needle: &str) -> Vec<serde_json::Value> {
    let path = env.dir.join(file);
    let t = Instant::now();
    while !read(&path).contains(needle) {
        assert!(t.elapsed() < Duration::from_secs(20), "{file} has no {needle:?}: {:?}\n{}", read(&path), daemon_log(env));
        std::thread::sleep(Duration::from_millis(50));
    }
    read(&path).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The last record of message `id` in messages.jsonl.
fn msg_record(env: &Env, id: &str) -> serde_json::Value {
    read(&env.dir.join("messages.jsonl")).lines().rev().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|v| v["id"] == id).unwrap_or_default()
}

#[test]
fn msg_two_panes() {
    let p2 = r#"while ! grep -q sent "$HOME/a.out" 2>/dev/null; do sleep 0.2; done; msg "$HOME/b.out" '{"Inbox":{"all":false}}'"#;
    let (env, mut daemon) = msg_server("msg-two", &[Some("a"), Some("b")], &[r#"msg "$HOME/a.out" '{"Send":{"to":"b","text":"hi there"}}'"#, p2], None);
    let a = msg_lines(&env, "a.out", "sent");
    assert_eq!(a[0], serde_json::json!({"ok": {"protocol": 1, "pane": 1, "name": "a"}}));
    assert_eq!(a[1]["sent"]["state"], "queued");
    let id = a[1]["sent"]["id"].as_str().unwrap();
    let rec = msg_record(&env, id);
    let from = &rec["from"]["pane"];
    assert_eq!((from["name"].clone(), from["pane"].clone(), from["uid"].clone()), (serde_json::json!("a"), serde_json::json!(1), serde_json::json!(unsafe { nix::libc::geteuid() })), "{rec}");
    assert_eq!(rec["to"]["pane"]["pane"], 2);
    let b = msg_lines(&env, "b.out", "messages");
    assert_eq!(b[0]["ok"]["name"], "b");
    assert_eq!((b[1]["messages"][0]["id"].clone(), b[1]["messages"][0]["text"].clone()), (serde_json::json!(id), serde_json::json!("hi there")));
    assert!(wait_reading(&mut connect(&env, 80, 24), Duration::from_secs(5), || msg_record(&env, id)["state"] == "read"), "{}", msg_record(&env, id));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_refusals() {
    let (env, mut daemon) = msg_server("msg-refuse", &[None], &[r#"msg "$HOME/a.out" '{"Send":{"to":"b","text":"hi"}}' '{"Agents":{}}'"#], None);
    let a = msg_lines(&env, "a.out", "refused");
    assert_eq!(a[0], serde_json::json!({"ok": {"protocol": 1, "pane": 1, "name": null}}));
    assert_eq!(a[1], serde_json::json!({"refused": "this pane has no name: the owner names it with Rename pane…"}));
    // This test's own process descends from no pane.
    let mut s = UnixStream::connect(env.dir.join("nerd.msock")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"{\"Hello\":{\"protocol\":1}}\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert_eq!(out, "{\"error\":\"only panes inside nerd can send\"}\n", "{}", daemon_log(&env));
    stop_and_wait(&env, &mut daemon);
    assert!(!env.dir.join("nerd.msock").exists(), "the socket goes with the server");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_strict() {
    let strict = r#"{"version":1,"on":true,"mode":"strict","routes":[],"accounts":[],"allow_from_panes":false}"#;
    let p1 = r#"msg "$HOME/a.out" '{"Send":{"to":"b","text":"hi"}}' '{"Send":{"to":"b","text":"","ask_route":true}}'"#;
    let (env, mut daemon) = msg_server("msg-strict", &[Some("a"), Some("b")], &[p1], Some(strict));
    let a = msg_lines(&env, "a.out", "held");
    assert_eq!(a[1], serde_json::json!({"refused": "no route a → b, strict mode"}));
    assert_eq!(a[2]["held"]["why"], "asked the owner for a route to b");
    assert_eq!(msg_record(&env, a[2]["held"]["id"].as_str().unwrap())["state"], serde_json::json!({"held": "route_request"}));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_handover() {
    let p2 = r#"while [ ! -f "$HOME/go" ]; do sleep 0.2; done; msg "$HOME/b.out" '{"Inbox":{"all":false}}'"#;
    let (env, _daemon) = msg_server("msg-update", &[Some("a"), Some("b")], &[r#"msg "$HOME/a.out" '{"Send":{"to":"b","text":"kept"}}'"#, p2], None);
    let a = msg_lines(&env, "a.out", "sent");
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    update_and_wait(&env, &mut client);
    std::fs::write(env.dir.join("go"), "").unwrap();
    let mut b = Vec::new();
    assert!(wait_reading(&mut client, Duration::from_secs(20), || read(&env.dir.join("b.out")).contains("messages")), "{}", daemon_log(&env));
    b.extend(read(&env.dir.join("b.out")).lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()));
    assert_eq!(b[1]["messages"][0]["id"], a[1]["sent"]["id"], "the queued message crossed the update: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_held_restart() {
    let ask = r#"{"version":1,"on":true,"mode":"open","routes":[{"from":"a","to":"b","action":"ask","until":null}],"accounts":[],"allow_from_panes":false}"#;
    let p1 = r#"if [ -f "$HOME/a.out" ]; then id=$(sed -n 's/.*"id":"\([0-9A-Z]*\)".*/\1/p' "$HOME/a.out"); msg "$HOME/a2.out" "{\"Show\":{\"id\":\"$id\"}}"; else msg "$HOME/a.out" '{"Send":{"to":"b","text":"wait for it"}}'; fi"#;
    let (env, mut daemon) = msg_server("msg-held", &[Some("a"), Some("b")], &[p1], Some(ask));
    let a = msg_lines(&env, "a.out", "held");
    assert_eq!(a[1]["held"]["why"], "route 1: a → b ask");
    stop_and_wait(&env, &mut daemon);
    let shell = env.dir.join("sh").into_os_string();
    let _daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &[("SHELL", shell)]);
    let shown = msg_lines(&env, "a2.out", "messages");
    assert_eq!((shown[1]["messages"][0]["id"].clone(), shown[1]["messages"][0]["state"].clone()), (a[1]["held"]["id"].clone(), serde_json::json!({"held": "route"})), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_log_no_text() {
    let (env, mut daemon) = msg_server("msg-log", &[Some("a"), Some("b")], &[r#"msg "$HOME/a.out" '{"Send":{"to":"b","text":"SECRET-7Q3 text"}}'"#], None);
    let a = msg_lines(&env, "a.out", "sent");
    let id = a[1]["sent"]["id"].as_str().unwrap();
    stop_and_wait(&env, &mut daemon);
    let log = daemon_log(&env);
    assert!(log.contains(&format!("message {id} a(1,uid ")), "the record is logged: {log}");
    assert!(!log.contains("SECRET-7Q3"), "the text reached the log: {log}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Waits until `file` in the test's folder ends in a newline, and returns its text.
fn msg_file(env: &Env, file: &str, secs: u64) -> String {
    let t = Instant::now();
    loop {
        let text = read(&env.dir.join(file));
        if text.ends_with('\n') {
            return text;
        }
        assert!(t.elapsed() < Duration::from_secs(secs), "{file} did not appear: {text:?}\n{}", daemon_log(env));
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn msg_flood() {
    let bin = env!("CARGO_BIN_EXE_nerd");
    let p1 = format!(
        r#"N='{bin}'; until "$N" agents >/dev/null 2>&1; do sleep 0.3; done
i=0; : > "$HOME/rc.txt"
while [ $i -lt 1000 ]; do i=$((i+1)); "$N" send b "SECRET-FLOOD-$i" >/dev/null 2>&1; echo $? >> "$HOME/rc.txt"; done
echo done > "$HOME/done""#
    );
    let (env, mut daemon) = msg_server("msg_flood", &[Some("a"), Some("b")], &[&p1], None);
    let t = Instant::now();
    msg_file(&env, "done", 240);
    let secs = t.elapsed().as_secs();
    let codes = read(&env.dir.join("rc.txt"));
    let codes: Vec<&str> = codes.lines().collect();
    assert_eq!(codes.len(), 1000, "{}", daemon_log(&env));
    assert!(codes.iter().all(|c| ["0", "3", "1"].contains(c)), "an exit code outside 0, 3 and 1: {codes:?}");
    assert!(codes.contains(&"0"), "the first sends are queued");
    stop_and_wait(&env, &mut daemon);
    let log = daemon_log(&env);
    let lines = log.lines().filter(|l| l.contains(" message ")).count();
    // Held and refused lines are at most one a second each; queued ones are at most 6 a minute (the pair limit).
    let bound = (2 * (secs + 2) + 6 * (secs / 60 + 1)) as usize;
    assert!(lines <= bound, "{lines} message lines in {secs} s (bound {bound}): {log}");
    assert!(!log.contains("SECRET-FLOOD"), "the text reached the log");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn msg_wait_thread() {
    let bin = env!("CARGO_BIN_EXE_nerd");
    let open = r#"{"version":1,"on":true,"mode":"open","routes":[],"accounts":[],"allow_from_panes":false}"#;
    let hub = format!(
        r#"N='{bin}'
while [ ! -f "$HOME/ready.w1" ] || [ ! -f "$HOME/ready.w2" ] || [ ! -f "$HOME/ready.w3" ]; do sleep 0.2; done
until out=$("$N" send all ping 2>&1); do sleep 0.3; done
echo "$out" > "$HOME/sent.txt"
"$N" wait "${{out#sent }}" --thread --count 3 --timeout 20 > "$HOME/w1.txt" 2>&1; echo $? > "$HOME/w1.rc"
"$N" wait "${{out#sent }}" --thread --count 4 --timeout 2 > "$HOME/w2.txt" 2>&1; echo $? > "$HOME/w2.rc""#
    );
    let worker = |k: u8| {
        format!(
            r#"N='{bin}'; until "$N" agents >/dev/null 2>&1; do sleep 0.3; done
touch "$HOME/ready.w{k}"
until id=$("$N" inbox 2>/dev/null | sed -n 's/.*id \([0-9A-Z]\{{26\}}\);.*/\1/p' | head -1); [ -n "$id" ]; do sleep 0.3; done
"$N" send --reply "$id" "ok from w{k}" > "$HOME/r{k}.txt" 2>&1"#
        )
    };
    let (w1, w2, w3) = (worker(1), worker(2), worker(3));
    let (env, mut daemon) = msg_server("msg_wait", &[Some("hub"), Some("w1"), Some("w2"), Some("w3")], &[&hub, &w1, &w2, &w3], Some(open));
    let rc = msg_file(&env, "w1.rc", 60);
    let got = read(&env.dir.join("w1.txt"));
    assert_eq!(rc, "0\n", "{got}\n{}", daemon_log(&env));
    assert_eq!(got.matches("[message from w").count(), 3, "{got}");
    for k in 1..=3 {
        assert!(got.contains(&format!("ok from w{k}")), "{got}");
    }
    assert_eq!(msg_file(&env, "w2.rc", 20), "2\n", "{}", read(&env.dir.join("w2.txt")));
    stop_and_wait(&env, &mut daemon);
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// The new build adopts the broker, so the pid it anchors on comes with the handover.
#[test]
fn after_an_update_a_pane_is_still_refused() {
    let env = setup_env("inside-updated");
    let mut daemon = spawn_daemon(&env);
    let mut client = connect(&env, 200, 50);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    ClientMsg::Update.write_to(&mut client).unwrap();
    let t = Instant::now();
    while !daemon_log(&env).contains("carried over") && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(daemon_log(&env).contains("carried over"), "no handover: {}", daemon_log(&env));
    type_line(&mut client, &format!("'{}' --stop 1 2>\"$HOME/err\"; echo $? >\"$HOME/rc\"", env!("CARGO_BIN_EXE_nerd")));
    let t = Instant::now();
    while !std::fs::read_to_string(env.dir.join("rc")).is_ok_and(|s| s.ends_with('\n')) {
        assert!(t.elapsed() < Duration::from_secs(10), "the command did not finish: {}", daemon_log(&env));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(env.dir.join("err")).unwrap(), "nerd: refused: stop from inside a pane\n", "{}", daemon_log(&env));
    assert_eq!(std::fs::read_to_string(env.dir.join("rc")).unwrap(), "1\n");
    assert!(daemon.0.try_wait().unwrap().is_none(), "the server stopped: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// An agent nerd has never heard of, described by a manifest in the plugins folder: hooked after
/// a yes, found in its pane, and back on its own session after the server restarts.
#[test]
fn a_user_manifest_makes_a_new_agent_hooked_detected_and_resumed() {
    let env = setup_env("fakeagent");
    // The hook writes only under nerd's folder for this HOME, so the state root is that folder.
    let root = env.dir.join("Library/Application Support/nerd");
    std::fs::create_dir_all(root.join("plugins/fakeagent")).unwrap();
    std::fs::create_dir_all(env.dir.join(".fakeagent")).unwrap();
    let manifest = serde_json::json!({
        "name": "fakeagent",
        "title": "Fake Agent",
        "agent": {
            "names": ["fakeagent"],
            "config": { "path": "~/.fakeagent/hooks.json", "shape": "nested" },
            "arg": "fakeagent",
            "events": [{ "on": "Start", "do": "idle" }],
            "session": { "field": "sid", "kind": "word" },
            "resume": ["fakeagent", "--again", "{id}"]
        }
    });
    std::fs::write(root.join("plugins/fakeagent/plugin.json"), manifest.to_string()).unwrap();
    let bin = env.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // Reports its session through its hook and stays; args.log first, since the test reads it on seeing the screen line.
    let agent = bin.join("fakeagent");
    std::fs::write(&agent, "#!/bin/sh\necho \"$*\" >> \"$HOME/args.log\"\necho \"FAKE-ARGS:$(printf '%s,' \"$@\"):\"\ncmd=$(sed -n 's/.*\"command\": \"\\(.*\\)\"/\\1/p' \"$HOME/.fakeagent/hooks.json\" | sed 's/\\\\//g')\nprintf '{\"sid\":\"fk-42\"}' | eval \"$cmd\"\nwhile :; do sleep 1; done\n").unwrap();
    std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let install = |answer: &[u8]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nerd"))
            .arg("--install-hook")
            .env("HOME", &env.dir)
            .env("NERD_STATE_DIR", &root)
            .env_remove("NERD_SOCKET")
            .env_remove("NERD_INSTANCE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(answer).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let asked = install(b"n\n");
    assert!(asked.contains("\nFake Agent  ~/.fakeagent/hooks.json  fakeagent --again <id>\n"), "{asked}");
    assert!(asked.contains("Nothing installed") && !env.dir.join(".fakeagent/hooks.json").exists(), "a no writes nothing: {asked}");
    let garbled = install(b"y\xd0\n");
    assert!(garbled.contains("Nothing installed"), "an answer that is not UTF-8 is a no, not an error: {garbled}");
    let said = install(b"y\n");
    let config = std::fs::read_to_string(env.dir.join(".fakeagent/hooks.json")).unwrap();
    assert!(said.contains("Fake Agent  ") && config.contains("nerd-agent-state.sh' idle fakeagent"), "{said}\n{config}");
    assert!(!install(b"").contains("Install?"), "asked once: an agent already hooked is not asked about again");

    let extra = [("NERD_STATE_DIR", root.clone().into_os_string()), ("PATH", format!("{}:/usr/bin:/bin", bin.display()).into())];
    let mut daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &extra);
    let log = || std::fs::read_to_string(root.join("daemon.log")).unwrap_or_default();
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", log());
    type_line(&mut client, "fakeagent");
    assert!(wait_for(&mut client, "FAKE-ARGS:,:", Duration::from_secs(5)), "the agent did not start: {}", log());
    // the pane's agent and its session, as the saved snapshot holds them
    let saved = || -> Option<(String, String)> {
        let snap: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("session.json")).ok()?).ok()?;
        let pane = &snap["spaces"][0]["tabs"][0]["panes"][0];
        Some((pane["agent"].as_str()?.to_string(), pane["agent_session"].as_str()?.to_string()))
    };
    let found = Some(("fakeagent".to_string(), "fk-42".to_string()));
    assert!(wait_reading(&mut client, Duration::from_secs(10), || saved() == found), "detected and its session saved: {:?}\n{}", saved(), log());
    let mut s = UnixStream::connect(&env.socket).unwrap();
    ClientMsg::Shutdown.write_to(&mut s).unwrap();
    assert!(wait_exit(&mut daemon.0, Duration::from_secs(10)).success());
    assert_eq!(saved(), found, "and still so after the stop");
    assert!(log().contains("agent manifest loaded: fakeagent"), "{}", log());

    let _daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &extra);
    let mut client = connect(&env, 100, 30);
    // A space already blank on screen goes out as a cursor move, not a character: the needle has none.
    let (seen, screen) = wait_text(&mut client, "FAKE-ARGS:--again,fk-42,:", Duration::from_secs(10));
    let ran = std::fs::read_to_string(env.dir.join("args.log")).unwrap_or_default();
    assert!(seen && ran == "\n--again fk-42\n", "not resumed: the agent ran with {ran:?}\nscreen: {screen:?}\n{}", log());
    assert!(log().contains("pane 1: fakeagent session fk-42 resumed; typed: fakeagent --again fk-42"), "{}", log());
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// The network a plugin may be granted is outbound IP on its ports and name lookups: nerd's own socket, where screens
/// attach and stop is heard, stays out of its reach.
#[test]
fn a_plugin_allowed_the_network_cannot_reach_the_servers_socket() {
    let env = setup_env("plugin-net-unix");
    let data = install_plugin(&env, "netty", serde_json::json!({ "name": "netty", "title": "N", "exec": { "command": "sh", "args": ["plugin.sh"] }, "events": ["command"], "badges": ["tab"], "permissions": { "network": { "hosts": ["example.com"], "ports": [443] } } }));
    let script = "main=\"${NERD_SOCKET%.psock}.sock\"\nif /usr/bin/nc -U \"$main\" < /dev/null; then echo REACHED; else echo refused; fi > \"$NERD_PLUGIN_DATA/probe.tmp\"\nmv \"$NERD_PLUGIN_DATA/probe.tmp\" \"$NERD_PLUGIN_DATA/probe\"\nexec sleep 30\n";
    std::fs::write(env.dir.join("plugins/netty/plugin.sh"), script).unwrap();
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(10), || data.join("probe").exists()), "the plugin never ran: {}", daemon_log(&env));
    assert_eq!(read(&data.join("probe")).trim(), "refused", "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A plugin cannot grow the log at will: refused second connections, their reports, garbage and over-cap messages
/// are each logged at most once a second, however many it opens.
#[test]
fn a_plugin_that_floods_its_socket_cannot_grow_the_log() {
    let env = setup_env("plugin-flood");
    let data = install_plugin(&env, "flood", serde_json::json!({ "name": "flood", "title": "F", "exec": { "command": "sh", "args": ["plugin.sh"] }, "events": ["command"], "badges": ["tab"] }));
    let script = r#"cd "$NERD_PLUGIN_DATA"
c='{"Connect":{"protocol":2,"role":"plugin","plugin":"flood","events":[]}}'
{ printf '%s\n' "$c"; exec sleep 1000; } | /usr/bin/nc -U "$NERD_SOCKET" > /dev/null &
sleep 1
printf '%s\n' "$c" > payload
printf 'garbage\n{"Segment":{"id":"s","text":"%s"}}\n' "$(printf '%0100d' 0)" >> payload
n=0; while [ $n -lt 1000 ]; do printf '{"ReportState":{"pane":1,"state":"working"}}\n'; n=$((n+1)); done >> payload
end=$(( $(date +%s) + 6 ))
while [ $(date +%s) -lt $end ]; do /usr/bin/nc -w 2 -U "$NERD_SOCKET" < payload > /dev/null 2>&1; done
touch done
exec sleep 1000
"#;
    std::fs::write(env.dir.join("plugins/flood/plugin.sh"), script).unwrap();
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(30), || data.join("done").exists()), "the flood never ended: {}", daemon_log(&env));
    let old = std::fs::metadata(env.dir.join("daemon.log.old")).map(|m| m.len()).unwrap_or(0);
    let now = std::fs::metadata(env.dir.join("daemon.log")).map(|m| m.len()).unwrap_or(0);
    assert!(old == 0 && now < 100_000, "the log grew to {now} bytes, {old} rolled over");
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A plugin holding hundreds of idle connections to its socket neither stops the server nor keeps a screen out:
/// past the cap they are refused, and one that never says Connect is closed after the handshake time.
#[test]
fn a_plugin_holding_many_connections_cannot_starve_the_server() {
    let env = setup_env("plugin-hold");
    let data = install_plugin(&env, "holder", serde_json::json!({ "name": "holder", "title": "H", "exec": { "command": "sh", "args": ["plugin.sh"] }, "events": ["command"], "badges": ["tab"] }));
    let script = "cd \"$NERD_PLUGIN_DATA\"\ni=0\nwhile [ $i -lt 300 ]; do ( sleep 60 | /usr/bin/nc -U \"$NERD_SOCKET\" > /dev/null 2>&1 ) & i=$((i+1)); done\ntouch held\nexec sleep 60\n";
    std::fs::write(env.dir.join("plugins/holder/plugin.sh"), script).unwrap();
    // Another plugin, started alongside: it must still get its connection.
    let mut good = test_plugin("good", &["command"], &["command"], "ping");
    good["badges"] = serde_json::json!(["tab"]);
    install_plugin(&env, "good", good);
    let daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(20), || data.join("held").exists() && daemon_log(&env).contains("already holds")), "{}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(15), || daemon_log(&env).contains("plugin good: connected")), "another plugin is locked out: {}", daemon_log(&env));
    let mut second = connect(&env, 100, 30);
    assert!(wait_for(&mut second, "spaces", Duration::from_secs(5)), "a screen still attaches: {}", daemon_log(&env));
    let threads = Command::new("ps").args(["-M", "-p", &daemon.0.id().to_string()]).output().unwrap();
    let threads = String::from_utf8_lossy(&threads.stdout).lines().count().saturating_sub(1);
    assert!(threads < 60, "{threads} threads for 300 held connections");
    assert!(!daemon_log(&env).contains("panic"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A plugin that keeps two hundred sockets reconnecting does not keep another plugin, started after it, out.
#[test]
fn a_plugin_reconnecting_in_a_loop_does_not_lock_another_out() {
    let env = setup_env("plugin-spin");
    install_plugin(&env, "spin", serde_json::json!({ "name": "spin", "title": "S", "exec": { "command": "sh", "args": ["plugin.sh"] }, "events": ["command"], "badges": ["tab"] }));
    let script = r#"exec /usr/bin/perl -MIO::Socket::UNIX -MIO::Select -e '
$SIG{PIPE} = "IGNORE";
my $p = $ENV{NERD_SOCKET};
my $c = IO::Socket::UNIX->new(Peer => $p) or die;
print $c qq({"Connect":{"protocol":2,"role":"plugin","plugin":"spin","events":[]}}\n); $c->flush;
my $sel = IO::Select->new; my $end = time + 60;
while (time < $end) {
  while ($sel->count < 200) { my $s = IO::Socket::UNIX->new(Peer => $p) or last; $sel->add($s); }
  for my $s ($sel->can_read(0.01)) { my $b; if (!sysread($s, $b, 4096)) { $sel->remove($s); close $s; } }
}'
"#;
    std::fs::write(env.dir.join("plugins/spin/plugin.sh"), script).unwrap();
    let (name, _) = install_template(&env, "segment");
    let p = env.dir.join("plugins").join(&name).join("plugin.sh");
    let late = std::fs::read_to_string(&p).unwrap().replacen("set -eu\n", "set -eu\nsleep 3\n", 1);
    std::fs::write(&p, late).unwrap();
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let connected = format!("plugin {name}: connected");
    assert!(wait_reading(&mut client, Duration::from_secs(25), || daemon_log(&env).contains(&connected)), "locked out: {}", daemon_log(&env));
    assert!(!daemon_log(&env).contains(&format!("plugin {name}: off after")), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A peer that drips its Connect a byte at a time is closed at the handshake deadline all the same.
#[test]
fn a_connect_dripped_a_byte_at_a_time_is_cut_off() {
    let env = setup_env("plugin-drip");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let mut drip = UnixStream::connect(env.dir.join("nerd.psock")).unwrap();
    let t = Instant::now();
    // Each byte arrives well inside the handshake time; together they never finish a line.
    while t.elapsed() < Duration::from_secs(15) {
        if drip.write_all(b"x").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(t.elapsed() < Duration::from_secs(9), "still open after {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A connection to the plugin socket that never says Connect is closed once the handshake time is up.
#[test]
fn a_silent_connection_to_the_plugin_socket_is_closed() {
    let env = setup_env("plugin-silent");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let mut idle = UnixStream::connect(env.dir.join("nerd.psock")).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    let t = Instant::now();
    let mut buf = [0u8; 256];
    let _ = std::io::Read::read(&mut idle, &mut buf);
    assert!(t.elapsed() > Duration::from_secs(3) && t.elapsed() < Duration::from_secs(9), "closed after {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A plugin from tests/fixtures/plugins in this server's plugins folder; returns its data folder.
fn install_plugin(env: &Env, name: &str, manifest: serde_json::Value) -> PathBuf {
    let dir = env.dir.join("plugins").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plugins/plugin.sh"), dir.join("plugin.sh")).unwrap();
    env.dir.join("plugin-data").join(name)
}

/// `declared`: what the manifest allows; `subscribed`: what the script asks for on Connect.
fn test_plugin(name: &str, declared: &[&str], subscribed: &[&str], command: &str) -> serde_json::Value {
    let args: Vec<&str> = std::iter::once("plugin.sh").chain(subscribed.iter().copied()).collect();
    serde_json::json!({
        "name": name,
        "title": name,
        "exec": { "command": "sh", "args": args },
        "commands": [{ "id": command, "title": format!("Run {command}"), "where": "global" }],
        "events": declared
    })
}

fn key(w: &mut impl Write, code: KeyCode, m: KeyModifiers) {
    ClientMsg::Event(Event::Key(KeyEvent::new(code, m))).write_to(w).unwrap();
}

/// ⌥+M opens the global menu; up from its first item wraps past Stop, Disconnect and Detach to the plugin group's last item.
fn run_last_menu_item(w: &mut impl Write) {
    key(w, KeyCode::Char('m'), KeyModifiers::ALT);
    for _ in 0..4 {
        key(w, KeyCode::Up, KeyModifiers::NONE);
    }
    key(w, KeyCode::Enter, KeyModifiers::NONE);
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn plugin_pid(data: &std::path::Path) -> Option<i32> {
    read(&data.join("pid")).trim().parse().ok()
}

/// Nothing left in the plugin's process group: it was its session's leader.
fn group_gone(pid: i32) -> bool {
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), None).is_err()
}

#[test]
fn plugins_start_after_an_update_from_a_broker_too_old_to_launch_them() {
    let env = setup_env("plugin-old-broker");
    let mut m = test_plugin("slot", &["command"], &["command"], "ping");
    m["badges"] = serde_json::json!(["tab"]);
    let data = install_plugin(&env, "slot", m);
    // The older build's handover names no broker protocol: that broker can only open panes.
    let (_daemon, _broker) = adopt_command_pane(&env, &["/bin/sh"], None);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    let connected = || daemon_log(&env).matches("plugin slot: connected").count();
    assert!(wait_reading(&mut client, Duration::from_secs(15), || connected() == 1), "the slot plugin did not start: {}", daemon_log(&env));
    assert!(!daemon_log(&env).contains("restart the server"), "{}", daemon_log(&env));
    assert!(!env.dir.join("s.xsock").exists() && !std::fs::read_dir(&env.dir).unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "xsock")), "the one-time socket is gone");
    assert_eq!(saved_panes(&env), Some(1), "the helper is no pane");

    run_last_menu_item(&mut client);
    let heard = || read(&data.join("events.log")).matches("\"id\":\"ping\"").count();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard() == 1), "the command did not arrive: {}", daemon_log(&env));

    // The next update drops the helper with its plugins and starts both again.
    let first = plugin_pid(&data).unwrap();
    update_and_wait(&env, &mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(5), || group_gone(first)), "the plugin outlived the update");
    assert!(wait_reading(&mut client, Duration::from_secs(15), || connected() == 2), "not started again: {}", daemon_log(&env));

    // The helper dying is said, and plugins are off, not waited on, until the next update.
    let xsock = env.dir.join("nerd.xsock").to_string_lossy().into_owned();
    let found = Command::new("pgrep").args(["-f", &format!("exec-broker {xsock}")]).output().unwrap();
    let helper: i32 = String::from_utf8_lossy(&found.stdout).lines().next().expect("the helper runs").trim().parse().unwrap();
    let orphan = plugin_pid(&data).unwrap();
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(helper), nix::sys::signal::Signal::SIGKILL);
    assert!(wait_reading(&mut client, Duration::from_secs(5), || daemon_log(&env).contains("the broker for plugins ended")), "{}", daemon_log(&env));
    // A broker killed outright stops nothing, as with the main one: the test cleans up after it.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(orphan), nix::sys::signal::Signal::SIGKILL);
    assert_eq!(saved_panes(&env), Some(1));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_starts_on_its_menu_command_hears_it_and_starts_again_after_an_update() {
    let env = setup_env("plugin-lazy");
    let data = install_plugin(&env, "lazy", test_plugin("lazy", &["command"], &["command"], "ping"));
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(!wait_reading(&mut client, Duration::from_secs(1), || data.join("pid").exists()), "started before its first use");

    run_last_menu_item(&mut client);
    let heard = || read(&data.join("events.log")).matches("\"id\":\"ping\"").count();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard() == 1), "the command did not arrive: {:?}\n{}", read(&data.join("events.log")), daemon_log(&env));
    assert!(read(&data.join("events.log")).starts_with("{\"event\":\"connected\",\"protocol\":2}\n"));
    assert!(daemon_log(&env).contains("plugin lazy: connected, events command"), "{}", daemon_log(&env));
    let first = plugin_pid(&data).unwrap();

    update_and_wait(&env, &mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(3), || group_gone(first)), "the plugin outlived the build that started it");
    run_last_menu_item(&mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard() == 2), "not started again after the update: {}", daemon_log(&env));
    assert_ne!(plugin_pid(&data), Some(first));
    assert!(!daemon_log(&env).contains("exited on its own"), "an exit during a handover counted as a crash: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_hears_only_what_it_subscribed_to_cannot_leave_its_sandbox_and_dies_with_the_server() {
    let env = setup_env("plugin-probe");
    let data = install_plugin(&env, "probe", test_plugin("probe", &["command", "pane_opened"], &["command"], "probe"));
    let manifest = read(&env.dir.join("plugins/probe/plugin.json"));
    // HOME apart from the state root, so its key folder is refused on its own account; allow_from_panes on, which must not reach a plugin.
    let home = std::path::PathBuf::from(format!("{}-home", env.dir.display()));
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_test"), "secret").unwrap();
    std::fs::create_dir_all(home.join(".local/share/fish")).unwrap();
    let _gone = Gone(vec![env.dir.clone(), home.clone()]);
    std::fs::write(home.join(".local/share/fish/fish_history"), "- cmd: secret").unwrap();
    std::fs::write(home.join(".zsh_history.new"), "secret").unwrap();
    std::fs::create_dir_all(home.join("dotfiles")).unwrap();
    std::fs::write(home.join("dotfiles/fish_history"), "- cmd: secret").unwrap();
    std::os::unix::fs::symlink(home.join("dotfiles/fish_history"), home.join(".local/share/fish/linked_history")).unwrap();
    // How MySQL turns its history off: /dev/null must stay readable and writable.
    std::os::unix::fs::symlink("/dev/null", home.join(".mysql_history")).unwrap();
    std::fs::write(home.join(".zshrc"), "").unwrap();
    std::fs::create_dir_all(home.join("Library/Application Support/nerd")).unwrap();
    std::fs::write(home.join("Library/Application Support/nerd/update-check"), "off\n").unwrap();
    std::fs::write(env.dir.join("policy.json"), ALLOW_FROM_PANES).unwrap();
    std::fs::create_dir_all(env.dir.join("plugin-data/other")).unwrap();
    std::fs::write(env.dir.join("plugin-data/other/secret"), "secret").unwrap();
    let extra = [("HOME", home.clone().into_os_string())];
    let mut daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &extra);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // pane_opened is declared, so the first pane starts it; it did not subscribe, so it never hears one.
    assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("plugin probe: connected")), "{}", daemon_log(&env));
    run_last_menu_item(&mut client);
    let results = data.join("results.log");
    assert!(wait_reading(&mut client, Duration::from_secs(20), || read(&results).contains("done") && data.join("detached.out").exists()), "{:?}\n{}", read(&results), daemon_log(&env));
    let events = read(&data.join("events.log"));
    assert!(events.contains("\"id\":\"probe\"") && !events.contains("pane_opened"), "{events}");

    let rc = |what: &str| read(&results).lines().find_map(|l| l.strip_prefix(&format!("{what} "))?.parse::<i32>().ok());
    for what in ["open", "launchctl", "osascript", "tcp", "main-socket", "write-manifest", "write-outside", "read-ssh", "read-fish", "read-zsh-new", "read-link", "read-session", "list-root", "read-other"] {
        assert!(rc(what).is_some_and(|r| r != 0), "{what} went through: {}", read(&results));
    }
    for what in ["write-data", "read-own", "read-home"] {
        assert_eq!(rc(what), Some(0), "{what}: {}", read(&results));
    }
    assert_eq!(read(&data.join("fd3")), "closed\n", "it inherited the broker's socket");
    assert_eq!(read(&env.dir.join("plugins/probe/plugin.json")), manifest);
    assert!(!env.dir.join("outside").exists());
    let listed = Command::new("/bin/launchctl").arg("list").output().unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("dev.nerd.probe."), "launchd took a job from it");
    assert!(daemon_log(&env).contains("plugin probe: report for pane 1 refused"), "a pane without its agent: {}", daemon_log(&env));
    assert_eq!(read(&data.join("again.out")), "refused: plugin \"probe\" is already connected\n", "a second Connect, answered and closed");
    // Its detached grandchild: only Connect on the plugin socket, and the main socket out of reach.
    assert_eq!(read(&data.join("shutdown.out")), "refused: the plugin socket serves only Connect\n");
    assert_eq!(read(&data.join("hello.out")), "refused: the plugin socket serves only Connect\n");
    assert_ne!(read(&data.join("detached.out")).trim(), "0", "reached the main socket");
    assert!(daemon.0.try_wait().unwrap().is_none(), "the server stopped: {}", daemon_log(&env));
    assert!(!daemon_log(&env).contains("attached: 80x24"), "a screen was attached: {}", daemon_log(&env));

    let pid = plugin_pid(&data).unwrap();
    let sleeper: i32 = read(&data.join("sleeper")).trim().parse().unwrap();
    stop_and_wait(&env, &mut daemon);
    let t = Instant::now();
    while !(group_gone(pid) && gone(sleeper)) && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(group_gone(pid) && gone(sleeper), "a plugin process outlived the server");
}

/// Removes its folders when the test ends, failed or not.
struct Gone(Vec<PathBuf>);

impl Drop for Gone {
    fn drop(&mut self) {
        for d in &self.0 {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn gone(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
}

/// Waits for the n-th counted crash of `name`, reading the client.
fn wait_crash(env: &Env, client: &mut UnixStream, name: &str, n: usize) {
    let line = format!("plugin {name}: crash {n} of 5 within 180 s");
    assert!(wait_reading(client, Duration::from_secs(10), || daemon_log(env).contains(&line)), "no {line:?}: {}", daemon_log(env));
}

#[test]
fn a_plugin_that_connects_and_then_dies_is_counted_and_its_children_go_with_it() {
    let env = setup_env("pl-exit");
    let mut m = test_plugin("quits", &["command"], &[], "go");
    m["exec"]["args"] = serde_json::json!(["plugin.sh", "exit-after-connect"]);
    let data = install_plugin(&env, "quits", m);
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    for n in 1..=5 {
        run_last_menu_item(&mut client);
        wait_crash(&env, &mut client, "quits", n);
        let sleeper: i32 = read(&data.join("sleeper")).trim().parse().unwrap();
        assert!(wait_reading(&mut client, Duration::from_secs(3), || gone(sleeper)), "its child outlived it");
    }
    assert_eq!(daemon_log(&env).matches("plugin quits: connected").count(), 5, "{}", daemon_log(&env));
    assert!(daemon_log(&env).lines().any(|l| l.contains("ERROR") && l.contains("plugin quits: 5 crashes within 180 s, disabled until Reload plugins and agents")), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_whose_connection_dies_before_it_exits_is_counted() {
    let env = setup_env("pl-drop");
    let mut m = test_plugin("drops", &["command"], &[], "go");
    m["exec"]["args"] = serde_json::json!(["plugin.sh", "drop-then-exit"]);
    install_plugin(&env, "drops", m);
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    for n in 1..=5 {
        run_last_menu_item(&mut client);
        wait_crash(&env, &mut client, "drops", n);
    }
    assert_eq!(daemon_log(&env).matches("plugin drops: connected").count(), 5, "{}", daemon_log(&env));
    assert!(daemon_log(&env).lines().any(|l| l.contains("ERROR") && l.contains("plugin drops: 5 crashes within 180 s, disabled until Reload plugins and agents")), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_that_never_connects_is_stopped_after_five_seconds_and_counted() {
    let env = setup_env("pl-mute");
    let mut m = test_plugin("mute", &["command"], &[], "go");
    m["exec"]["args"] = serde_json::json!(["plugin.sh", "never-connect"]);
    let data = install_plugin(&env, "mute", m);
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    run_last_menu_item(&mut client);
    let t = Instant::now();
    wait_crash(&env, &mut client, "mute", 1);
    assert!(t.elapsed() >= Duration::from_secs(4), "stopped before its 5 s");
    assert!(daemon_log(&env).contains("plugin mute: no Connect within 5 s, stopped"), "{}", daemon_log(&env));
    let pid = plugin_pid(&data).unwrap();
    assert!(wait_reading(&mut client, Duration::from_secs(3), || group_gone(pid)), "still running");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_that_keeps_crashing_is_turned_off() {
    let env = setup_env("plugin-crash");
    let mut m = test_plugin("crash", &["command"], &[], "boom");
    m["exec"] = serde_json::json!({ "command": "sh", "args": ["-c", "exit 3"] });
    install_plugin(&env, "crash", m);
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    for n in 1..=5 {
        run_last_menu_item(&mut client);
        wait_crash(&env, &mut client, "crash", n);
    }
    assert!(daemon_log(&env).lines().any(|l| l.contains("ERROR") && l.contains("plugin crash: 5 crashes within 180 s, disabled until Reload plugins and agents")), "{}", daemon_log(&env));
    // The last crash's notice goes in a frame of its own first: a card drawn over one just like it would
    // send only the cells that differ.
    key(&mut client, KeyCode::Esc, KeyModifiers::NONE);
    let _ = wait_reading(&mut client, Duration::from_millis(500), || false);
    run_last_menu_item(&mut client);
    // The notice card: a gap between words may arrive as a cursor move, so the needle has none.
    let (ok, seen) = wait_text(&mut client, "crashes;Reload", Duration::from_secs(5));
    assert!(ok, "{seen}\n{}", daemon_log(&env));
    assert_eq!(daemon_log(&env).matches("plugin crash started: runs /bin/sh, network none").count(), 5, "started again while off");
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Through its proxy the wrapper is the plugin's parent: a stop still reaches the plugin, and the server hears it end.
#[test]
fn a_plugin_behind_its_proxy_is_stopped_like_any_other() {
    for net in [serde_json::json!(false), serde_json::json!({ "hosts": ["wttr.in"], "ports": [443] })] {
        let env = setup_env("plugin-proxy-stop");
        let mut m = test_plugin("netstay", &["command"], &["command"], "go");
        m["permissions"] = serde_json::json!({ "network": net });
        install_plugin(&env, "netstay", m);
        let _daemon = spawn_daemon(&env);
        let mut client = connect(&env, 100, 30);
        assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
        run_last_menu_item(&mut client);
        assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("plugin netstay: connected")), "{}", daemon_log(&env));
        let off = Command::new(env!("CARGO_BIN_EXE_nerd")).args(["--plugin-off", "netstay"]).env("HOME", &env.dir).env("NERD_STATE_DIR", &env.dir).env("NERD_SOCKET", &env.socket).output().unwrap();
        assert!(off.status.success(), "{}", String::from_utf8_lossy(&off.stderr));
        assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("plugin netstay: stopped")), "{net}: {}", daemon_log(&env));
        let _ = std::fs::remove_dir_all(&env.dir);
    }
}

/// A new commit under an unchanged manifest (re-added within one poll, or only its code updated) stops the old code.
#[test]
fn a_plugin_whose_commit_changed_under_the_same_manifest_is_stopped() {
    let env = setup_env("plugin-new-commit");
    install_plugin(&env, "pinned", test_plugin("pinned", &["command"], &["command"], "go"));
    let lock = |c: char| std::fs::write(env.dir.join("plugins.lock"), serde_json::json!([{ "name": "pinned", "source": "/x", "tag": null, "commit": c.to_string().repeat(40) }]).to_string()).unwrap();
    lock('a');
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    run_last_menu_item(&mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("plugin pinned: connected")), "{}", daemon_log(&env));
    // A later mtime than the server saw at start, whatever the file system's clock grain.
    std::thread::sleep(Duration::from_millis(1100));
    lock('b');
    assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("plugin pinned: stopped")), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// What a claim of role `plugin` from this test process is answered with.
fn claim(env: &Env, role: &str, plugin: &str) -> String {
    claim_at(&env.socket.with_extension("psock"), role, plugin)
}

fn claim_at(socket: &std::path::Path, role: &str, plugin: &str) -> String {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    ClientMsg::Connect { protocol: 2, role: role.into(), plugin: Some(plugin.into()), events: vec!["command".into()] }.write_to(&mut s).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn a_plugin_claim_from_a_process_nerd_did_not_launch_is_refused() {
    let env = setup_env("plugin-claim");
    let data = install_plugin(&env, "lazy", test_plugin("lazy", &["command"], &["command"], "ping"));
    let _daemon = spawn_daemon(&env);
    assert_eq!(claim(&env, "plugin", "lazy"), "refused: plugin \"lazy\" was not started by this server\n");
    assert_eq!(claim(&env, "control", "lazy"), "refused: role \"control\" is not served\n");
    assert_eq!(claim_at(&env.socket, "plugin", "lazy"), "refused: a plugin connects on the plugin socket\n", "not on the main socket");
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    run_last_menu_item(&mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(10), || read(&data.join("events.log")).contains("ping")), "{}", daemon_log(&env));
    assert_eq!(claim(&env, "plugin", "lazy"), "refused: plugin \"lazy\" was not started by this server\n", "while the real one runs");
    assert!(daemon_log(&env).lines().any(|l| l.contains("WARN") && l.contains("refused a plugin claim from pid")), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_reports_the_state_of_a_pane_running_its_own_agent() {
    let env = setup_env("plugin-report");
    let mut m = test_plugin("fakeagent", &["agent_state"], &["agent_state"], "x");
    m["agent"] = serde_json::json!({ "names": ["fakeagent"] });
    m["commands"] = serde_json::json!([]);
    let data = install_plugin(&env, "fakeagent", m);
    let bin = env.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("fakeagent"), "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(bin.join("fakeagent")).unwrap().permissions(), 0o755);
    Command::new("chmod").arg("755").arg(bin.join("fakeagent")).status().unwrap();
    let path = [("PATH", format!("{}:/usr/bin:/bin", bin.display()).into())];
    let _daemon = spawn_daemon_in(&env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &path);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    type_line(&mut client, "fakeagent");
    let state = || read(&env.dir.join("agents/1")).trim().to_string();
    let heard = || read(&data.join("events.log"));
    assert!(wait_reading(&mut client, Duration::from_secs(15), || state() == "working"), "state {:?}; heard {:?}\n{}", state(), heard(), daemon_log(&env));
    // events.log and stale.out are written by the plugin's own processes, which can run behind the server's log.
    assert!(wait_reading(&mut client, Duration::from_secs(5), || heard().contains("\"event\":\"agent_state\",\"pane\":1,\"agent\":\"fakeagent\"")), "heard {:?}\n{}", heard(), daemon_log(&env));
    assert!(!daemon_log(&env).contains("report for pane 1 refused"), "{}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || daemon_log(&env).contains("plugin fakeagent: report for pane 1 dropped: not from its current connection")), "{}", daemon_log(&env));
    let stale = || read(&data.join("stale.out"));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || stale() == "refused: plugin \"fakeagent\" is already connected\n"), "stale {:?}\n{}", stale(), daemon_log(&env));
    assert_eq!(state(), "working", "a report from a connection that is not the current one was taken");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn the_plugin_template_connects_hears_panes_and_answers_its_command() {
    let env = setup_env("pl-tpl");
    let dir = env.dir.join("plugins/hello");
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["plugin.json", "plugin.sh"] {
        std::fs::copy(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/plugin").join(f), dir.join(f)).unwrap();
    }
    let log = env.dir.join("plugin-data/hello/events.log");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    assert!(wait_reading(&mut client, Duration::from_secs(10), || read(&log).contains("\"event\":\"pane_opened\",\"pane\":1")), "{:?}\n{}", read(&log), daemon_log(&env));
    run_last_menu_item(&mut client);
    assert!(wait_reading(&mut client, Duration::from_secs(10), || read(&log).contains("hello, said hello")), "{:?}\n{}", read(&log), daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A broker that answers every spawn `ms` late.
fn slow_broker(env: &Env, ms: u64, extra: &[(&str, std::ffi::OsString)]) -> DaemonGuard {
    let mut vars = vec![("NERD_BROKER_DELAY_MS", std::ffi::OsString::from(ms.to_string()))];
    vars.extend(extra.iter().cloned());
    spawn_daemon_in(env, env!("CARGO_BIN_EXE_nerd").as_ref(), "xterm-256color", true, &[], &vars)
}

#[test]
fn a_slow_broker_does_not_stall_the_server_and_early_keys_reach_the_shell() {
    let env = setup_env("slow-spawn");
    let _daemon = slow_broker(&env, 4000, &[]);
    let t = Instant::now();
    // Its attach asks for the first pane, which the broker answers 4 s later.
    let mut first = connect(&env, 80, 24);
    assert!(wait_for(&mut first, "spaces", Duration::from_secs(2)), "no frame: {}", daemon_log(&env));
    let mut second = connect(&env, 80, 24);
    assert!(wait_for(&mut second, "spaces", Duration::from_secs(2)), "no frame: {}", daemon_log(&env));
    assert!(t.elapsed() < Duration::from_secs(3), "the server waited on the broker: {:?}", t.elapsed());
    type_line(&mut second, "echo EARLY-$((6*7))");
    assert!(wait_for(&mut second, "EARLY-42", Duration::from_secs(10)), "the early keys were lost: {}", daemon_log(&env));
    assert!(t.elapsed() >= Duration::from_secs(4), "answered before the broker did: {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_restore_with_a_slow_broker_attaches_every_pane() {
    use std::os::unix::fs::PermissionsExt as _;
    let env = setup_env("slow-restore");
    let shell = env.dir.join("sh");
    std::fs::write(&shell, "#!/bin/sh\necho READY-$NERD_PANE\nexec /bin/sh\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pane = |id: u64| serde_json::json!({"id": id, "cwd": env.dir, "argv": ["/bin/sh"], "agent_session": null, "title": null, "cols": 80, "rows": 24});
    let tabs: Vec<_> = (1..=3).map(|id| serde_json::json!({"name": null, "layout": {"Pane": id}, "panes": [pane(id)], "focused": id})).collect();
    let snap = serde_json::json!({"version": 2, "active_space": 0, "spaces": [{"name": null, "cwd": env.dir, "tabs": tabs, "active_tab": 2}]});
    std::fs::write(env.dir.join("session.json"), snap.to_string()).unwrap();
    // 3 x 2 s is past one 5 s wait: only a wait that starts over at each answer lets the last pane through.
    let mut daemon = slow_broker(&env, 2000, &[("SHELL", shell.into())]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "READY-3", Duration::from_secs(12)), "the last pane never attached: {}", daemon_log(&env));
    stop_and_wait(&env, &mut daemon);
    for id in 1..=3 {
        let text = std::fs::read_to_string(screens_dir(&env).join(format!("{id}.txt"))).unwrap_or_default();
        assert!(text.contains(&format!("READY-{id}")), "pane {id} was not attached: {text:?}\n{}", daemon_log(&env));
    }
    assert!(!daemon_log(&env).contains("failed to launch"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn an_update_while_a_pane_is_starting_carries_it_over_whole() {
    let env = setup_env("slow-update");
    let _daemon = slow_broker(&env, 2000, &[]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(2)), "no frame: {}", daemon_log(&env));
    ClientMsg::Update.write_to(&mut client).unwrap();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || daemon_log(&env).contains("carried over")), "no handover: {}", daemon_log(&env));
    let log = daemon_log(&env);
    assert!(log.contains("handover: 1 panes"), "the starting pane was not waited for: {log}");
    assert!(!log.contains("update failed") && !log.contains("failed to launch"), "{log}");
    type_line(&mut client, "echo AFTER-$((6*7))");
    assert!(wait_for(&mut client, "AFTER-42", Duration::from_secs(5)), "the pane came over without its shell: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// The server's own `open` helper, reached only from the default state root, so HOME is short enough for a socket path.
#[test]
fn the_open_helper_still_starts_under_a_slow_broker() {
    use std::os::unix::process::CommandExt as _;
    let home = PathBuf::from(format!("/tmp/nerd-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let nerd = home.join("Library/Application Support/nerd");
    std::fs::create_dir_all(&nerd).unwrap();
    std::fs::write(nerd.join("config.json"), r#"{"tmux": "new_window"}"#).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nerd"));
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
    }
    let _daemon = DaemonGuard(
        cmd.arg("--daemon")
            .env("HOME", &home)
            .env("SHELL", "/bin/sh")
            .env("NERD_BROKER_DELAY_MS", "1000")
            .env_remove("NERD_STATE_DIR")
            .env_remove("NERD_SOCKET")
            .env_remove("NERD_INSTANCE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let env = Env { dir: home.clone(), socket: nerd.join("nerd.sock") };
    let log = || std::fs::read_to_string(nerd.join("daemon.log")).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut s = loop {
        match UnixStream::connect(&env.socket) {
            Ok(s) => break s,
            Err(e) => assert!(Instant::now() < deadline, "no socket: {e}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let hello = serde_json::json!({"Hello": {"cwd": home, "cols": 80, "rows": 24, "mux": true, "terminal": "/nonexistent/Nope.app"}});
    s.write_all(format!("{hello}\n").as_bytes()).unwrap();
    // `open` fails at once; what it printed reaches the log only through its attached master.
    assert!(wait_reading(&mut s, Duration::from_secs(10), || log().contains("open said")), "{}", log());
    assert!(log().contains("Nope.app cannot be opened"), "the helper's output was not read: {}", log());
    assert!(!log().contains("open outside tmux failed"), "{}", log());
    let _ = std::fs::remove_dir_all(&home);
}

/// Pane ids in the saved session.
fn saved_ids(env: &Env) -> Vec<u64> {
    let Some(v) = std::fs::read_to_string(env.dir.join("session.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return Vec::new() };
    let tabs = v["spaces"].as_array().into_iter().flatten().flat_map(|s| s["tabs"].as_array().into_iter().flatten());
    tabs.flat_map(|t| t["panes"].as_array().into_iter().flatten()).filter_map(|p| p["id"].as_u64()).collect()
}

#[test]
fn a_broker_silent_past_the_wait_fails_the_pane_and_its_late_answer_is_dropped() {
    let env = setup_env("silent-broker");
    let _daemon = slow_broker(&env, 6000, &[]);
    let t = Instant::now();
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(2)), "no frame: {}", daemon_log(&env));
    let failed = "pane 1: failed to launch /bin/sh with 1 argument: the broker did not answer";
    assert!(wait_reading(&mut client, Duration::from_secs(8), || daemon_log(&env).contains(failed)), "{}", daemon_log(&env));
    assert!(t.elapsed() < Duration::from_secs(6), "failed only after the answer was due: {:?}", t.elapsed());
    // The late answer for pane 1 comes at 6 s; the fresh space's pane 2 is still waiting then.
    assert!(wait_reading(&mut client, Duration::from_secs(4), || t.elapsed() > Duration::from_secs(7) && saved_ids(&env) == [2]), "{:?}: {}", saved_ids(&env), daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// As before panes started in the background: every failed pane is said, one fresh space is tried, and the client stays.
#[test]
fn a_restore_whose_panes_all_fail_keeps_the_client_and_tries_one_fresh_space() {
    let env = setup_env("restore-fails");
    let tab = |id: u64| serde_json::json!({"name": null, "layout": {"Pane": id}, "panes": [{"id": id, "cwd": env.dir, "argv": ["/bin/sh"], "agent_session": null, "title": null, "cols": 80, "rows": 24}], "focused": id});
    let snap = serde_json::json!({"version": 2, "active_space": 0, "spaces": [{"name": null, "cwd": env.dir, "tabs": [tab(1), tab(2)], "active_tab": 0}]});
    std::fs::write(env.dir.join("session.json"), snap.to_string()).unwrap();
    let _daemon = slow_broker(&env, 300, &[("SHELL", "/nonexistent/sh".into())]);
    let mut client = connect(&env, 80, 24);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(2)), "no frame: {}", daemon_log(&env));
    let failed = |n| daemon_log(&env).matches("failed to launch /nonexistent/sh").count() == n;
    assert!(wait_reading(&mut client, Duration::from_secs(5), || failed(3)), "two restored panes and one fresh space: {}", daemon_log(&env));
    let t = Instant::now();
    let mut buf = [0u8; 4096];
    while t.elapsed() < Duration::from_secs(2) {
        if let Ok(0) = client.read(&mut buf) {
            panic!("the client was detached: {}", daemon_log(&env));
        }
    }
    assert!(failed(3), "nothing more was tried: {}", daemon_log(&env));
    assert!(daemon_log(&env).contains("pane 3: failed to launch"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// Reads the client into `term` until its top row satisfies `want`; returns whether it did, and the row.
fn top_until(stream: &mut UnixStream, term: &mut vt100::Parser, timeout: Duration, want: impl Fn(&str) -> bool) -> (bool, String) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 65536];
    loop {
        let cols = term.screen().size().1;
        let row = term.screen().rows(0, cols).next().unwrap_or_default();
        if want(&row) {
            return (true, row);
        }
        if Instant::now() >= deadline {
            return (false, row);
        }
        match stream.read(&mut buf) {
            Ok(0) => return (false, row),
            Ok(n) => term.process(&buf[..n]),
            Err(_) => {}
        }
    }
}

/// Reads the client into `term` until its whole screen satisfies `want`; returns whether it did, and the screen.
fn screen_until(stream: &mut UnixStream, term: &mut vt100::Parser, timeout: Duration, want: impl Fn(&str) -> bool) -> (bool, String) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 65536];
    loop {
        let screen = term.screen().contents();
        if want(&screen) {
            return (true, screen);
        }
        if Instant::now() >= deadline {
            return (false, screen);
        }
        match stream.read(&mut buf) {
            Ok(0) => return (false, screen),
            Ok(n) => term.process(&buf[..n]),
            Err(_) => {}
        }
    }
}

fn click(w: &mut impl Write, column: u16, row: u16) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        ClientMsg::Event(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })).write_to(w).unwrap();
    }
}

/// A plugin with slots; `lines` go into its data folder as `file`, which the fixture sends after Connect.
fn slot_plugin(env: &Env, name: &str, extra: serde_json::Value, file: &str, lines: &[String]) -> PathBuf {
    let mut m = test_plugin(name, &["command"], &["command"], "hit");
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    let data = install_plugin(env, name, m);
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join(file), lines.join("\n") + "\n").unwrap();
    data
}

fn wait_log(env: &Env, what: &str, n: usize) -> bool {
    let t = Instant::now();
    while daemon_log(env).matches(what).count() < n && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon_log(env).matches(what).count() >= n
}

#[test]
fn a_slot_plugin_starts_with_the_server_its_segment_runs_its_command_and_it_is_back_after_an_update() {
    let env = setup_env("pl-slots");
    let seg = r#"{"Segment":{"id":"s","text":"hello-seg","role":"ok","command":"hit"}}"#.to_string();
    let data = slot_plugin(&env, "status", serde_json::json!({ "segments": [{ "id": "s" }] }), "send.jsonl", &[seg]);
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, "plugin status: connected", 1), "not started with the server, before any client: {}", daemon_log(&env));
    let mut client = connect(&env, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| r.contains("hello-seg"));
    assert!(ok, "{row:?}\n{}", daemon_log(&env));
    let col = row[..row.find("hello-seg").unwrap()].chars().count() as u16;
    click(&mut client, col + 2, 0);
    let heard = || read(&data.join("events.log")).matches("\"id\":\"hit\"").count();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard() == 1), "a click on the segment ran nothing: {}", daemon_log(&env));

    update_and_wait(&env, &mut client);
    assert!(wait_log(&env, "plugin status: connected", 2), "not started again after the update: {}", daemon_log(&env));
    // A key takes the update's toast off the segments; the bytes read while waiting are gone, so a resize redraws it all.
    type_line(&mut client, "true");
    let mut term = vt100::Parser::new(30, 160, 0);
    ClientMsg::Event(Event::Resize(160, 30)).write_to(&mut client).unwrap();
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| r.contains("hello-seg"));
    assert!(ok, "the segment did not come back: {row:?}\n{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_flood_of_slot_messages_ends_in_its_last_state_and_undeclared_slots_are_refused() {
    let env = setup_env("pl-flood");
    let mut lines = Vec::new();
    for i in 0..3000 {
        lines.push(format!(r#"{{"Segment":{{"id":"n","text":"n{i}","role":"warn"}}}}"#));
        lines.push(format!(r#"{{"Badges":{{"target":"tab","items":[{{"pane":1,"text":"{}"}}]}}}}"#, i % 100));
        if i == 10 {
            lines.push(r#"{"Badges":{"target":"tree","items":[{"path":"/tmp/x","text":"1"}]}}"#.into());
            lines.push(r#"{"Segment":{"id":"undeclared","text":"nope"}}"#.into());
            lines.push(r#"{"Segment":{"id":"n","text":"way too long for any segment"}}"#.into());
        }
    }
    lines.push(r#"{"Segment":{"id":"n","text":"final-state","role":"ok"}}"#.into());
    lines.push(r#"{"Badges":{"target":"tab","items":[{"pane":1,"text":"tb"}]}}"#.into());
    let data = slot_plugin(&env, "flood", serde_json::json!({ "segments": [{ "id": "n" }], "badges": ["tab"] }), "send.jsonl", &lines);
    let mut daemon = spawn_daemon(&env);
    let mut client = connect(&env, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(15), |r| r.contains("final-state") && r.contains("1 \u{b7} tb "));
    assert!(ok, "the last state did not stay: {row:?}\n{}", daemon_log(&env));
    let log = daemon_log(&env);
    let refused: Vec<&str> = log.lines().filter(|l| l.contains("plugin flood: Segment refused") || l.contains("plugin flood: Badges refused")).collect();
    assert_eq!(refused.len(), 2, "one WARN from the reader for the over-cap one, one from the ingest for the rest: {log}");
    assert!(refused.iter().all(|l| l.contains("WARN")), "{log}");
    assert!(refused.iter().any(|l| l.contains("Segment refused: segment \"way too long")), "{log}");
    assert!(refused.iter().any(|l| l.contains("Badges refused: badges on \"tree\" are not in its manifest")), "{log}");
    assert!(!row.contains("nope"));
    type_line(&mut client, "echo still-here");
    assert!(wait_for(&mut client, "still-here", Duration::from_secs(5)), "the server stopped answering");
    assert!(daemon.0.try_wait().unwrap().is_none());
    let _ = data;
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn what_a_plugin_showed_goes_when_it_dies() {
    let env = setup_env("pl-dies");
    let once = vec![r#"{"Segment":{"id":"s","text":"gone-soon"}}"#.to_string(), r#"{"Badges":{"target":"tab","items":[{"pane":1,"text":"zz"}]}}"#.to_string()];
    let data = slot_plugin(&env, "dies", serde_json::json!({ "segments": [{ "id": "s" }], "badges": ["tab"] }), "once.jsonl", &once);
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| r.contains("gone-soon") && r.contains("1 \u{b7} zz "));
    assert!(ok, "{row:?}\n{}", daemon_log(&env));
    let pid = plugin_pid(&data).unwrap();
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).unwrap();
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| !r.contains("gone-soon") && !r.contains("zz"));
    assert!(ok, "its segment and badge outlived it: {row:?}\n{}", daemon_log(&env));
    assert!(wait_log(&env, "plugin dies: connected", 2), "a slot plugin is started again: {}", daemon_log(&env));
    let (_, row) = top_until(&mut client, &mut term, Duration::from_secs(1), |_| false);
    assert!(!row.contains("gone-soon"), "nothing came back: it sent it only once: {row:?}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_under_its_profile_answers_a_preview_of_its_file_type_and_is_asked_for_nothing_else() {
    let env = setup_env("pl-preview");
    let events = ["command", "preview_request"];
    let mut m = test_plugin("viewer", &events, &events, "hit");
    m["previews"] = serde_json::json!([{ "extensions": ["xyz"] }]);
    let data = install_plugin(&env, "viewer", m);
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("preview.json"), r#"[[{"text":"from the sandboxed plugin","role":"ok"}]]"#).unwrap();
    let files = env.dir.join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("a.xyz"), "plain xyz\n").unwrap();
    std::fs::write(files.join("b.txt"), "plain txt\n").unwrap();
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, "plugin viewer: connected", 1), "a preview plugin starts with the server: {}", daemon_log(&env));
    let mut client = connect_in(&env, &files, 160, 30);
    key(&mut client, KeyCode::Char('t'), KeyModifiers::ALT);
    let asked = || read(&data.join("events.log")).lines().filter(|l| l.contains("preview_request")).map(String::from).collect::<Vec<_>>();
    let mut term = vt100::Parser::new(30, 160, 0);
    // The tree opens on its first row; down until the .xyz file is the one selected.
    let t = Instant::now();
    while !asked().iter().any(|l| l.contains("a.xyz")) {
        assert!(t.elapsed() < Duration::from_secs(10), "never asked: {:?}\n{}", read(&data.join("events.log")), daemon_log(&env));
        key(&mut client, KeyCode::Down, KeyModifiers::NONE);
        screen_until(&mut client, &mut term, Duration::from_millis(300), |_| false);
    }
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("from the sandboxed plugin"));
    assert!(ok, "the answer was not drawn:\n{screen}\n{}", daemon_log(&env));
    assert!(screen.contains("a.xyz · 10B · viewer"), "{screen}");
    key(&mut client, KeyCode::Down, KeyModifiers::NONE);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("plain txt"));
    assert!(ok, "{screen}");
    let asked = asked();
    assert_eq!(asked.len(), 1, "one request, for its own type only: {asked:?}");
    assert!(asked[0].starts_with(r#"{"event":"preview_request","req":"#) && asked[0].contains(r#""cols":"#), "{}", asked[0]);
    assert!(!daemon_log(&env).contains("Preview refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_plugin_under_its_profile_fills_its_panel_when_it_opens_and_hears_the_row_clicked() {
    let env = setup_env("pl-panel");
    let events = ["command", "panel_opened", "panel_item"];
    let mut m = test_plugin("lister", &events, &events, "hit");
    m["panels"] = serde_json::json!([{ "id": "jobs", "title": "Jobs", "where": "global" }]);
    let data = install_plugin(&env, "lister", m);
    std::fs::create_dir_all(&data).unwrap();
    let panel = r#"{"Panel":{"id":"jobs","kind":"list","title":"Jobs now","items":[{"id":"job-1","label":"first-job","opens":true},{"id":"job-2","label":"second-job","detail":"2 min","role":"ok"}]}}"#;
    std::fs::write(data.join("panel.jsonl"), format!("{panel}\n")).unwrap();
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, "plugin lister: connected", 1), "a panel plugin starts with the server: {}", daemon_log(&env));
    let mut client = connect(&env, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    // Its panel is the plugin group's last item, after its command.
    run_last_menu_item(&mut client);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("second-job") && s.contains("Jobs now"));
    assert!(ok, "the panel was not filled:\n{screen}\n{}", daemon_log(&env));
    let heard = || read(&data.join("events.log"));
    assert!(heard().contains(r#"{"event":"panel_opened","id":"jobs"}"#), "{}", heard());
    // A row that opens is heard and the panel stays for the plugin's next content.
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains("first-job")).unwrap();
    click(&mut client, line[..line.find("first-job").unwrap()].chars().count() as u16 + 2, y as u16);
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard().contains(r#"{"event":"panel_item","panel":"jobs","item":"job-1"}"#)), "{}", heard());
    let mut term = vt100::Parser::new(30, 160, 0);
    ClientMsg::Event(Event::Resize(160, 30)).write_to(&mut client).unwrap();
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("second-job") && s.contains("Jobs now"));
    assert!(ok, "a row that opens keeps the panel:\n{screen}");
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains("second-job")).unwrap();
    click(&mut client, line[..line.find("second-job").unwrap()].chars().count() as u16 + 2, y as u16);
    assert!(
        wait_reading(&mut client, Duration::from_secs(10), || heard().contains(r#"{"event":"panel_item","panel":"jobs","item":"job-2"}"#)),
        "the chosen row was not heard: {}\n{}",
        heard(),
        daemon_log(&env)
    );
    // The bytes read while waiting are gone, so a resize redraws it all.
    let mut term = vt100::Parser::new(30, 160, 0);
    ClientMsg::Event(Event::Resize(160, 30)).write_to(&mut client).unwrap();
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("spaces") && !s.contains("second-job"));
    assert!(ok, "chosen, the panel closes:\n{screen}");
    assert!(!daemon_log(&env).contains("Panel refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

/// A templates/<folder> slot example, installed the way a user would: its folder renamed to the
/// manifest's own name. Returns that name and its plugin-data folder.
fn install_template(env: &Env, folder: &str) -> (String, PathBuf) {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates").join(folder);
    let text = std::fs::read_to_string(src.join("plugin.json")).unwrap();
    let name = serde_json::from_str::<serde_json::Value>(&text).unwrap()["name"].as_str().unwrap().to_string();
    let dst = env.dir.join("plugins").join(&name);
    std::fs::create_dir_all(&dst).unwrap();
    for f in ["plugin.json", "plugin.sh"] {
        std::fs::copy(src.join(f), dst.join(f)).unwrap();
    }
    let data = env.dir.join("plugin-data").join(&name);
    (name, data)
}

/// A folder outside nerd's state root, which a plugin's sandbox may read; `env.dir` itself is denied.
fn outside_dir(env: &Env, suffix: &str) -> PathBuf {
    let d = env.dir.with_file_name(format!("{}-{suffix}", env.dir.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Opens the menu of the pane under the middle of the screen with a right click, and clicks the row holding `label`.
fn click_pane_menu_item(client: &mut UnixStream, term: &mut vt100::Parser, label: &str) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (rows, cols) = term.screen().size();
    // A right click that lands before the first frame is drawn is dropped, so it is sent again.
    let mut shown = (false, String::new());
    for _ in 0..4 {
        for kind in [MouseEventKind::Down(MouseButton::Right), MouseEventKind::Up(MouseButton::Right)] {
            ClientMsg::Event(Event::Mouse(MouseEvent { kind, column: cols * 2 / 3, row: rows / 2, modifiers: KeyModifiers::NONE })).write_to(&mut *client).unwrap();
        }
        shown = screen_until(client, term, Duration::from_secs(5), |s| s.contains(label));
        if shown.0 {
            break;
        }
    }
    let (ok, screen) = shown;
    assert!(ok, "pane menu item {label:?} not on screen:\n{screen}");
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains(label)).unwrap();
    click(client, line[..line.find(label).unwrap()].chars().count() as u16 + 2, y as u16);
}

#[test]
fn the_badges_template_marks_big_files_in_the_tree_and_the_tab() {
    let env = setup_env("tpl-badges");
    let (name, _data) = install_template(&env, "badges");
    let files = outside_dir(&env, "files");
    std::fs::write(files.join("large.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    std::fs::write(files.join("small.txt"), "tiny\n").unwrap();
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, &format!("plugin {name}: connected"), 1), "not started with the server: {}", daemon_log(&env));
    let mut client = connect_in(&env, &files, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| r.contains("1 big"));
    assert!(ok, "no tab badge: {row:?}\n{}", daemon_log(&env));
    key(&mut client, KeyCode::Char('t'), KeyModifiers::ALT);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("large.bin") && s.contains("small.txt"));
    assert!(ok, "the tree did not draw:\n{screen}\n{}", daemon_log(&env));
    // The preview header on the same row also names the selected file; only the tree's own row lacks " . ".
    let big_row = screen.lines().find(|l| l.contains("large.bin") && !l.contains('\u{b7}')).unwrap();
    let small_row = screen.lines().find(|l| l.contains("small.txt") && !l.contains('\u{b7}')).unwrap();
    assert!(big_row.contains("big"), "the big file has no badge: {big_row:?}");
    assert!(!small_row.contains("big"), "the small file was marked too: {small_row:?}");
    assert!(!daemon_log(&env).contains("Badges refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
    let _ = std::fs::remove_dir_all(&files);
}

#[test]
fn the_segment_template_shows_the_clock_and_a_click_refreshes_it() {
    let env = setup_env("tpl-segment");
    let (name, data) = install_template(&env, "segment");
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, &format!("plugin {name}: connected"), 1), "not started with the server: {}", daemon_log(&env));
    let mut client = connect(&env, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let (ok, row) = top_until(&mut client, &mut term, Duration::from_secs(10), |r| r.contains(':'));
    assert!(ok, "no clock segment: {row:?}\n{}", daemon_log(&env));
    let col = row.chars().position(|c| c == ':').map(|i| i.saturating_sub(1)).unwrap_or(0) as u16;
    click(&mut client, col, 0);
    let heard = || read(&data.join("events.log")).matches("\"id\":\"refresh\"").count();
    assert!(wait_reading(&mut client, Duration::from_secs(10), || heard() == 1), "a click on the segment ran nothing: {}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn the_preview_template_shows_only_error_and_warn_lines_of_a_log_file() {
    let env = setup_env("tpl-preview");
    let (name, data) = install_template(&env, "preview");
    let files = outside_dir(&env, "files");
    std::fs::write(files.join("app.log"), "2024-01-01 INFO starting\n2024-01-01 ERROR boom\n2024-01-01 WARN low disk\n2024-01-01 INFO done\n").unwrap();
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, &format!("plugin {name}: connected"), 1), "a preview plugin starts with the server: {}", daemon_log(&env));
    let mut client = connect_in(&env, &files, 160, 30);
    key(&mut client, KeyCode::Char('t'), KeyModifiers::ALT);
    let asked = || read(&data.join("events.log")).lines().filter(|l| l.contains("preview_request")).map(String::from).collect::<Vec<_>>();
    let mut term = vt100::Parser::new(30, 160, 0);
    let t = Instant::now();
    while !asked().iter().any(|l| l.contains("app.log")) {
        assert!(t.elapsed() < Duration::from_secs(10), "never asked: {:?}\n{}", read(&data.join("events.log")), daemon_log(&env));
        key(&mut client, KeyCode::Down, KeyModifiers::NONE);
        screen_until(&mut client, &mut term, Duration::from_millis(300), |_| false);
    }
    // The plain preview has these lines too: the plugin's is the one whose header names it.
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains(&format!("· {name}")) && s.contains("ERROR boom") && s.contains("WARN low disk"));
    assert!(ok, "the filtered preview did not draw:\n{screen}\n{}", daemon_log(&env));
    assert!(!screen.contains("INFO starting"), "an INFO line leaked into the preview:\n{screen}");
    assert!(!daemon_log(&env).contains("Preview refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
    let _ = std::fs::remove_dir_all(&files);
}

#[test]
fn the_panels_template_lists_a_pane_s_files_opens_one_in_place_and_groups_by_type() {
    let env = setup_env("tpl-panels");
    let (name, data) = install_template(&env, "panels");
    let files = outside_dir(&env, "files");
    std::fs::write(files.join("a.txt"), "hello from a\n").unwrap();
    std::fs::write(files.join("b.log"), "hello from b\n").unwrap();
    let _daemon = spawn_daemon(&env);
    assert!(wait_log(&env, &format!("plugin {name}: connected"), 1), "a panel plugin starts with the server: {}", daemon_log(&env));
    let mut client = connect_in(&env, &files, 160, 30);
    let mut term = vt100::Parser::new(30, 160, 0);
    let heard = || read(&data.join("events.log"));
    assert!(wait_reading(&mut client, Duration::from_secs(5), || heard().contains("pane_opened")), "no pane_opened heard: {}", daemon_log(&env));

    click_pane_menu_item(&mut client, &mut term, "Files here");
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("a.txt") && s.contains("b.log"));
    assert!(ok, "the list panel was not filled:\n{screen}\n{}", daemon_log(&env));
    assert!(heard().contains(r#""event":"panel_opened","id":"files","pane":"#), "opened from a pane's menu, it names the pane: {}", heard());
    let row = |screen: &str, label: &str| {
        let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains(label)).unwrap_or_else(|| panic!("no {label}:\n{screen}"));
        (line[..line.find(label).unwrap()].chars().count() as u16 + 2, y as u16)
    };
    let (x, y) = row(&screen, "a.txt");
    click(&mut client, x, y);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("hello from a") && s.contains("Back to the files"));
    assert!(ok, "the file opens in the same panel:\n{screen}\n{}", heard());
    let (x, y) = row(&screen, "Back to the files");
    click(&mut client, x, y);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("a.txt") && s.contains("b.log") && !s.contains("hello from a"));
    assert!(ok, "and back to the list:\n{screen}");
    // Esc closes it: the next menu would otherwise land in its filter box.
    key(&mut client, KeyCode::Esc, KeyModifiers::NONE);

    click_pane_menu_item(&mut client, &mut term, "By type");
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains(".txt") && s.contains(".log"));
    assert!(ok, "the tree panel was not filled:\n{screen}\n{}", daemon_log(&env));
    let (x, y) = row(&screen, ".log");
    click(&mut client, x, y);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("b.log") && !s.contains("hello from b"));
    assert!(ok, "the branch opens:\n{screen}");
    let (x, y) = row(&screen, "b.log");
    click(&mut client, x, y);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains("hello from b") && s.contains("Back to the files"));
    assert!(ok, "a file in the tree opens in the same panel:\n{screen}\n{}", heard());
    let (x, y) = row(&screen, "Back to the files");
    click(&mut client, x, y);
    let (ok, screen) = screen_until(&mut client, &mut term, Duration::from_secs(10), |s| s.contains(".txt") && !s.contains("hello from b"));
    assert!(ok, "and back to the tree:\n{screen}");
    assert!(!daemon_log(&env).contains("Panel refused"), "{}", daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
    let _ = std::fs::remove_dir_all(&files);
}

#[test]
fn a_plugin_installed_while_panes_are_open_hears_them_as_they_are() {
    let env = setup_env("pl-late");
    let _daemon = spawn_daemon(&env);
    let mut client = connect(&env, 100, 30);
    assert!(wait_for(&mut client, "spaces", Duration::from_secs(5)), "no frame: {}", daemon_log(&env));
    // Installed after the pane opened: a slot plugin, so it starts as soon as the lock tells the server.
    let mut manifest = test_plugin("late", &["command", "pane_opened"], &["pane_opened"], "late");
    manifest["badges"] = serde_json::json!(["tab"]);
    let data = install_plugin(&env, "late", manifest);
    std::fs::write(env.dir.join("plugins.lock"), r#"[{"name":"late","source":"https://example.com/late.git","commit":"0000000000000000000000000000000000000000"}]"#).unwrap();
    let log = data.join("events.log");
    assert!(wait_reading(&mut client, Duration::from_secs(15), || read(&log).contains("\"event\":\"pane_opened\",\"pane\":1")), "{:?}\n{}", read(&log), daemon_log(&env));
    let _ = std::fs::remove_dir_all(&env.dir);
}
