//! The messages socket: agents in panes send and read messages through it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::layout::PaneId;
use crate::transport::{Inbound, Inbox};

/// The first line, and a hook's body, arrive within this of the accept.
const FIRST: Duration = Duration::from_secs(5);
const LINE: u64 = 128 * 1024;
const IDLE: Duration = Duration::from_secs(600);
const HOOK_BYTES: usize = 256 * 1024;
const PUT_BYTES: usize = 64 * 1024;
const APPEND_BYTES: usize = 4096;
const CONNECTIONS: usize = 64;
const PER_PANE: usize = 8;
const SHARED: usize = 8;
/// Over this many `Hook`/`Append` calls of one pane within `BUDGET_WINDOW`, only state words pass.
const BUDGET: usize = 30;
const BUDGET_WINDOW: Duration = Duration::from_secs(3);
const STATE_WORDS: &[&str] = &["working", "idle", "blocked", "fail", "stop", "notify"];

pub const OUTSIDE: &str = "only panes inside nerd can send";
pub const STARTING: &str = "this pane is still starting; try again";

static INBOX: OnceLock<Inbox> = OnceLock::new();
static LIVE: AtomicUsize = AtomicUsize::new(0);
static LIVE_SHARED: AtomicUsize = AtomicUsize::new(0);
static PANES: Mutex<Option<HashMap<PaneId, usize>>> = Mutex::new(None);
static SPENT: Mutex<Option<HashMap<PaneId, VecDeque<Instant>>>> = Mutex::new(None);
static NO_AGENT: Mutex<Option<HashSet<PaneId>>> = Mutex::new(None);

/// One request line of the wire, after `Hello`.
#[derive(Deserialize, Debug)]
pub enum Request {
    Send {
        #[serde(default)]
        to: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        auto: bool,
        #[serde(default)]
        ask_route: bool,
    },
    Agents {},
    Inbox {
        #[serde(default)]
        all: bool,
    },
    Show {
        id: String,
    },
    Wait {
        id: String,
        #[serde(default)]
        thread: bool,
        #[serde(default = "one")]
        count: usize,
        #[serde(default = "ten_minutes")]
        timeout: u64,
    },
    PaneRights {},
    Hook {
        word: String,
        from: String,
        bytes: usize,
    },
    Put {
        file: String,
        data: String,
    },
    Append {
        file: String,
        line: String,
    },
    Drop {
        file: String,
    },
    Take {},
    /// The extension has the message `Take` gave it.
    Took {
        id: String,
    },
}

fn one() -> usize {
    1
}

fn ten_minutes() -> u64 {
    600
}

#[derive(Deserialize)]
enum First {
    Hello { protocol: u32 },
}

/// Removes a stale file, binds, chmod 0600.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    crate::plugins::bind(path)
}

/// The accept loop for `nerd.msock`.
pub fn start(own: UnixListener, inbox: Inbox) {
    let _ = INBOX.set(inbox.clone());
    accept(own, inbox, false);
}

/// The accept loop for the shared door admitted accounts reach; false before `start`.
pub fn start_shared(listener: UnixListener) -> bool {
    let Some(inbox) = INBOX.get() else { return false };
    accept(listener, inbox.clone(), true);
    true
}

/// Client side, for the CLI guards: what the pane's server says of `allow_from_panes`; any failure is false.
pub fn pane_rights() -> bool {
    let path = std::env::var_os("NERD_MSOCK").map(PathBuf::from).unwrap_or_else(crate::session::msock_path);
    ask_rights(&path).unwrap_or(false)
}

fn ask_rights(path: &Path) -> std::io::Result<bool> {
    let s = crate::transport::connect_own(path)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    (&s).write_all(b"{\"Hello\":{\"protocol\":1}}\n{\"PaneRights\":{}}\n")?;
    let mut lines = BufReader::new(&s).lines();
    let hello: Value = serde_json::from_str(&lines.next().transpose()?.unwrap_or_default())?;
    let rights: Value = serde_json::from_str(&lines.next().transpose()?.unwrap_or_default())?;
    Ok(hello.get("ok").is_some() && rights["allow_from_panes"] == Value::Bool(true))
}

/// Who may connect: the owner, and on the shared door also an admitted account.
fn admitted(uid: u32, shared: bool, owner: u32, accounts: &[crate::policy::Account]) -> bool {
    uid == owner || (shared && accounts.iter().any(|a| a.uid == uid))
}

fn accept(listener: UnixListener, inbox: Inbox, shared: bool) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (at, accepted) = (Instant::now(), wall_micros());
            let uid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerCred).ok().map(|c| c.uid());
            let Some(uid) = uid.filter(|u| admitted(*u, shared, crate::policy::owner_uid(), &crate::policy::current().accounts)) else {
                crate::log::warn_limited("message socket stranger", &format!("refused a message connection from uid {}", uid.map_or("unknown".into(), |u| u.to_string())));
                continue;
            };
            let Some(slot) = Slot::take(shared) else {
                crate::log::warn_limited("message socket full", &format!("refused a message connection: {CONNECTIONS} are open, or {SHARED} on the shared door"));
                continue;
            };
            let pid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerPid).ok();
            let started = pid.and_then(crate::transport::start_time);
            let inbox = inbox.clone();
            let spawned = std::thread::Builder::new().spawn(move || {
                let _slot = slot;
                serve(stream, &inbox, uid, pid, started, accepted, at);
            });
            if spawned.is_err() {
                crate::log::warn_limited("message socket no thread", "refused a message connection: no thread left for it");
            }
        }
    });
}

fn wall_micros() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

/// The pane a peer runs in: its own pid or an ancestor's is a pane's child pid.
/// `broker`: the broker's pid while some pane is starting; a process under it may be in that pane, so it is told to try again.
// This narrows the pid-reuse window between connect and accept; it does not close it.
pub(crate) fn identify(pid: i32, started: Option<u64>, accepted: u64, panes: &[(PaneId, i32)], broker: Option<i32>, parent: impl Fn(i32) -> Option<i32>) -> Result<PaneId, String> {
    if started.is_some_and(|s| s > accepted) {
        return Err("the sending process is gone".into());
    }
    let mut p = pid;
    for _ in 0..=32 {
        if let Some((pane, _)) = panes.iter().find(|(_, child)| *child == p) {
            return Ok(*pane);
        }
        if Some(p) == broker {
            return Err(STARTING.into());
        }
        match parent(p) {
            Some(pp) if pp > 1 => p = pp,
            _ => break,
        }
    }
    Err(OUTSIDE.into())
}

/// One of the connections, and of the shared door's share; given back when its thread ends.
struct Slot(bool);

impl Slot {
    fn take(shared: bool) -> Option<Slot> {
        if !take(&LIVE, CONNECTIONS) {
            return None;
        }
        if shared && !take(&LIVE_SHARED, SHARED) {
            LIVE.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(shared))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        if self.0 {
            LIVE_SHARED.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn take(counter: &AtomicUsize, cap: usize) -> bool {
    if counter.fetch_add(1, Ordering::SeqCst) >= cap {
        counter.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    true
}

/// One of a pane's `PER_PANE` connections.
struct PaneSlot(PaneId);

impl PaneSlot {
    fn take(pane: PaneId) -> Option<PaneSlot> {
        let mut g = PANES.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.get_or_insert_default().entry(pane).or_default();
        if *n >= PER_PANE {
            return None;
        }
        *n += 1;
        Some(PaneSlot(pane))
    }
}

impl Drop for PaneSlot {
    fn drop(&mut self) {
        let mut g = PANES.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_default();
        if let Some(n) = map.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.0);
            }
        }
    }
}

fn say(out: &UnixStream, v: &Value) -> std::io::Result<()> {
    (&*out).write_all(format!("{v}\n").as_bytes())
}

fn error(why: impl Into<String>) -> Value {
    json!({ "error": why.into() })
}

fn serve(stream: UnixStream, inbox: &Inbox, uid: u32, pid: Option<i32>, started: Option<u64>, accepted: u64, at: Instant) {
    let Ok(copy) = stream.try_clone() else { return };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    // A deadline for the whole line, not per read: dripping a byte at a time does not keep it open.
    let _ = copy.set_read_timeout(Some(Duration::from_millis(200)));
    let mut reader = BufReader::new(copy);
    let until = at + FIRST;
    let hello = match crate::trial::next_line(&mut reader, until, LINE) {
        crate::trial::Read::Line(l) if l.ends_with('\n') => serde_json::from_str::<First>(&l).ok().filter(|First::Hello { protocol }| *protocol == 1),
        _ => None,
    };
    if hello.is_none() {
        let _ = say(&stream, &error("the first line is {\"Hello\":{\"protocol\":1}}, within 5 s"));
        return;
    }
    let Some(pid) = pid else {
        let _ = say(&stream, &error(OUTSIDE));
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if inbox.send(Inbound::MailHello { pid, uid, started, accepted, reply: tx }).is_err() {
        return;
    }
    let (pane, name) = match rx.recv_timeout(FIRST) {
        Ok(Ok(found)) => found,
        Ok(Err(why)) => {
            let _ = say(&stream, &error(why));
            return;
        }
        Err(_) => {
            let _ = say(&stream, &error("the server did not answer; try again"));
            return;
        }
    };
    let Some(_pane_slot) = PaneSlot::take(pane) else {
        crate::log::warn_limited("message socket pane full", &format!("refused a message connection from pane {pane}: it holds {PER_PANE}"));
        let _ = say(&stream, &error(format!("this pane already holds {PER_PANE} connections")));
        return;
    };
    if say(&stream, &json!({ "ok": { "protocol": 1, "pane": pane, "name": name } })).is_err() {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(Some(IDLE));
    loop {
        let mut line = Vec::new();
        match (&mut reader).take(LINE + 1).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.last() != Some(&b'\n') => {
                let _ = say(&stream, &error(format!("a request line over {LINE} bytes")));
                return;
            }
            Ok(_) => {}
        }
        let req = match serde_json::from_slice::<Request>(&line) {
            Ok(r) => r,
            Err(e) => {
                if say(&stream, &error(format!("not a request: {e}"))).is_err() {
                    return;
                }
                continue;
            }
        };
        match here(pane, req, &mut reader, until) {
            Done::Reply(v) => {
                if say(&stream, &v).is_err() {
                    return;
                }
            }
            Done::Close(v) => {
                let _ = say(&stream, &v);
                return;
            }
            Done::Loop(req) => {
                if !forward(&stream, inbox, pane, uid, req) {
                    return;
                }
            }
        }
    }
}

enum Done {
    Reply(Value),
    /// The stream can no longer be read in step: answered, then closed.
    Close(Value),
    Loop(Request),
}

/// The requests the connection's thread answers itself; the rest go to the loop.
fn here(pane: PaneId, req: Request, reader: &mut BufReader<UnixStream>, until: Instant) -> Done {
    match req {
        Request::PaneRights {} => Done::Reply(json!({ "allow_from_panes": crate::policy::current().allow_from_panes })),
        Request::Hook { word, from, bytes } => {
            if bytes > HOOK_BYTES {
                return Done::Close(error(format!("a hook body over {HOOK_BYTES} bytes")));
            }
            let Some(payload) = read_body(reader, bytes, until) else {
                return Done::Close(error("the hook body did not arrive within 5 s of the connection"));
            };
            if !crate::agents::WORDS.contains(&word.as_str()) {
                return Done::Reply(error(format!("{word:?} is not a hook word")));
            }
            if !(1..=64).contains(&from.len()) || !from.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
                return Done::Reply(error("from is 1-64 of a-z, 0-9 and -"));
            }
            if !spend(pane, STATE_WORDS.contains(&word.as_str()), Instant::now()) {
                return Done::Reply(over_budget(pane));
            }
            Done::Reply(match crate::hook::run_for(pane, &word, &from, &payload) {
                Ok(out) => json!({ "out": out }),
                Err(e) => error(e.to_string()),
            })
        }
        Request::Put { file, data } => Done::Reply(answer(put(pane, &file, &data))),
        Request::Append { file, line } => {
            if !spend(pane, false, Instant::now()) {
                return Done::Reply(over_budget(pane));
            }
            Done::Reply(answer(append(pane, &file, &line)))
        }
        Request::Drop { file } => Done::Reply(answer(file_of(pane, &file).and_then(|p| match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }))),
        Request::Take {} => Done::Reply(take_mail(pane)),
        Request::Took { id } => Done::Reply(if crate::mail::mailbox_took(pane, &id) { json!({ "ok": {} }) } else { error(format!("no claimed message {id}")) }),
        other => Done::Loop(other),
    }
}

fn answer(r: Result<(), String>) -> Value {
    match r {
        Ok(()) => json!({ "ok": {} }),
        Err(e) => error(e),
    }
}

fn over_budget(pane: PaneId) -> Value {
    crate::log::warn_limited("message socket hook budget", &format!("pane {pane}: over {BUDGET} hook calls within 3 s; only state words pass"));
    error(format!("over {BUDGET} hook calls within 3 s; only state words pass"))
}

/// Exactly `n` bytes by `until`, or None.
fn read_body(reader: &mut BufReader<UnixStream>, n: usize, until: Instant) -> Option<Vec<u8>> {
    let _ = reader.get_ref().set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        if Instant::now() >= until {
            return None;
        }
        match reader.read(&mut buf[got..]) {
            Ok(0) => return None,
            Ok(k) => got += k,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(_) => return None,
        }
    }
    let _ = reader.get_ref().set_read_timeout(Some(IDLE));
    Some(buf)
}

/// Counts a `Hook` or `Append` call; false when over the budget, unless `always`.
fn spend(pane: PaneId, always: bool, now: Instant) -> bool {
    let mut g = SPENT.lock().unwrap_or_else(|e| e.into_inner());
    let q = g.get_or_insert_default().entry(pane).or_default();
    let over = q.len() >= BUDGET && q.front().is_some_and(|t| now.duration_since(*t) <= BUDGET_WINDOW);
    q.push_back(now);
    if q.len() > BUDGET {
        q.pop_front();
    }
    always || !over
}

/// The verified pane's own file under the hook's folder.
fn file_of(pane: PaneId, file: &str) -> Result<PathBuf, String> {
    let suffix = match file {
        "" => "",
        "session" => ".session",
        "events" => ".events",
        "subagents" => ".subagents",
        "tasks" => ".tasks",
        _ => return Err(format!("{file:?} is not one of \"\", session, events, subagents, tasks")),
    };
    let dir = crate::hook::state_dir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(format!("{pane}{suffix}")))
}

fn put(pane: PaneId, file: &str, data: &str) -> Result<(), String> {
    if data.len() > PUT_BYTES {
        return Err(format!("data over {PUT_BYTES} bytes"));
    }
    let path = file_of(pane, file)?;
    // Each file its own temp name: `<pane>.tmp` would be shared by all of them.
    let tmp = if file.is_empty() { "tmp".to_string() } else { format!("{file}.tmp") };
    crate::paths::write_private(&path, &tmp, data.as_bytes()).map_err(|e| e.to_string())
}

/// One line; past 800 lines the last 400 are kept after a `cut` line, as the hook script does.
fn append(pane: PaneId, file: &str, line: &str) -> Result<(), String> {
    if line.len() > APPEND_BYTES || line.contains('\n') {
        return Err(format!("a line is at most {APPEND_BYTES} bytes, with no newline"));
    }
    let path = file_of(pane, file)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let ends: Vec<usize> = bytes.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i).collect();
    if ends.len() > 800 {
        let mut kept = format!("{}\t-\tcut\n", crate::hook::now()).into_bytes();
        kept.extend_from_slice(&bytes[ends[ends.len() - 401] + 1..]);
        let tmp = if file.is_empty() { "tmp".to_string() } else { format!("{file}.tmp") };
        crate::paths::write_private(&path, &tmp, &kept).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn take_mail(pane: PaneId) -> Value {
    if !crate::hook::state_dir().join(pane.to_string()).exists() && NO_AGENT.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().insert(pane) {
        crate::log::warn(&format!("pane {pane}: socket mode reports no agent"));
    }
    match crate::mail::mailbox_take(pane, "json") {
        Some((id, bytes)) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => json!({ "mail": v }),
            Err(e) => error(format!("mail {id}: {e}")),
        },
        None => json!({ "none": {} }),
    }
}

/// A loop request: its lines go back until the loop drops its end; false when the peer is gone.
fn forward(out: &UnixStream, inbox: &Inbox, pane: PaneId, uid: u32, req: Request) -> bool {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if inbox.send(Inbound::Mail { pane, uid, req, reply: tx }).is_err() {
        return false;
    }
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(line) => {
                if (&*out).write_all(line.as_bytes()).is_err() {
                    return false;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return true,
            // A `Wait` whose peer left gives its slot back now, not at its timeout.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let mut b = [0u8; 1];
                if let Ok(0) = nix::sys::socket::recv(out.as_raw_fd(), &mut b, nix::sys::socket::MsgFlags::MSG_PEEK | nix::sys::socket::MsgFlags::MSG_DONTWAIT) {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Account;

    #[test]
    fn the_own_door_admits_only_the_owner() {
        let scout = [Account { name: "scout".into(), uid: 502 }];
        assert!(admitted(501, false, 501, &scout));
        assert!(!admitted(502, false, 501, &scout), "an admitted account uses the shared door only");
        assert!(!admitted(0, false, 501, &[]), "not even root");
    }

    #[test]
    fn the_shared_door_admits_the_owner_and_admitted_accounts() {
        let scout = [Account { name: "scout".into(), uid: 502 }];
        assert!(admitted(501, true, 501, &scout));
        assert!(admitted(502, true, 501, &scout));
        assert!(!admitted(503, true, 501, &scout));
        assert!(!admitted(0, true, 501, &scout));
    }

    fn chain(links: &[(i32, i32)]) -> impl Fn(i32) -> Option<i32> + use<> {
        let map: HashMap<i32, i32> = links.iter().copied().collect();
        move |p| map.get(&p).copied()
    }

    #[test]
    fn a_recycled_pid_is_refused() {
        let parent = chain(&[(40, 30), (30, 1)]);
        let accepted = 1_000_000;
        assert_eq!(identify(40, Some(accepted + 1), accepted, &[(3, 30)], None, &parent), Err("the sending process is gone".into()));
        assert_eq!(identify(40, Some(accepted), accepted, &[(3, 30)], None, &parent), Ok(3));
        assert_eq!(identify(40, None, accepted, &[(3, 30)], None, &parent), Ok(3), "another account's process cannot be read");
        let now = wall_micros();
        assert!(crate::transport::start_time(std::process::id() as i32).is_some_and(|s| s <= now), "this process started before now");
    }

    #[test]
    fn the_walk_finds_the_pane_through_a_wrapper_and_sudo() {
        // nc 90 -> sh 80 -> sudo 70 (root) -> zsh 60 -> wrapper 50 (the pane's child) -> broker 10
        let parent = chain(&[(90, 80), (80, 70), (70, 60), (60, 50), (50, 10), (10, 1)]);
        let panes = [(1, 20), (7, 50)];
        assert_eq!(identify(90, None, 0, &panes, None, &parent), Ok(7));
        assert_eq!(identify(50, None, 0, &panes, None, &parent), Ok(7), "the pane's own child");
        assert_eq!(crate::transport::parent_pid(1), Some(0), "a root process is read, as sudo in the chain would be");
    }

    #[test]
    fn a_process_in_no_pane_is_refused() {
        let parent = chain(&[(90, 80), (80, 1)]);
        assert_eq!(identify(90, None, 0, &[(1, 20)], None, &parent), Err(OUTSIDE.into()));
        let long: Vec<(i32, i32)> = (0..40).map(|i| (1000 + i, 1001 + i)).collect();
        assert_eq!(identify(1000, None, 0, &[(1, 1040)], None, chain(&long)), Err(OUTSIDE.into()), "at most 32 steps up");
    }

    #[test]
    fn a_starting_pane_says_try_again_only_to_the_brokers_descendants() {
        // nc 90 -> zsh 80 -> broker 10, the pane's child pid not yet known; nc 95 -> launchd
        let parent = chain(&[(90, 80), (80, 10), (10, 1), (95, 1)]);
        assert_eq!(identify(90, None, 0, &[(1, 20)], Some(10), &parent), Err(STARTING.into()));
        assert_eq!(identify(95, None, 0, &[(1, 20)], Some(10), &parent), Err(OUTSIDE.into()), "outside nerd while a pane starts");
        assert_eq!(identify(90, None, 0, &[(7, 80)], Some(10), &parent), Ok(7), "a started pane is found before the broker");
    }

    #[test]
    fn caps_refuse_the_ninth_connection_of_a_pane() {
        let pane = 90_001;
        let held: Vec<PaneSlot> = (0..PER_PANE).map(|_| PaneSlot::take(pane).expect("within the cap")).collect();
        assert!(PaneSlot::take(pane).is_none(), "the ninth");
        assert!(PaneSlot::take(pane + 1).is_some(), "another pane is not affected");
        drop(held);
        assert!(PaneSlot::take(pane).is_some(), "given back when the connections end");
    }

    /// A connection's reader with `bytes` already sent by the peer.
    fn reader_with(bytes: &[u8]) -> (BufReader<UnixStream>, UnixStream) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(bytes).unwrap();
        (BufReader::new(ours), theirs)
    }

    #[test]
    fn a_hook_body_over_256_kib_is_refused() {
        let (mut r, _peer) = reader_with(b"");
        let req = Request::Hook { word: "idle".into(), from: "codex".into(), bytes: HOOK_BYTES + 1 };
        assert!(matches!(here(90_010, req, &mut r, Instant::now() + FIRST), Done::Close(v) if v == error("a hook body over 262144 bytes")));
        let (mut r, _peer) = reader_with(b"abc");
        let req = Request::Hook { word: "idle".into(), from: "codex".into(), bytes: 4 };
        assert!(matches!(here(90_010, req, &mut r, Instant::now() + Duration::from_millis(300)), Done::Close(_)), "a body short of its size");
        let (mut r, _peer) = reader_with(b"{}");
        let req = Request::Hook { word: "nope".into(), from: "codex".into(), bytes: 2 };
        assert!(matches!(here(90_010, req, &mut r, Instant::now() + FIRST), Done::Reply(v) if v["error"].as_str().is_some_and(|e| e.contains("not a hook word"))));
    }

    #[test]
    fn the_hook_budget_keeps_state_words() {
        let pane = 90_020;
        let t = Instant::now();
        for i in 0..BUDGET {
            assert!(spend(pane, false, t + Duration::from_millis(i as u64)), "call {i}");
        }
        let t = t + Duration::from_millis(100);
        assert!(!spend(pane, false, t), "a tool word over the budget");
        assert!(spend(pane, true, t), "a state word always passes");
        assert!(!spend(pane, false, t), "Append too");
        assert!(spend(pane, false, t + Duration::from_secs(4)), "the window moved on");
    }

    #[test]
    fn take_claims_and_took_confirms() {
        crate::session::tests::with_temp_state_dir("msock-took", || {
            let go = |req| match here(7, req, &mut reader_with(b"").0, Instant::now()) {
                Done::Reply(v) => v,
                _ => panic!("not answered here"),
            };
            crate::mail::mailbox_put(7, "01A", "json", br#"{"id":"01A","frame":"[f]","text":"t"}"#).unwrap();
            assert_eq!(go(Request::Take {}), json!({"mail": {"id": "01A", "frame": "[f]", "text": "t"}}));
            let dir = crate::mail::mailbox(7);
            assert!(dir.join("01A.claim").exists() && !dir.join("01A.sent").exists(), "claimed, not yet the agent's");
            assert_eq!(go(Request::Took { id: "01A".into() }), json!({"ok": {}}));
            assert!(dir.join("01A.sent").exists());
            assert_eq!(go(Request::Took { id: "01A".into() }), error("no claimed message 01A"));
            assert_eq!(go(Request::Take {}), json!({"none": {}}));
        });
    }

    #[test]
    fn put_and_append_stay_in_the_verified_panes_files() {
        crate::session::tests::with_temp_state_dir("msock-files", || {
            let dir = crate::hook::state_dir();
            let ok = |req| matches!(here(7, req, &mut reader_with(b"").0, Instant::now()), Done::Reply(v) if v == json!({"ok": {}}));
            assert!(ok(Request::Put { file: "session".into(), data: "abc".into() }));
            assert_eq!(std::fs::read_to_string(dir.join("7.session")).unwrap(), "abc");
            assert!(ok(Request::Put { file: "".into(), data: "idle".into() }));
            assert_eq!(std::fs::read_to_string(dir.join("7")).unwrap(), "idle");
            for bad in ["../8", "8.session", "mail", "/etc/passwd"] {
                assert!(!ok(Request::Put { file: bad.into(), data: "x".into() }), "{bad}");
            }
            assert!(!ok(Request::Put { file: "session".into(), data: "x".repeat(PUT_BYTES + 1) }));
            for i in 0..801 {
                append(7, "events", &format!("{i}\tx\ttool")).unwrap();
            }
            let events = std::fs::read_to_string(dir.join("7.events")).unwrap();
            let lines: Vec<&str> = events.lines().collect();
            assert_eq!(lines.len(), 401, "a cut line and the last 400");
            assert!(lines[0].ends_with("\t-\tcut"), "{}", lines[0]);
            assert_eq!((lines[1], lines[400]), ("401\tx\ttool", "800\tx\ttool"));
            assert!(append(7, "events", "a\nb").is_err());
            assert!(ok(Request::Drop { file: "session".into() }));
            assert!(!dir.join("7.session").exists());
            let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            assert!(names.iter().all(|n| n == "7" || n.starts_with("7.")), "{names:?}");
        });
    }
}
