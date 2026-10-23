//! Delivery: queued messages into idle agents' panes, each by its agent's way in.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::json;

use super::App;
use crate::agents::{MailForm, MailVia};
use crate::layout::PaneId;
use crate::mail::{Hold, Message, State, Via};
use crate::policy::{self, Action, Mode};
use crate::term::AgentState;

/// An idle pane whose hook or extension has not taken its file by then gets the message typed.
const TAKE_BACK: Duration = Duration::from_secs(3);
const CLAIM_LOST: Duration = Duration::from_secs(60);
const INBOX_TIMEOUT: Duration = Duration::from_secs(5);
/// Some TUIs do not submit a paste and Enter that arrive in one write.
const CR_AFTER: Duration = Duration::from_millis(150);
const SUBMIT_CHECK: Duration = Duration::from_secs(2);
const OWNER_QUIET: Duration = Duration::from_secs(10);
/// A pane whose hand-over failed is not tried again on every tick.
const RETRY: Duration = Duration::from_secs(5);
const CURRENT_FOR: Duration = Duration::from_secs(2);

/// `socket_fdinfo` (sys/proc_info.h): `psi.soi_kind`, and `psi.soi_proto.pri_un.unsi_addr.ua_sun.sun_path`
/// (`struct un_sockinfo`: two u64 handles, then a `sockaddr_un`, whose path follows its length and family bytes).
const SOI_KIND: usize = 256;
const UN_PATH: usize = 282;
const SUN_PATH_LEN: usize = 104;
const SOCKINFO_UN: i32 = 3;
const PROC_PIDFDSOCKETINFO: i32 = 3;

enum Way {
    Inbox(Receiver<std::io::Result<()>>),
    Hook,
    Extension,
    /// true once the carriage return went in.
    Typed(bool),
}

struct Hand {
    id: String,
    way: Way,
    at: Instant,
    idle: Option<Instant>,
    claimed: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct Delivery {
    started: bool,
    /// At most one hand-over in flight per pane.
    hands: HashMap<PaneId, Hand>,
    no_inbox: HashSet<PaneId>,
    failed_at: HashMap<PaneId, Instant>,
    /// Taken back from a hook or an extension: typed next.
    type_next: HashSet<String>,
    current: HashMap<String, (bool, Instant)>,
    warned_inbox: HashSet<PaneId>,
    warned_typed: HashSet<PaneId>,
}

impl App {
    pub(super) fn tick_deliver(&mut self) -> bool {
        let mut changed = false;
        if !self.mail.delivery.started {
            self.mail.delivery.started = true;
            changed = self.after_restart();
        }
        let mut done = HashSet::new();
        for pane in self.mail.delivery.hands.keys().copied().collect::<Vec<_>>() {
            let Some(hand) = self.mail.delivery.hands.remove(&pane) else { continue };
            let id = hand.id.clone();
            let before = self.mail.get(&id).map(|m| m.events.len());
            match self.poll(pane, hand) {
                Some(hand) => {
                    self.mail.delivery.hands.insert(pane, hand);
                }
                None => {
                    done.insert(pane);
                }
            }
            changed |= self.mail.get(&id).map(|m| m.events.len()) != before;
        }
        if !self.mail.msgs.iter().any(|m| m.state == State::Queued) {
            return changed;
        }
        let now = crate::hook::now();
        let mut pairs = HashSet::new();
        let queued: Vec<Message> = self.mail.msgs.iter().filter(|m| m.state == State::Queued).cloned().collect();
        for m in queued {
            if !pairs.insert((m.from.name().to_string(), m.to.name().to_string())) {
                continue;
            }
            let Some(pane) = m.to.pane() else { continue };
            let d = &self.mail.delivery;
            if done.contains(&pane) || d.hands.contains_key(&pane) || d.failed_at.get(&pane).is_some_and(|t| t.elapsed() < RETRY) {
                continue;
            }
            let Some(p) = self.pane(pane) else { continue };
            let (Some(agent), state) = (p.agent.clone(), p.term.state()) else { continue };
            if state == AgentState::Blocked {
                continue;
            }
            if self.stopped(&m, now) {
                changed = true;
                continue;
            }
            changed |= self.hand_over(pane, &agent, state, &m);
        }
        changed
    }

    /// The delivery re-check (F11): true when the policy as it is now holds or refuses `m`, which then has that state.
    pub(super) fn stopped(&mut self, m: &Message, now: u64) -> bool {
        let policy = policy::current();
        if !policy.on {
            return self.mail.set(&m.id, State::Held(Hold::Off), None, None);
        }
        let v = self.verdict_for(m, now);
        let stop = match v.action {
            Action::Allow => return false,
            Action::No if v.route.is_none() && policy.mode == Mode::Strict => State::Held(Hold::Mode),
            Action::No => State::Refused(v.why.clone()),
            Action::Ask if v.route.is_some() => State::Held(Hold::Route),
            Action::Ask => State::Held(Hold::CrossUid),
        };
        self.mail.set(&m.id, stop, None, Some(v.why))
    }

    /// A hand-over the previous run did not finish is made again, once: its frame's id makes a repeat recognisable.
    fn after_restart(&mut self) -> bool {
        let unfinished: Vec<String> = self.mail.msgs.iter().filter(|m| m.state == State::Delivered && m.via.is_none()).map(|m| m.id.clone()).collect();
        for id in &unfinished {
            self.mail.set(id, State::Queued, None, Some("after a restart".into()));
        }
        for e in std::fs::read_dir(crate::hook::state_dir()).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().ends_with(".mail") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        !unfinished.is_empty()
    }

    /// Some: still in flight.
    fn poll(&mut self, pane: PaneId, mut h: Hand) -> Option<Hand> {
        let Some(state) = self.pane(pane).map(|p| p.term.state()) else {
            for ext in ["out", "json"] {
                crate::mail::mailbox_void(pane, &h.id, ext);
            }
            self.requeue(&h.id, "the pane closed");
            return None;
        };
        match h.way {
            Way::Inbox(ref rx) => {
                let r = match rx.try_recv() {
                    Ok(r) => r,
                    Err(TryRecvError::Empty) if h.at.elapsed() < INBOX_TIMEOUT => return Some(h),
                    Err(TryRecvError::Empty) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer within 5 s")),
                    Err(TryRecvError::Disconnected) => Err(std::io::Error::other("the hand-over stopped")),
                };
                match r {
                    Ok(()) => self.handed(&h.id, Via::Inbox, None),
                    Err(e) => {
                        self.mail.delivery.no_inbox.insert(pane);
                        self.requeue(&h.id, &format!("inbox: {e}"));
                    }
                }
                None
            }
            Way::Hook | Way::Extension => {
                let (via, ext, who) = if matches!(h.way, Way::Hook) { (Via::Hook, "out", "hook") } else { (Via::Extension, "json", "extension") };
                if crate::mail::mailbox_sent(pane).contains(&h.id) {
                    self.handed(&h.id, via, Some(format!("claimed by the {who}")));
                    return None;
                }
                if crate::mail::mailbox(pane).join(format!("{}.claim", h.id)).exists() {
                    if h.claimed.get_or_insert_with(Instant::now).elapsed() >= CLAIM_LOST && crate::mail::mailbox_void(pane, &h.id, "claim") {
                        self.requeue(&h.id, &format!("the {who} claimed it and did not finish"));
                        return None;
                    }
                    return Some(h);
                }
                // The agent left and its mailbox went with it: no one will claim the file, and `void` cannot take it back.
                let gone = |e: &str| !crate::mail::mailbox(pane).join(format!("{}.{e}", h.id)).exists();
                if gone(ext) && gone("claim") && gone("sent") {
                    self.requeue(&h.id, &format!("the {who}'s mailbox was cleared"));
                    return None;
                }
                h.idle = if matches!(state, AgentState::Idle | AgentState::Done) { h.idle.or(Some(Instant::now())) } else { None };
                if h.idle.is_some_and(|t| t.elapsed() >= TAKE_BACK) && crate::mail::mailbox_void(pane, &h.id, ext) {
                    self.requeue(&h.id, &format!("the {who} did not take it"));
                    self.mail.delivery.type_next.insert(h.id);
                    return None;
                }
                Some(h)
            }
            Way::Typed(false) => {
                if h.at.elapsed() < CR_AFTER {
                    return Some(h);
                }
                self.submit_typed(pane);
                self.handed(&h.id, Via::Typed, None);
                Some(Hand { way: Way::Typed(true), at: Instant::now(), ..h })
            }
            Way::Typed(true) => {
                if h.at.elapsed() < SUBMIT_CHECK {
                    return Some(h);
                }
                let last = self.pane(pane).and_then(|p| p.term.screen().contents().lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string));
                if last.is_some_and(|l| l.contains(&h.id)) && self.mail.delivery.warned_typed.insert(pane) {
                    crate::log::warn(&format!("pane {pane}: a typed message was not submitted"));
                }
                None
            }
        }
    }

    fn submit_typed(&mut self, pane: PaneId) {
        if let Some(p) = self.pane_mut(pane) {
            p.pty.write(b"\r");
        }
    }

    /// Delivered by `via`; a message the agent read meanwhile (`nerd inbox`) keeps its state.
    fn handed(&mut self, id: &str, via: Via, note: Option<String>) {
        if self.mail.get(id).is_some_and(|m| m.state == State::Delivered) {
            self.mail.set(id, State::Delivered, Some(via), note);
        } else {
            self.mail.update(id, |m| m.via = Some(via));
        }
    }

    fn requeue(&mut self, id: &str, note: &str) {
        if self.mail.get(id).is_some_and(|m| m.state == State::Delivered) {
            self.mail.set(id, State::Queued, None, Some(note.into()));
            // The way it went is over: queued again, it counts as unread until the next hand-over.
            self.mail.update(id, |m| m.via = None);
        }
    }

    /// True: the record changed.
    fn hand_over(&mut self, pane: PaneId, agent: &str, state: AgentState, m: &Message) -> bool {
        let mail = crate::agents::get(agent).and_then(|a| a.agent.mail);
        let via = mail.as_ref().map(|x| x.via).filter(|_| !self.mail.delivery.type_next.contains(&m.id));
        let framed = crate::mail::framed(m);
        if via == Some(MailVia::Inbox)
            && let Some((path, root)) = self.own_inbox(pane)
        {
            self.mail.set(&m.id, State::Delivered, None, Some("inbox hand-over".into()));
            let line = format!("{}\n", json!({ "type": "user", "message": { "role": "user", "content": framed } }));
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || tx.send(send_inbox(&path, line.as_bytes(), root)));
            self.start(pane, &m.id, Way::Inbox(rx));
            return true;
        }
        if via.is_some() && self.hook_current(agent) {
            let (way, ext, who, bytes) = if via == Some(MailVia::Extension) {
                (Way::Extension, "json", "extension", json!({ "id": m.id, "frame": crate::mail::frame_line(m.from.name(), &m.id), "text": crate::mail::body(m) }))
            } else {
                let line = match mail.and_then(|x| x.form).unwrap_or(MailForm::Block) {
                    MailForm::Block => json!({ "decision": "block", "reason": framed }),
                    MailForm::Deny => json!({ "decision": "deny", "reason": framed }),
                    MailForm::Followup => json!({ "followup_message": framed }),
                };
                (Way::Hook, "out", "hook", line)
            };
            self.mail.set(&m.id, State::Delivered, None, Some(format!("{who} hand-over")));
            match crate::mail::mailbox_put(pane, &m.id, ext, format!("{bytes}\n").as_bytes()) {
                Ok(()) => self.start(pane, &m.id, way),
                Err(e) => {
                    self.requeue(&m.id, &format!("{who}: {e}"));
                    self.mail.delivery.failed_at.insert(pane, Instant::now());
                }
            }
            return true;
        }
        let owner = self.mail.owner_typed.is_some_and(|(p, t)| p == pane && t.elapsed() < OWNER_QUIET);
        if !matches!(state, AgentState::Idle | AgentState::Done) || owner || !self.pane(pane).is_some_and(|p| p.pty.agent_in_front(agent)) {
            return false;
        }
        self.mail.set(&m.id, State::Delivered, None, Some("typing hand-over".into()));
        // Without bracketed paste every newline would submit a line of its own: the message goes in as one line.
        let one_line = if self.pane(pane).is_some_and(|p| p.term.screen().bracketed_paste()) { framed } else { framed.replace("\r\n", "\n").replace(['\n', '\r'], " ⏎ ") };
        if !self.paste_into(pane, &one_line) {
            self.requeue(&m.id, "the pane closed");
            return true;
        }
        self.mail.delivery.type_next.remove(&m.id);
        self.start(pane, &m.id, Way::Typed(false));
        true
    }

    fn start(&mut self, pane: PaneId, id: &str, way: Way) {
        self.mail.delivery.hands.insert(pane, Hand { id: id.to_string(), way, at: Instant::now(), idle: None, claimed: None });
    }

    fn hook_current(&mut self, agent: &str) -> bool {
        let cache = &mut self.mail.delivery.current;
        if let Some((yes, at)) = cache.get(agent)
            && at.elapsed() < CURRENT_FOR
        {
            return *yes;
        }
        let yes = crate::hook::hook_current(agent);
        cache.insert(agent.to_string(), (yes, Instant::now()));
        yes
    }

    /// The inbox the pane's session recorded, only when a process of that pane is listening on it.
    /// With the pane's process the listener must descend from. Only an unisolated pane: under a profile the pane
    /// cannot reach every socket, and nerd must not write into one for it.
    fn own_inbox(&mut self, pane: PaneId) -> Option<(String, i32)> {
        let p = self.pane(pane)?;
        if !p.isolation.is_open() || self.mail.delivery.no_inbox.contains(&pane) {
            return None;
        }
        let path = std::fs::read_to_string(crate::hook::state_dir().join(format!("{pane}.inbox"))).ok()?.trim_end_matches('\n').to_string();
        if let Some(root) = p.pty.child_pid.map(|pid| pid as i32).filter(|root| inbox_of(&path, *root)) {
            return Some((path, root));
        }
        if self.mail.delivery.warned_inbox.insert(pane) {
            crate::log::warn(&format!("pane {pane}: its recorded inbox is not its own; delivering by hook"));
        }
        None
    }
}

/// The path may have been swapped since it was checked: the end actually reached must belong to the pane too.
fn send_inbox(path: &str, line: &[u8], root: i32) -> std::io::Result<()> {
    use std::io::Write;
    let mut s = std::os::unix::net::UnixStream::connect(path)?;
    let peer = nix::sys::socket::getsockopt(&s, nix::sys::socket::sockopt::LocalPeerPid).map_err(std::io::Error::from)?;
    if peer != root && !crate::transport::descendants(root, 256).contains(&peer) {
        return Err(std::io::Error::other(format!("the socket now answers from process {peer}, not this pane's")));
    }
    s.set_write_timeout(Some(INBOX_TIMEOUT))?;
    s.write_all(line)
}

/// A socket of the owner's at `path`, bound by `root` or a process below it.
fn inbox_of(path: &str, root: i32) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let Ok(md) = std::fs::symlink_metadata(path) else { return false };
    md.uid() == policy::owner_uid() && md.file_type().is_socket() && std::iter::once(root).chain(crate::transport::descendants(root, 256)).any(|pid| binds(pid, path.as_bytes()))
}

/// `pid` holds a unix socket bound at `path`.
fn binds(pid: i32, path: &[u8]) -> bool {
    use nix::libc::{PROC_PIDLISTFDS, PROX_FDTYPE_SOCKET, proc_fdinfo, proc_pidfdinfo, proc_pidinfo};
    if pid <= 0 || path.is_empty() || path.len() >= SUN_PATH_LEN {
        return false;
    }
    let size = std::mem::size_of::<proc_fdinfo>();
    // SAFETY: a null buffer asks only for the size; then the buffer is as long as the size passed.
    let need = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return false;
    }
    let mut fds: Vec<proc_fdinfo> = (0..need as usize / size + 16).map(|_| proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }).collect();
    let n = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), (fds.len() * size) as i32) };
    fds.truncate(n.max(0) as usize / size);
    fds.iter().filter(|f| f.proc_fdtype == PROX_FDTYPE_SOCKET as u32).any(|f| {
        let mut buf = [0u64; 128];
        // SAFETY: as above.
        let n = unsafe { proc_pidfdinfo(pid, f.proc_fd, PROC_PIDFDSOCKETINFO, buf.as_mut_ptr().cast(), 1024) };
        // SAFETY: plain bytes of a buffer we own.
        let b: &[u8; 1024] = unsafe { &*buf.as_ptr().cast() };
        let kind = i32::from_ne_bytes(b[SOI_KIND..SOI_KIND + 4].try_into().unwrap());
        n as usize >= UN_PATH + SUN_PATH_LEN && kind == SOCKINFO_UN && b[UN_PATH..UN_PATH + SUN_PATH_LEN].split(|c| *c == 0).next() == Some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Layout, Space, Tab};
    use crate::config::Config;
    use crate::mail::{Party, Step};
    use std::path::Path;

    /// A state folder, a HOME and a policy of their own; `f` gets pane 1 "planner" and pane 2 "reader" running
    /// `agent`, whose typed bytes land in the file it is given.
    fn with_app(tag: &str, agent: &str, state: AgentState, f: impl FnOnce(&mut App, &Path, &Path)) {
        with_front(tag, agent, agent, state, f);
    }

    /// As `with_app`, with the program in front of pane 2's terminal named `front`.
    fn with_front(tag: &str, agent: &str, front_name: &str, state: AgentState, f: impl FnOnce(&mut App, &Path, &Path)) {
        let _l = policy::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        policy::replace(policy::Policy::default());
        crate::session::tests::with_temp_state_dir(tag, || {
            let home = crate::paths::test_dir(&format!("deliver-{tag}"));
            let old = crate::paths::TEST_HOME.with(|h| h.replace(Some(home.clone())));
            let mut app = App::new(Config::default());
            let typed = home.join("typed");
            let (master, mut front) = terminal(&typed, front_name);
            let (mut pty, _output) = crate::pty::Pty::fake(2, master);
            // the shell is this test process; the program in front of it is a `sleep` named `front_name`
            pty.child_pid = Some(std::process::id());
            let mut reader = crate::app::snapshots::agent_pane(&app, 2, state, 0);
            (reader.pty, reader.label, reader.agent) = (pty, Some("reader".into()), Some(agent.into()));
            let mut planner = crate::app::snapshots::agent_pane(&app, 1, AgentState::Idle, 0);
            planner.label = Some("planner".into());
            let mut layout = Layout::new(1);
            layout.split(crate::layout::Dir::Horizontal, 2);
            let tab = Tab { name: None, layout, panes: HashMap::from([(1, planner), (2, reader)]), isolation_new: None };
            app.spaces.push(Space { name: Some("work".into()), cwd: "/".into(), tabs: vec![tab], active_tab: 0, isolation_new: None });
            app.next_id = 3;
            f(&mut app, &home, &typed);
            let _ = front.kill();
            let _ = front.wait();
            crate::paths::TEST_HOME.with(|h| h.replace(old));
            let _ = std::fs::remove_dir_all(&home);
        });
        policy::replace(policy::Policy::default());
    }

    /// A real terminal in raw mode, whose foreground is a `sleep` called `name` in a session of its own; what is
    /// typed into the master is copied to `typed`.
    fn terminal(typed: &Path, name: &str) -> (std::fs::File, std::process::Child) {
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;
        let pty = nix::pty::openpty(None, None).unwrap();
        let mut t = nix::sys::termios::tcgetattr(&pty.slave).unwrap();
        nix::sys::termios::cfmakeraw(&mut t);
        nix::sys::termios::tcsetattr(&pty.slave, nix::sys::termios::SetArg::TCSANOW, &t).unwrap();
        let slave = std::fs::File::from(pty.slave);
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg0(name).arg("600").stdin(slave.try_clone().unwrap()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        // SAFETY: only async-signal-safe calls after the fork.
        unsafe {
            cmd.pre_exec(|| {
                if nix::libc::setsid() < 0 || nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let front = cmd.spawn().unwrap();
        let mut out = std::fs::File::create(typed).unwrap();
        std::thread::spawn(move || {
            let (mut slave, mut buf) = (slave, [0u8; 4096]);
            while let Ok(n @ 1..) = slave.read(&mut buf) {
                let _ = out.write_all(&buf[..n]);
            }
        });
        (std::fs::File::from(pty.master), front)
    }

    fn queue(app: &mut App) -> String {
        queue_text(app, "find X")
    }

    fn queue_text(app: &mut App, text: &str) -> String {
        let (id, uid, now) = (crate::mail::new_id(), policy::owner_uid(), crate::hook::now());
        app.mail.add(Message {
            thread: id.clone(),
            id: id.clone(),
            reply_to: None,
            from: Party::Pane { pane: Some(1), name: "planner".into(), agent: Some("claude".into()), uid },
            to: Party::Pane { pane: Some(2), name: "reader".into(), agent: None, uid },
            text: text.into(),
            hops: vec!["planner".into(), "reader".into()],
            auto: false,
            created: now,
            state: State::Queued,
            via: None,
            events: vec![Step { state: State::Queued, at: now, note: None }],
            passed: false,
        });
        id
    }

    fn rec(app: &App, id: &str) -> Message {
        app.mail.get(id).unwrap().clone()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The pane's writer runs on a thread of its own.
    fn read_until(path: &Path, done: impl Fn(&str) -> bool) -> String {
        for _ in 0..200 {
            if done(&read(path)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        read(path)
    }

    fn install_hook(home: &Path, dir: &str, agent: &str) {
        std::fs::create_dir_all(home.join(dir)).unwrap();
        crate::hook::install().unwrap();
        assert!(crate::hook::hook_current(agent), "{agent}'s hook is current");
    }

    fn notes(m: &Message) -> Vec<(State, Option<String>)> {
        m.events.iter().map(|s| (s.state.clone(), s.note.clone())).collect()
    }

    fn log() -> String {
        read(&crate::session::state_dir().join("daemon.log"))
    }

    #[test]
    fn typing_pastes_then_sends_cr_after_150_ms() {
        with_app("typing", "codex", AgentState::Idle, |app, _, typed| {
            let id = queue(app);
            let framed = crate::mail::framed(&rec(app, &id)).replace('\n', " \u{23ce} ");
            assert!(app.tick_deliver());
            assert_eq!(read_until(typed, |s| s.len() >= framed.len()), framed);
            assert_eq!((rec(app, &id).state, rec(app, &id).via), (State::Delivered, None));
            app.tick_deliver();
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(read(typed), framed, "no carriage return before 150 ms");
            std::thread::sleep(Duration::from_millis(150));
            app.tick_deliver();
            assert_eq!(read_until(typed, |s| s.ends_with('\r')), format!("{framed}\r"), "the carriage return as a write of its own");
            assert_eq!(rec(app, &id).via, Some(Via::Typed));
        });
    }

    #[test]
    fn the_shell_in_front_is_never_typed_into() {
        with_app("shell-front", "codex", AgentState::Idle, |app, _, typed| {
            let id = queue(app);
            let front = app.pane(2).unwrap().pty.foreground().expect("the terminal has a foreground");
            // the agent left a moment ago: the probe still names it, and the shell has the terminal back
            app.pane_mut(2).unwrap().pty.child_pid = Some(front as u32);
            assert!(!app.tick_deliver());
            std::thread::sleep(Duration::from_millis(60));
            assert_eq!((read(typed), rec(app, &id).state), (String::new(), State::Queued));
            app.pane_mut(2).unwrap().pty.child_pid = Some(std::process::id());
            assert!(app.tick_deliver());
            assert!(read_until(typed, |s| s.contains(&id)).contains(&id), "typed once a program is in front again");
        });
    }

    #[test]
    fn a_program_other_than_the_agent_in_front_is_never_typed_into() {
        for front in ["less", "vim", "claude"] {
            with_front(&format!("other-front-{front}"), "codex", front, AgentState::Idle, |app, _, typed| {
                let id = queue(app);
                assert!(!app.tick_deliver(), "{front}");
                std::thread::sleep(Duration::from_millis(60));
                assert_eq!((read(typed), rec(app, &id).state), (String::new(), State::Queued), "{front}");
            });
        }
    }

    #[test]
    fn a_paste_while_a_window_is_open_goes_into_its_field_and_never_into_the_pane() {
        with_app("paste-window", "codex", AgentState::Idle, |app, _, typed| {
            app.spaces[0].tabs[0].layout.focus(2);
            app.overlay = app.run_action(crate::app::Action::SearchAll, &crate::app::MenuCtx::Global);
            app.paste("needle\x1b[201~ here\nrm -r ~/x\n");
            let crate::app::Overlay::Find(f) = &app.overlay else { panic!("Search stays") };
            assert_eq!(f.query, "needle[201~ here", "the first line, its controls dropped");
            app.overlay = app.run_action(crate::app::Action::RenamePane, &crate::app::MenuCtx::Pane(2));
            app.paste("boss\rignored");
            let crate::app::Overlay::Prompt(p, _) = &app.overlay else { panic!("the field stays") };
            assert!(p.value.ends_with("boss"), "{}", p.value);
            app.overlay = app.run_action(crate::app::Action::Settings, &crate::app::MenuCtx::Global);
            app.paste("dropped\n");
            std::thread::sleep(Duration::from_millis(60));
            assert_eq!(read(typed), "", "nothing reached the pane");
            app.overlay = crate::app::Overlay::None;
            app.paste("ok");
            assert_eq!(read_until(typed, |s| s.contains("ok")), "ok", "with no window the pane gets it");
        });
    }

    #[test]
    fn a_message_goes_in_as_one_line_without_bracketed_paste() {
        for bracketed in [false, true] {
            with_app("one-line", "codex", AgentState::Idle, |app, _, typed| {
                if bracketed {
                    app.pane_mut(2).unwrap().term.feed(b"\x1b[?2004h");
                }
                let id = queue_text(app, "line one\nline two");
                let m = rec(app, &id);
                let want = if bracketed {
                    format!("\x1b[200~{}\x1b[201~", crate::mail::framed(&m))
                } else {
                    format!("{} \u{23ce} line one \u{23ce} line two", crate::mail::frame_line("planner", &id))
                };
                assert!(app.tick_deliver());
                assert_eq!(read_until(typed, |s| s.len() >= want.len()), want, "bracketed {bracketed}");
            });
        }
    }

    #[test]
    fn a_blocked_pane_is_never_typed_into() {
        with_app("blocked", "codex", AgentState::Blocked, |app, _, typed| {
            let id = queue(app);
            for _ in 0..3 {
                assert!(!app.tick_deliver());
                std::thread::sleep(Duration::from_millis(60));
            }
            assert_eq!(read(typed), "");
            assert_eq!(rec(app, &id).state, State::Queued);
        });
    }

    #[test]
    fn the_owner_typing_holds_a_typed_message_for_10_s() {
        with_app("owner", "codex", AgentState::Idle, |app, _, typed| {
            let id = queue(app);
            app.mail.owner_typed = Some((2, Instant::now()));
            assert!(!app.tick_deliver());
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!((read(typed), rec(app, &id).state), (String::new(), State::Queued));
            app.mail.owner_typed = Some((2, Instant::now().checked_sub(Duration::from_secs(11)).unwrap()));
            assert!(app.tick_deliver());
            assert!(read_until(typed, |s| s.contains(&id)).contains(&id));
        });
    }

    #[test]
    fn delivered_is_written_before_the_hand_over_and_queued_again_on_failure() {
        with_app("failed", "codex", AgentState::Working, |app, home, _| {
            install_hook(home, ".codex", "codex");
            std::fs::create_dir_all(crate::hook::state_dir()).unwrap();
            std::fs::write(crate::mail::mailbox(2), "a file where the mailbox goes").unwrap();
            let id = queue(app);
            assert!(app.tick_deliver());
            let n = notes(&rec(app, &id));
            assert_eq!(n[1], (State::Delivered, Some("hook hand-over".into())));
            assert!(n[2].0 == State::Queued && n[2].1.as_deref().is_some_and(|e| e.starts_with("hook: ")), "{n:?}");
            assert!(!app.tick_deliver(), "a failed pane is not tried again at once");
            assert_eq!(rec(app, &id).events.len(), 3);
        });
    }

    #[test]
    fn a_hook_agent_gets_an_out_file_and_sent_marks_it_delivered() {
        with_app("hook", "codex", AgentState::Working, |app, home, _| {
            install_hook(home, ".codex", "codex");
            let id = queue(app);
            assert!(app.tick_deliver());
            let line = format!("{}\n", json!({ "decision": "block", "reason": crate::mail::framed(&rec(app, &id)) }));
            assert_eq!(std::fs::read_to_string(crate::mail::mailbox(2).join(format!("{id}.out"))).unwrap(), line);
            assert_eq!(crate::hook::run_for(2, "mail", "codex", b"{}").unwrap(), line, "the script hands it over as its answer");
            assert!(app.tick_deliver());
            let m = rec(app, &id);
            assert_eq!((m.state.clone(), m.via, m.events.last().unwrap().note.as_deref()), (State::Delivered, Some(Via::Hook), Some("claimed by the hook")));
        });
    }

    #[test]
    fn no_sent_within_3_s_of_idle_takes_it_back_and_types() {
        with_app("takeback", "codex", AgentState::Idle, |app, home, typed| {
            install_hook(home, ".codex", "codex");
            let id = queue(app);
            app.tick_deliver();
            let out = crate::mail::mailbox(2).join(format!("{id}.out"));
            assert!(out.exists());
            app.tick_deliver();
            std::thread::sleep(TAKE_BACK + Duration::from_millis(100));
            assert!(app.tick_deliver());
            assert!(!out.exists(), "taken back");
            assert_eq!(rec(app, &id).events.last().unwrap().note.as_deref(), Some("the hook did not take it"));
            app.tick_deliver();
            assert!(read_until(typed, |s| s.contains(&id)).contains(&id), "and typed");
            std::thread::sleep(CR_AFTER);
            app.tick_deliver();
            assert_eq!(rec(app, &id).via, Some(Via::Typed));
        });
    }

    #[test]
    fn a_mailbox_cleared_under_a_hand_over_queues_it_again() {
        with_app("cleared", "codex", AgentState::Working, |app, home, _| {
            install_hook(home, ".codex", "codex");
            let id = queue(app);
            assert!(app.tick_deliver());
            crate::hook::clear_state(2);
            assert!(app.tick_deliver());
            assert_eq!((rec(app, &id).state, rec(app, &id).events.last().unwrap().note.as_deref()), (State::Queued, Some("the hook's mailbox was cleared")));
            assert!(app.mail.delivery.hands.is_empty(), "the pane takes mail again");
        });
    }

    /// A listener under /tmp (a socket path over 104 bytes does not bind), held by this test process.
    fn listener(tag: &str) -> (std::os::unix::net::UnixListener, String) {
        let dir = format!("/tmp/nt-s3a-{}-{tag}", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = format!("{dir}/i.sock");
        (std::os::unix::net::UnixListener::bind(&path).unwrap(), path)
    }

    #[test]
    fn an_inbox_bound_outside_the_pane_is_not_used() {
        with_app("inbox-other", "claude", AgentState::Working, |app, home, _| {
            install_hook(home, ".claude", "claude");
            let (l, path) = listener("other");
            l.set_nonblocking(true).unwrap();
            std::fs::create_dir_all(crate::hook::state_dir()).unwrap();
            std::fs::write(crate::hook::state_dir().join("2.inbox"), format!("{path}\n")).unwrap();
            let mut other = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
            app.pane_mut(2).unwrap().pty.child_pid = Some(other.id());
            let id = queue(app);
            app.tick_deliver();
            assert!(crate::mail::mailbox(2).join(format!("{id}.out")).exists(), "delivered by hook");
            assert!(log().contains("pane 2: its recorded inbox is not its own; delivering by hook"));
            assert!(l.accept().is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock), "nothing sent to the socket");
            let _ = other.kill();
            let _ = other.wait();
            let _ = std::fs::remove_dir_all(Path::new(&path).parent().unwrap());
        });
    }

    #[test]
    fn an_inbox_bound_by_the_pane_gets_the_user_line() {
        with_app("inbox-own", "claude", AgentState::Working, |app, _, _| {
            let (l, path) = listener("own");
            std::fs::create_dir_all(crate::hook::state_dir()).unwrap();
            std::fs::write(crate::hook::state_dir().join("2.inbox"), format!("{path}\n")).unwrap();
            app.pane_mut(2).unwrap().pty.child_pid = Some(std::process::id());
            let got = std::thread::spawn(move || {
                use std::io::Read;
                let mut s = String::new();
                l.accept().unwrap().0.read_to_string(&mut s).unwrap();
                s
            });
            let id = queue(app);
            assert!(app.tick_deliver());
            let line = format!("{}\n", json!({ "type": "user", "message": { "role": "user", "content": crate::mail::framed(&rec(app, &id)) } }));
            assert_eq!(got.join().unwrap(), line);
            for _ in 0..200 {
                app.tick_deliver();
                if rec(app, &id).via.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!((rec(app, &id).state, rec(app, &id).via), (State::Delivered, Some(Via::Inbox)));
            let _ = std::fs::remove_dir_all(Path::new(&path).parent().unwrap());
        });
    }

    #[test]
    fn an_inbox_swapped_after_the_check_is_not_written() {
        let dir = format!("/tmp/nt-s3a-{}-swap", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = format!("{dir}/i.sock");
        // the pane: a listener of its own at the recorded path
        let mut pane = std::process::Command::new("/usr/bin/nc").args(["-lU", &path]).stdout(std::process::Stdio::null()).spawn().unwrap();
        for _ in 0..200 {
            if Path::new(&path).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = pane.id() as i32;
        assert!(inbox_of(&path, root), "the pane's own listener passes the check");
        let (other, other_path) = listener("swap-target");
        std::fs::rename(&path, format!("{dir}/moved.sock")).unwrap();
        std::os::unix::fs::symlink(&other_path, &path).unwrap();
        let e = send_inbox(&path, b"x\n", root).unwrap_err();
        assert!(e.to_string().contains("not this pane's"), "{e}");
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut other.accept().unwrap().0, &mut got).unwrap();
        assert!(got.is_empty(), "nothing written to the socket it was swapped for");
        let _ = pane.kill();
        let _ = pane.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(Path::new(&other_path).parent().unwrap());
    }

    #[test]
    fn an_isolated_pane_never_gets_the_inbox() {
        with_app("inbox-isolated", "claude", AgentState::Working, |app, home, _| {
            install_hook(home, ".claude", "claude");
            let (l, path) = listener("isolated");
            l.set_nonblocking(true).unwrap();
            std::fs::create_dir_all(crate::hook::state_dir()).unwrap();
            std::fs::write(crate::hook::state_dir().join("2.inbox"), format!("{path}\n")).unwrap();
            app.pane_mut(2).unwrap().isolation.network = policy::Network::None;
            let id = queue(app);
            app.tick_deliver();
            assert!(crate::mail::mailbox(2).join(format!("{id}.out")).exists(), "delivered by hook");
            assert!(l.accept().is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock), "nothing sent to the socket");
            let _ = std::fs::remove_dir_all(Path::new(&path).parent().unwrap());
        });
    }

    #[test]
    fn a_strict_switch_holds_what_waited() {
        with_app("strict", "codex", AgentState::Idle, |app, _, typed| {
            let id = queue(app);
            policy::replace(policy::Policy { mode: Mode::Strict, ..Default::default() });
            assert!(app.tick_deliver());
            assert_eq!(rec(app, &id).state, State::Held(Hold::Mode));
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(read(typed), "");
        });
    }

    #[test]
    fn a_restart_requeues_an_unfinished_hand_over() {
        with_app("restart", "codex", AgentState::Blocked, |app, _, _| {
            let id = queue(app);
            app.mail.set(&id, State::Delivered, None, Some("hook hand-over".into()));
            crate::mail::mailbox_put(2, &id, "out", b"x\n").unwrap();
            crate::mail::mailbox_put(2, "01OTHER", "claim", b"x\n").unwrap();
            assert!(app.tick_deliver());
            assert_eq!(notes(&rec(app, &id)).last().unwrap(), &(State::Queued, Some("after a restart".into())));
            assert!(!crate::mail::mailbox(2).exists(), "nothing left from before");
        });
    }
}
