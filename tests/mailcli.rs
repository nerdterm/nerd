//! The message command line against a scripted fake of the message socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

struct Fake {
    dir: String,
    sock: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Answers each request line with the next script entry (lines joined by \n are streamed); after `Hook` it reads `bytes` more.
fn fake(name: &str, script: &[&str]) -> Fake {
    let dir = format!("/tmp/nt-s2b-{}-{name}", std::process::id());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = format!("{dir}/m.sock");
    let l = UnixListener::bind(&sock).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (s2, script) = (seen.clone(), script.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    std::thread::spawn(move || {
        let Ok((c, _)) = l.accept() else { return };
        let mut w = c.try_clone().unwrap();
        let mut r = BufReader::new(c);
        for reply in script {
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if let Some(n) = line.split("\"bytes\":").nth(1).and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next()).and_then(|t| t.parse::<usize>().ok()) {
                let mut body = vec![0; n];
                std::io::Read::read_exact(&mut r, &mut body).unwrap();
            }
            s2.lock().unwrap().push(line);
            let _ = writeln!(w, "{reply}");
        }
        let mut rest = String::new();
        let _ = r.read_line(&mut rest);
    });
    Fake { dir, sock, seen }
}

const HELLO: &str = r#"{"ok":{"protocol":1,"pane":3,"name":"planner"}}"#;

fn msg(id: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","thread":"{id}","reply_to":null,"from":{{"pane":{{"pane":7,"name":"reader","agent":"pi","uid":501}}}},"to":{{"pane":{{"pane":3,"name":"planner","agent":"claude","uid":501}}}},"text":"{text}","hops":["planner","reader"],"auto":false,"created":1,"state":"delivered","via":null,"events":[],"passed":false}}"#
    )
}

fn run(f: &Fake, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nerd"));
    c.args(args).env("NERD_MSOCK", &f.sock).env_remove("NERD_SOCKET").env_remove("NERD_STATE_DIR").env_remove("NERD_INSTANCE").env_remove("NERD_MSOCK_UID");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.stdin(Stdio::null()).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn send_prints_sent_and_exits_0() {
    let f = fake("sent", &[HELLO, r#"{"sent":{"id":"ID1","state":"queued"}}"#]);
    let o = run(&f, &["send", "reader", "find", "X"], &[]);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(0), "sent ID1\n"));
    assert!(f.seen.lock().unwrap()[1].contains(r#""text":"find X""#));
}

#[test]
fn a_changed_list_is_said_on_stderr_beside_the_output() {
    const SAID: &str = "whom this pane may write to changed; run nerd agents\n";
    let f = fake("changed-send", &[HELLO, r#"{"sent":{"id":"ID1","state":"queued"},"reachable_changed":true}"#]);
    let o = run(&f, &["send", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), out(&o).as_str(), err(&o).as_str()), (Some(0), "sent ID1\n", SAID));
    let f = fake("changed-inbox", &[HELLO, r#"{"messages":[],"reachable_changed":true}"#]);
    let o = run(&f, &["inbox"], &[]);
    assert_eq!((out(&o).as_str(), err(&o).as_str()), ("no messages\n", SAID));
    let f = fake("changed-wait", &[HELLO, "{\"reachable_changed\":true}\n{\"done\":{\"count\":0}}"]);
    let o = run(&f, &["wait", "ID"], &[]);
    assert_eq!((o.status.code(), err(&o).as_str()), (Some(0), SAID));
    let f = fake("changed-show-error", &[HELLO, r#"{"error":"no message X for this pane","reachable_changed":true}"#]);
    let o = run(&f, &["inbox", "--show", "X"], &[]);
    assert_eq!((o.status.code(), err(&o)), (Some(1), format!("{SAID}refused: no message X for this pane\n")), "an error still carries the word");
}

#[test]
fn held_exits_3_and_refused_exits_1() {
    let f = fake("held", &[HELLO, r#"{"held":{"id":"ID2","why":"route 1: a → b ask"}}"#]);
    let o = run(&f, &["send", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(3), "held ID2: route 1: a → b ask\n"));
    let f = fake("refused", &[HELLO, r#"{"refused":"no such pane"}"#]);
    let o = run(&f, &["send", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), err(&o).as_str()), (Some(1), "refused: no such pane\n"));
}

#[test]
fn send_wait_times_out_with_2() {
    let f = fake("wait", &[HELLO, r#"{"sent":{"id":"ID3","state":"queued"}}"#, r#"{"timeout":{}}"#]);
    let o = run(&f, &["send", "--wait", "1", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), out(&o).as_str(), err(&o).as_str()), (Some(2), "sent ID3\n", "no reply yet after 1 s\n"));
}

#[test]
fn send_wait_on_a_held_message_returns_3_without_waiting() {
    let f = fake("heldwait", &[HELLO, r#"{"held":{"id":"ID4","why":"ask"}}"#]);
    let o = run(&f, &["send", "--wait", "30", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(3), "held ID4: ask\n"));
    assert_eq!(f.seen.lock().unwrap().len(), 2);
}

#[test]
fn a_mismatched_owner_uid_is_refused() {
    let f = fake("uid", &[HELLO, r#"{"sent":{"id":"x","state":"queued"}}"#]);
    let o = run(&f, &["send", "reader", "hi"], &[("NERD_MSOCK_UID", "1")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("it is not nerd's"), "{}", err(&o));
    assert!(f.seen.lock().unwrap().iter().all(|l| !l.contains("Send")));
}

#[test]
fn unreachable_names_the_path() {
    let f = fake("gone", &[]);
    let missing = format!("{}/none.sock", f.dir);
    let o = run(&f, &["agents"], &[("NERD_MSOCK", &missing)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains(&format!("cannot reach nerd at {missing}")), "{}", err(&o));
}

#[test]
fn inbox_prints_framed_messages() {
    let reply = format!(r#"{{"messages":[{},{}]}}"#, msg("A1", "one"), msg("A2", "two"));
    let f = fake("inbox", &[HELLO, &reply]);
    let o = run(&f, &["inbox"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        out(&o),
        "[message from reader via nerd, id A1; reply: nerd send --reply A1 \"…\"]\none\n\n[message from reader via nerd, id A2; reply: nerd send --reply A2 \"…\"]\ntwo\n"
    );
}

#[test]
fn escape_sequences_from_the_socket_never_reach_the_terminal() {
    let reply = format!(r#"{{"messages":[{}]}}"#, msg("A1", r"\u001b]0;pwned\u0007one\u009b2J"));
    let f = fake("esc-inbox", &[HELLO, &reply]);
    let o = run(&f, &["inbox"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).ends_with("\n]0;pwnedone2J\n"), "{:?}", out(&o));
    let f = fake("esc-refused", &[HELLO, r#"{"refused":"no\u001b[2J such pane\r"}"#]);
    let o = run(&f, &["send", "reader", "hi"], &[]);
    assert_eq!((o.status.code(), err(&o).as_str()), (Some(1), "refused: no[2J such pane\n"));
}

#[test]
fn wait_streams_until_done() {
    let (a, b) = (format!(r#"{{"message":{}}}"#, msg("B1", "x")), format!(r#"{{"message":{}}}"#, msg("B2", "y")));
    let f = fake("stream", &[HELLO, &format!("{a}\n{b}\n{{\"done\":{{\"count\":2}}}}")]);
    let o = run(&f, &["wait", "ID", "--count", "2"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("id B1;") && out(&o).contains("id B2;") && out(&o).contains("x\n\n["), "{}", out(&o));
}

#[test]
fn hook_relay_prints_out_and_never_fails() {
    let f = fake("relay", &[HELLO, r#"{"out":"out"}"#]);
    let mut c = Command::new(env!("CARGO_BIN_EXE_nerd"));
    c.args(["--hook-relay", "idle", ""]).env("NERD_MSOCK", &f.sock).env_remove("NERD_MSOCK_UID").stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(b"{\"a\":1}").unwrap();
    let o = ch.wait_with_output().unwrap();
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(0), "out"));
    assert!(f.seen.lock().unwrap()[1].contains(r#""from":"claude""#));
    let g = fake("relay-none", &[]);
    let missing = format!("{}/none.sock", g.dir);
    let o = run(&g, &["--hook-relay", "idle"], &[("NERD_MSOCK", &missing)]);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(0), ""));
}
