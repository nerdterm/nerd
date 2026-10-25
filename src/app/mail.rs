//! Messages between agents in the app: the live records, and every owner action of the feature.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::App;
use crate::layout::PaneId;
use crate::mail::{Hold, Message, Party, State, Step, Store, Via};
use crate::msock::Request;
use crate::policy::{self, Account, Action, Files, Isolation, Mode, Policy, Recipient, Route, Scope, Sender, ThreadFacts, Verdict};

pub const INSIDE_REFUSED: &str = "Not from a window inside a pane: open nerd from your terminal to change this.";
const UNNAMED: &str = "this pane has no name: the owner names it with Rename pane…";
/// Live `Wait`s are at most the socket's connections; past this the oldest is a dead one.
const WAITERS: usize = 64;
/// How often finished records are trimmed (`mail::trim`), and the file compacted when that changed them.
const TRIM_EVERY: Duration = Duration::from_secs(600);

pub(crate) struct SendReq {
    pub to: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub auto: bool,
    pub ask_route: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) enum SendReply {
    Sent { id: String },
    Held { id: String, why: String },
    Refused(String),
    /// Refused because no pane has the name or a route forbids it, not for a limit.
    Unreachable(String),
}

/// Whom a pane may write to: named panes and you, each "write" or "ask"; one left out may not be written to.
pub(crate) struct Reach {
    panes: Vec<(NamedPane, &'static str)>,
    you: Option<&'static str>,
}

impl Reach {
    fn may(&self, name: &str) -> Option<&'static str> {
        if name == "you" { self.you } else { self.panes.iter().find(|(n, _)| n.name == name).map(|(_, m)| *m) }
    }

    /// "planner, reader (ask), you"
    fn listed(&self) -> String {
        let one = |name: &str, may: &str| if may == "ask" { format!("{name} (ask)") } else { name.to_string() };
        let all: Vec<String> = self.panes.iter().map(|(n, m)| one(&n.name, m)).chain(self.you.map(|m| one("you", m))).collect();
        if all.is_empty() { "nobody".into() } else { all.join(", ") }
    }

    /// Names, places and "write"/"ask"; agent states are left out, they change all the time.
    fn digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (n, may) in &self.panes {
            (&n.name, &n.space, &n.tab, may).hash(&mut h);
        }
        self.you.hash(&mut h);
        h.finish()
    }
}

/// A pane's `Wait`: its lines go out on `tx`, and dropping it ends the request.
struct Waiter {
    pane: PaneId,
    id: String,
    /// Every new message of this thread to the pane counts, not only replies to `id`.
    thread: Option<String>,
    left: usize,
    got: usize,
    until: Instant,
    tx: std::sync::mpsc::Sender<String>,
}

impl Waiter {
    fn matches(&self, m: &Message) -> bool {
        m.to.pane() == Some(self.pane)
            && matches!(m.state, State::Queued | State::Delivered)
            && (m.reply_to.as_deref() == Some(self.id.as_str()) || self.thread.as_deref() == Some(m.thread.as_str()))
    }
}

fn line(v: Value) -> String {
    format!("{v}\n")
}

#[derive(Default)]
pub struct MailState {
    /// Live records, oldest id first.
    pub msgs: Vec<Message>,
    /// The last key or paste the owner sent, and to which pane.
    pub owner_typed: Option<(PaneId, Instant)>,
    pub(super) delivery: super::deliver::Delivery,
    store: Option<Store>,
    /// When each record entered its state; TTLs run on this clock, not the wall's.
    since: HashMap<String, Instant>,
    expired_at: Option<Instant>,
    append_failed_at: Option<Instant>,
    waiters: Vec<Waiter>,
    trimmed_at: Option<Instant>,
    /// Per sender name, when its `reply_to` named no message it may answer (wall secs).
    bad_replies: HashMap<String, Vec<u64>>,
    /// Per pane, a digest of whom it may write to as it last saw it.
    seen_reachable: HashMap<PaneId, u64>,
}

impl MailState {
    /// Replays `path`; on error the records live in memory only.
    pub fn open(path: &Path) -> MailState {
        let wall = crate::hook::now();
        match Store::open(path, wall) {
            Ok((store, msgs)) => {
                let now = Instant::now();
                let since = msgs.iter().map(|m| (m.id.clone(), now.checked_sub(Duration::from_secs(wall.saturating_sub(m.since()))).unwrap_or(now))).collect();
                MailState { msgs, store: Some(store), since, ..Default::default() }
            }
            Err(e) => {
                crate::log::error(&format!("messages.jsonl: {e}; messages are kept in memory only"));
                MailState::default()
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Message> {
        self.msgs.iter().find(|m| m.id == id)
    }

    /// Appends the record and logs its line.
    pub fn add(&mut self, m: Message) {
        let at = self.msgs.partition_point(|x| x.id < m.id);
        self.since.insert(m.id.clone(), Instant::now());
        self.msgs.insert(at, m);
        self.saved(at);
    }

    /// Pushes a step and sets the state (`via` only when Some); false: no such id.
    pub fn set(&mut self, id: &str, state: State, via: Option<Via>, note: Option<String>) -> bool {
        self.update(id, |m| {
            m.events.push(Step { state: state.clone(), at: crate::hook::now(), note });
            m.state = state;
            if via.is_some() {
                m.via = via;
            }
        })
    }

    /// Any other change to a record, appended.
    pub fn update(&mut self, id: &str, f: impl FnOnce(&mut Message)) -> bool {
        let Some(at) = self.msgs.iter().position(|m| m.id == id) else { return false };
        let before = self.msgs[at].state.clone();
        f(&mut self.msgs[at]);
        if self.msgs[at].state != before {
            self.since.insert(id.to_string(), Instant::now());
        }
        self.saved(at);
        true
    }

    /// The record at `at` changed: to the file, and to the log.
    fn saved(&mut self, at: usize) {
        let m = &self.msgs[at];
        if let Some(store) = self.store.as_mut() {
            let done = store.append(m).and_then(|_| if store.due() { store.compact(&self.msgs) } else { Ok(()) });
            if let Err(e) = done
                && self.append_failed_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
            {
                self.append_failed_at = Some(Instant::now());
                crate::log::error(&format!("messages.jsonl not written: {e}; the message goes on in memory"));
            }
        }
        let line = crate::mail::log_line(m);
        match m.state {
            State::Held(_) => crate::log::warn_limited("message held", &line),
            State::Refused(_) => crate::log::warn_limited("message refused", &line),
            _ => crate::log::info(&line),
        }
    }

    /// Queued and held records past their time become Expired; finished ones older than `HELD_TTL_SECS` leave memory.
    pub fn expire(&mut self, wall: u64) -> usize {
        let now = Instant::now();
        let ttl = |s: &State| match s {
            State::Queued => Some((crate::mail::QUEUED_TTL_SECS, "after 24 h")),
            State::Held(_) => Some((crate::mail::HELD_TTL_SECS, "after 7 days")),
            _ => None,
        };
        let due: Vec<(String, &'static str)> = self
            .msgs
            .iter()
            .filter_map(|m| {
                let (secs, note) = ttl(&m.state)?;
                let since = *self.since.get(&m.id).unwrap_or(&now);
                (now.duration_since(since) > Duration::from_secs(secs)).then(|| (m.id.clone(), note))
            })
            .collect();
        for (id, note) in &due {
            self.set(id, State::Expired, None, Some(note.to_string()));
        }
        let before = self.msgs.len();
        let since = &mut self.since;
        self.msgs.retain(|m| {
            let keep = !(m.state.finished() && wall.saturating_sub(m.since()) > crate::mail::HELD_TTL_SECS);
            if !keep {
                since.remove(&m.id);
            }
            keep
        });
        if self.trimmed_at.is_none_or(|t| t.elapsed() >= TRIM_EVERY) {
            self.trimmed_at = Some(now);
            if crate::mail::trim(&mut self.msgs, wall) {
                let live: std::collections::HashSet<&str> = self.msgs.iter().map(|m| m.id.as_str()).collect();
                self.since.retain(|id, _| live.contains(id.as_str()));
                if let Some(Err(e)) = self.store.as_mut().map(|s| s.compact(&self.msgs)) {
                    crate::log::error(&format!("messages.jsonl not compacted: {e}"));
                }
            }
        }
        due.len() + before - self.msgs.len()
    }

    /// Every Held, and messages to you still Delivered; oldest first.
    pub fn waiting(&self) -> impl Iterator<Item = &Message> {
        self.msgs.iter().filter(|m| matches!(m.state, State::Held(_)) || (m.to == Party::You && m.state == State::Delivered))
    }

    /// Addressed to that pane and not yet in its agent's hands: a hand-over sets `via`.
    pub fn unread(&self, pane: PaneId) -> usize {
        self.msgs.iter().filter(|m| m.to.pane() == Some(pane) && (m.state == State::Queued || (m.state == State::Delivered && m.via.is_none()))).count()
    }

    /// Ends the `Wait`s whose message expired undelivered or whose time is up.
    fn end_waits(&mut self) {
        let now = Instant::now();
        let msgs = &self.msgs;
        self.waiters.retain(|w| {
            let end = if msgs.iter().any(|m| m.id == w.id && m.state == State::Expired) {
                json!({ "expired": { "id": w.id } })
            } else if now >= w.until {
                json!({ "timeout": {} })
            } else {
                return true;
            };
            let _ = w.tx.send(line(end));
            false
        });
    }
}

pub struct NamedPane {
    pub name: String,
    pub pane: PaneId,
    pub space: String,
    pub tab: String,
    pub agent: Option<String>,
    pub uid: u32,
    pub isolation: Isolation,
}

pub enum RenameCheck {
    Address,
    Label,
    Taken { space: String, tab: String },
    History { routes: Vec<String>, waiting: usize },
}

/// Tab(space index, tab index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsolationTarget {
    Pane(PaneId),
    Tab(usize, usize),
    Space(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum MailAction {
    /// None removes; replaces a route with the same ends.
    SetRoute { from: Scope, to: Scope, action: Option<Action>, until: Option<u64> },
    /// allow → ask → no → removed.
    CycleRoute(usize),
    SetMode(Mode),
    SetOn(bool),
    SetAllowFromPanes(bool),
    /// A macOS account name; its uid from the user database.
    AdmitAccount(String),
    RemoveAccount(String),
    /// Each replaces a route with the same ends, else is appended.
    Topology(Vec<Route>),
    /// keep: the name's routes and waiting mail from before.
    Rename { pane: PaneId, name: Option<String>, keep: bool },
    /// None clears a tab or space default.
    SetIsolation { target: IsolationTarget, isolation: Option<Isolation>, restart: bool },
    Deliver(String),
    /// Deliver, and an allow route from→to until now + 3600.
    DeliverAllow(String),
    Drop(String),
    Reply { id: String, text: String },
    /// to: a name or "all"; the sender is you.
    Send { to: String, text: String },
    AnswerRequest { id: String, action: Action },
}

pub struct MailView {
    pub on: bool,
    pub mode: Mode,
    pub broken: bool,
    /// Spaces in sidebar order, then the last column.
    pub columns: Vec<MailColumn>,
    pub routes: Vec<RouteView>,
    pub traffic: Vec<Traffic>,
    /// As `MailState::waiting`.
    pub waiting: Vec<Message>,
    /// Created in the last 24 hours, newest first.
    pub today: Vec<Message>,
}

/// space None: the last column, `you` first, then names with routes or waiting mail and no pane.
pub struct MailColumn {
    pub space: Option<String>,
    pub cards: Vec<Card>,
}

pub struct Card {
    pub name: String,
    pub pane: Option<PaneId>,
    pub tab: String,
    pub agent: Option<String>,
    pub uid: u32,
    pub isolation: Isolation,
}

/// risk: an Allow between two pane cards where one has network and the other none; left: secs until `until`.
pub struct RouteView {
    pub index: usize,
    pub route: Route,
    pub risk: bool,
    pub left: Option<u64>,
}

pub struct Traffic {
    pub from: String,
    pub to: String,
    pub held: usize,
    pub today: usize,
}

const DAY: u64 = 86400;

impl App {
    /// Ok(toast, may be empty) or Err(notice); a window inside a pane changes nothing.
    pub fn mail_act(&mut self, action: MailAction, inside: bool) -> Result<String, String> {
        if inside {
            return Err(INSIDE_REFUSED.into());
        }
        match action {
            MailAction::SetRoute { from, to, action, until } => self.edit_policy(|p| {
                p.routes.retain(|r| r.from != from || r.to != to);
                if let Some(action) = action {
                    p.routes.push(Route { from, to, action, until });
                }
            })?,
            MailAction::CycleRoute(i) => {
                if i >= policy::current().routes.len() {
                    return Err("that route is gone".into());
                }
                self.edit_policy(|p| match p.routes[i].action {
                    Action::Allow => p.routes[i].action = Action::Ask,
                    Action::Ask => p.routes[i].action = Action::No,
                    Action::No => {
                        p.routes.remove(i);
                    }
                })?
            }
            MailAction::SetMode(m) => self.edit_policy(|p| p.mode = m)?,
            MailAction::SetOn(on) => self.edit_policy(|p| p.on = on)?,
            MailAction::SetAllowFromPanes(on) => self.edit_policy(|p| p.allow_from_panes = on)?,
            MailAction::AdmitAccount(name) => {
                let shaped = (1..=32).contains(&name.len())
                    && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'));
                if !shaped {
                    return Err("an account name is lowercase letters, digits, '_', '.' and '-'".into());
                }
                let uid = account_uid(&name).ok_or_else(|| format!("no macOS account is called {name}"))?;
                if uid == policy::owner_uid() {
                    return Err("that is your own account".into());
                }
                // 4294967294 and up: `nobody` and the unknown uid.
                if !(501..4_294_967_294).contains(&uid) {
                    return Err(format!("{name} is a system account"));
                }
                if policy::current().accounts.iter().any(|a| a.name == name) {
                    return Ok(String::new());
                }
                self.edit_policy(|p| p.accounts.push(Account { name, uid }))?;
                if let Err(e) = crate::isolation::ensure_shared() {
                    crate::log::warn(&format!("the admitted account's panes cannot reach messages: {e}"));
                }
            }
            MailAction::RemoveAccount(name) => self.edit_policy(|p| p.accounts.retain(|a| a.name != name))?,
            MailAction::Topology(routes) => self.edit_policy(|p| {
                for r in routes {
                    match p.routes.iter_mut().find(|x| x.from == r.from && x.to == r.to) {
                        Some(x) => *x = r,
                        None => p.routes.push(r),
                    }
                }
            })?,
            MailAction::Rename { pane, name, keep } => self.rename(pane, name, keep)?,
            MailAction::SetIsolation { target, isolation, restart } => self.set_isolation(target, isolation, restart)?,
            other => return self.mail_message_act(other),
        }
        Ok(String::new())
    }

    fn mail_message_act(&mut self, action: MailAction) -> Result<String, String> {
        let now = crate::hook::now();
        match action {
            MailAction::Send { to, text } => {
                let text = checked_text(&text)?;
                let targets: Vec<PaneId> = match to.as_str() {
                    "all" => self.named_panes().iter().map(|n| n.pane).collect(),
                    name => vec![self.pane_by_name(name).filter(|_| policy::valid_address(name)).ok_or_else(|| format!("no pane is called {name}"))?],
                };
                if targets.is_empty() {
                    return Err("no pane has a name".into());
                }
                let mut thread = None;
                for p in targets {
                    let to = self.party(p);
                    let id = crate::mail::new_id();
                    let thread = thread.get_or_insert_with(|| id.clone()).clone();
                    let hops = vec!["you".to_string(), to.name().to_string()];
                    self.owner_sends(Message { thread, hops, ..message(id, Party::You, to, text.clone(), now) });
                }
            }
            MailAction::Reply { id, text } => {
                let text = checked_text(&text)?;
                let r = self.mail.get(&id).filter(|m| m.from != Party::You).cloned().ok_or_else(|| format!("no message {id} from a pane"))?;
                let to = self.live(r.from.clone());
                let mut hops = r.hops.clone();
                hops.push(to.name().to_string());
                let mut m = Message { thread: r.thread.clone(), reply_to: Some(id.clone()), hops, ..message(crate::mail::new_id(), Party::You, to, text, now) };
                if m.to.pane().is_none() {
                    stamp(&mut m, State::Held(Hold::NoAgent), now);
                }
                self.owner_sends(m);
                self.mark_answered(&id);
            }
            MailAction::Deliver(id) => self.deliver(&id)?,
            MailAction::DeliverAllow(id) => {
                self.deliver(&id)?;
                let m = self.mail.get(&id).cloned().ok_or_else(|| format!("message {id} is gone"))?;
                let (from, to) = (scope_of(&m.from), scope_of(&m.to));
                self.edit_policy(|p| {
                    p.routes.retain(|r| r.from != from || r.to != to);
                    p.routes.push(Route { from, to, action: Action::Allow, until: Some(now + 3600) });
                })?;
            }
            MailAction::Drop(id) => match self.mail.get(&id).map(|m| (&m.state, &m.to)) {
                Some((State::Held(_), _)) => {
                    self.mail.set(&id, State::Dropped, None, None);
                }
                Some((State::Delivered, Party::You)) => {
                    self.mail.set(&id, State::Read, None, None);
                }
                _ => return Err(format!("message {id} is not waiting")),
            },
            MailAction::AnswerRequest { id, action } => {
                let m = self.mail.get(&id).filter(|m| m.state == State::Held(Hold::RouteRequest)).cloned().ok_or_else(|| format!("no route request {id}"))?;
                let (from, to) = (scope_of(&m.from), scope_of(&m.to));
                self.edit_policy(|p| {
                    p.routes.retain(|r| r.from != from || r.to != to);
                    p.routes.push(Route { from, to, action, until: None });
                })?;
                self.mail.set(&id, State::Answered, None, None);
            }
            other => return Err(format!("{other:?} is not a message action")),
        }
        Ok(String::new())
    }

    /// A held message goes on its way, not held again at delivery.
    fn deliver(&mut self, id: &str) -> Result<(), String> {
        let Some(m) = self.mail.get(id).filter(|m| matches!(&m.state, State::Held(h) if *h != Hold::RouteRequest)) else {
            return Err(format!("message {id} is not waiting"));
        };
        let state = if m.to == Party::You { State::Delivered } else { State::Queued };
        // One record, not two: a separate `passed` write would log the message as held once more.
        self.mail.update(id, |m| {
            m.passed = true;
            m.events.push(Step { state: state.clone(), at: crate::hook::now(), note: Some("let through by you".into()) });
            m.state = state;
        });
        self.wake(id);
        Ok(())
    }

    fn owner_sends(&mut self, mut m: Message) {
        m.passed = true;
        let id = m.id.clone();
        self.mail.add(m);
        self.wake(&id);
    }

    /// Only a message that reached its pane is answered: a held or refused one stays as it is.
    fn mark_answered(&mut self, id: &str) {
        if self.mail.get(id).is_some_and(|m| matches!(m.state, State::Queued | State::Delivered | State::Read)) {
            self.mail.set(id, State::Answered, None, None);
        }
    }

    /// A pane as a message's party, named as it is now.
    fn party(&self, p: PaneId) -> Party {
        let pane = self.pane(p);
        Party::Pane { pane: Some(p), name: pane.and_then(|x| x.label.clone()).unwrap_or_default(), agent: pane.and_then(|x| x.agent.clone()), uid: self.pane_uid(p) }
    }

    /// A party from an older record: its pane only while that pane still holds the name.
    fn live(&self, mut party: Party) -> Party {
        if let Party::Pane { pane, name, .. } = &mut party
            && pane.is_some_and(|p| self.pane(p).and_then(|x| x.label.as_deref()) != Some(name.as_str()))
        {
            *pane = None;
        }
        party
    }

    /// The message socket's `Hello`: the pane a peer runs in, and its name when it has one.
    pub(crate) fn mail_hello(&self, pid: i32, started: Option<u64>, accepted: u64) -> Result<(PaneId, Option<String>), String> {
        let all = || self.spaces.iter().flat_map(|s| &s.tabs).flat_map(|t| t.panes.values());
        let panes: Vec<(PaneId, i32)> = all().filter_map(|p| Some((p.id, p.pty.child_pid? as i32))).collect();
        let broker = all().any(|p| p.pty.starting()).then(crate::broker::pid).filter(|_| !crate::plugins::within_a_plugin(pid));
        let pane = crate::msock::identify(pid, started, accepted, &panes, broker, crate::transport::parent_pid)?;
        Ok((pane, self.pane(pane).and_then(|p| p.label.clone()).filter(|l| policy::valid_address(l))))
    }

    /// A message socket request of a verified pane; true: redraw.
    pub(crate) fn mail_request(&mut self, pane: PaneId, uid: u32, req: Request, reply: std::sync::mpsc::Sender<String>) -> bool {
        let now = crate::hook::now();
        let v = if self.pane(pane).is_none() {
            json!({ "error": "this pane is gone" })
        } else {
            match req {
                Request::Send { to, text, reply_to, auto, ask_route } => match self.mail_send(pane, uid, SendReq { to, text, reply_to, auto, ask_route }, now) {
                    SendReply::Sent { id } => {
                        let state = self.mail.get(&id).and_then(|m| serde_json::to_value(&m.state).ok()).unwrap_or_else(|| json!("queued"));
                        self.noted(pane, uid, now, json!({ "sent": { "id": id, "state": state } }))
                    }
                    SendReply::Held { id, why } => self.noted(pane, uid, now, json!({ "held": { "id": id, "why": why } })),
                    SendReply::Refused(why) => self.noted(pane, uid, now, json!({ "refused": why })),
                    // Say who it may write to, so the agent's list is current again.
                    SendReply::Unreachable(why) => match self.reachable(pane, uid, now) {
                        Some(r) => {
                            self.mail.seen_reachable.insert(pane, r.digest());
                            json!({ "refused": format!("{why}; may write to: {}", r.listed()) })
                        }
                        None => json!({ "refused": why }),
                    },
                },
                Request::Agents {} => {
                    let r = self.reachable(pane, uid, now);
                    if let Some(r) = &r {
                        self.mail.seen_reachable.insert(pane, r.digest());
                    }
                    self.agents_for(pane, uid, r)
                }
                Request::Inbox { all } => {
                    let v = self.inbox(pane, all);
                    self.noted(pane, uid, now, v)
                }
                Request::Show { id } => {
                    let v = self.show(pane, &id);
                    self.noted(pane, uid, now, v)
                }
                Request::Wait { id, thread, count, timeout } => {
                    if self.reachable_changed(pane, uid, now) {
                        let _ = reply.send(line(json!({ "reachable_changed": true })));
                    }
                    self.wait(pane, id, thread, count, timeout, reply);
                    return true;
                }
                _ => json!({ "error": "not a request for the server" }),
            }
        };
        let _ = reply.send(line(v));
        true
    }

    /// The whole send path of a pane, in the order the policy, the limits and the records need.
    pub(crate) fn mail_send(&mut self, from: PaneId, uid: u32, req: SendReq, now: u64) -> SendReply {
        if !policy::current().on {
            return SendReply::Refused("messaging is off".into());
        }
        let Some(sender) = self.sender_of(from, uid) else { return SendReply::Refused(UNNAMED.into()) };
        let text = crate::mail::clean(&req.text);
        if text.len() > crate::mail::MESSAGE_BYTES {
            return SendReply::Refused(format!("text over {} bytes", crate::mail::MESSAGE_BYTES));
        }
        if text.is_empty() && !req.ask_route {
            return SendReply::Refused("an empty message".into());
        }
        let me = Party::Pane { pane: Some(from), name: sender.name.clone(), agent: sender.agent.clone(), uid };
        let mut m = Message { auto: req.auto, ..message(crate::mail::new_id(), me, Party::You, text, now) };
        if let Some(rid) = &req.reply_to {
            let bad = self.mail.bad_replies.entry(sender.name.clone()).or_default();
            bad.retain(|t| now.saturating_sub(*t) < 60);
            if bad.len() >= crate::mail::PER_PAIR_PER_MINUTE {
                crate::log::warn_limited("message reply guesses", &format!("pane {from}: {} replies to messages it may not answer within 60 s", bad.len()));
                return SendReply::Refused("too many replies to messages this pane did not get; try later".into());
            }
            let Some(r) = self.mail.get(rid).filter(|r| r.to.pane() == Some(from)) else {
                self.mail.bad_replies.entry(sender.name.clone()).or_default().push(now);
                return SendReply::Refused(format!("no message {rid} to this pane"));
            };
            // Only a message that passed the policy to this pane gives a reply; one that did not is no guess, so not counted, and one neutral text hides the owner's decision.
            if !matches!(r.state, State::Queued | State::Delivered | State::Read | State::Answered) {
                return SendReply::Refused(format!("message {rid} is not one this pane can answer"));
            }
            m.to = self.live(r.from.clone());
            m.thread = r.thread.clone();
            m.hops = r.hops.clone();
            m.hops.push(m.to.name().to_string());
            m.reply_to = Some(rid.clone());
        } else {
            m.to = match req.to.as_str() {
                "you" => Party::You,
                "all" if req.ask_route => return SendReply::Refused("a route is asked for one name".into()),
                "all" => return self.broadcast(m, from, &sender, now),
                name => match self.pane_by_name(name).filter(|_| policy::valid_address(name)) {
                    None => return SendReply::Unreachable(format!("no pane is called {name} (the owner names one with Rename pane…)")),
                    Some(p) if p == from => return SendReply::Refused("a pane cannot write to itself".into()),
                    Some(p) => self.party(p),
                },
            };
            m.hops = vec![m.from.name().to_string(), m.to.name().to_string()];
        }
        let asked = |r: &&Message| r.state == State::Held(Hold::RouteRequest) && r.from.name() == m.from.name() && r.to.name() == m.to.name();
        if req.ask_route
            && let Some(r) = self.mail.msgs.iter().find(asked)
        {
            return SendReply::Held { id: r.id.clone(), why: format!("asked the owner for a route to {}", m.to.name()) };
        }
        let (state, out) = self.judge(&m, &sender, req.ask_route, now);
        // A limit refuses only a name the pane may write to; a name it may not was refused by its route.
        let unreachable = out.is_err() && req.reply_to.is_none() && !req.ask_route && self.reachable(from, uid, now).is_some_and(|r| r.may(m.to.name()).is_none());
        stamp(&mut m, state, now);
        match self.record(m, out, now) {
            SendReply::Refused(why) if unreachable => SendReply::Unreachable(why),
            r => r,
        }
    }

    /// The state a pane's new message gets, and its answer: Ok(None) on its way, Ok(Some(why)) held, Err(why) refused.
    fn judge(&self, m: &Message, sender: &Sender, ask_route: bool, now: u64) -> (State, Result<Option<String>, String>) {
        let tripped = crate::mail::tripped(&self.mail.msgs, m, now);
        let refusal = match tripped {
            Some("held_total") => Some(format!("the owner already has {} held messages; try later", crate::mail::HELD_TOTAL)),
            Some("duplicate_window") => Some(format!("the same text to {} within {} s", m.to.name(), crate::mail::DUPLICATE_WINDOW_SECS)),
            _ => None,
        };
        let refused = |why: String| (State::Refused(why.clone()), Err(why));
        if ask_route {
            // Not the duplicate rule: a refused record keeps no text, so it would match every request's empty one.
            return match tripped {
                Some("held_total") => refused(refusal.unwrap_or_default()),
                _ => (State::Held(Hold::RouteRequest), Ok(Some(format!("asked the owner for a route to {}", m.to.name())))),
            };
        }
        let Some(recipient) = self.recipient_of(&m.to) else { return (State::Held(Hold::NoAgent), Ok(Some(Hold::NoAgent.words()))) };
        let v = policy::decide(&policy::current(), sender, &recipient, self.reply_facts(m).as_ref(), now);
        match (v.action, tripped, refusal) {
            (Action::No, ..) => refused(v.why),
            (Action::Ask, ..) => (State::Held(if v.route.is_some() { Hold::Route } else { Hold::CrossUid }), Ok(Some(v.why))),
            (Action::Allow, _, Some(why)) => refused(why),
            (Action::Allow, Some(name), None) => (State::Held(Hold::limit(name)), Ok(Some(format!("limit {name}")))),
            (Action::Allow, None, None) if m.to == Party::You => (State::Delivered, Ok(None)),
            (Action::Allow, None, None) => (State::Queued, Ok(None)),
        }
    }

    /// Writes a pane's new message and answers for it.
    fn record(&mut self, mut m: Message, mut out: Result<Option<String>, String>, now: u64) -> SendReply {
        let id = m.id.clone();
        let held = self.mail.msgs.iter().filter(|r| matches!(r.state, State::Held(_)) && r.from.name() == m.from.name()).count();
        if out.as_ref().is_ok_and(Option::is_some) && held >= crate::mail::HELD_PER_SENDER {
            let why = format!("this pane already has {} held messages; wait for the owner", crate::mail::HELD_PER_SENDER);
            stamp(&mut m, State::Refused(why.clone()), now);
            out = Err(why);
        }
        let pair = self.mail.msgs.iter().filter(|r| r.from.name() == m.from.name() && r.to.name() == m.to.name() && now.saturating_sub(r.created) < 60).count();
        // A refusal past the pair limit is only logged: a sender in a loop would fill memory and the file.
        if out.is_err() && pair >= crate::mail::PER_PAIR_PER_MINUTE {
            crate::log::warn_limited("message refused", &crate::mail::log_line(&m));
        } else {
            let replied = m.reply_to.clone().filter(|_| out.is_ok());
            self.mail.add(m);
            if let Some(r) = replied {
                self.mark_answered(&r);
            }
            self.wake(&id);
        }
        match out {
            Ok(None) => SendReply::Sent { id },
            Ok(Some(why)) => SendReply::Held { id, why },
            Err(why) => SendReply::Refused(why),
        }
    }

    /// `all`: one message per pane the sender may write to, in one thread; past `BROADCAST` the rest are held.
    fn broadcast(&mut self, first: Message, from: PaneId, sender: &Sender, now: u64) -> SendReply {
        let p = policy::current();
        let targets: Vec<NamedPane> = self
            .named_panes()
            .into_iter()
            .filter(|n| n.pane != from && policy::decide(&p, sender, &recipient(n), None, now).action != Action::No)
            .collect();
        let thread = first.id.clone();
        let (mut sent, mut held, mut whys) = (0, 0, Vec::<String>::new());
        for (i, n) in targets.iter().enumerate() {
            let mut m = Message { thread: thread.clone(), to: self.party(n.pane), ..first.clone() };
            if i > 0 {
                m.id = crate::mail::new_id();
            }
            m.hops = vec![m.from.name().to_string(), n.name.clone()];
            let (state, out) = if i < crate::mail::BROADCAST {
                self.judge(&m, sender, false, now)
            } else {
                (State::Held(Hold::limit("broadcast")), Ok(Some("limit broadcast".into())))
            };
            match &out {
                Ok(None) => sent += 1,
                Ok(Some(why)) => {
                    held += 1;
                    if !whys.contains(why) {
                        whys.push(why.clone());
                    }
                }
                Err(_) => {}
            }
            stamp(&mut m, state, now);
            self.record(m, out, now);
        }
        match (sent, held) {
            (_, 1..) => SendReply::Held { id: thread, why: format!("{held} of {} held: {}", targets.len(), whys.join(", ")) },
            (1.., 0) => SendReply::Sent { id: thread },
            _ => SendReply::Refused("no pane you may write to".into()),
        }
    }

    /// Whom the caller may write to; None for an unnamed caller.
    fn reachable(&self, pane: PaneId, uid: u32, now: u64) -> Option<Reach> {
        let sender = self.sender_of(pane, uid)?;
        let p = policy::current();
        let may = |r: &Recipient| match policy::decide(&p, &sender, r, None, now).action {
            Action::Allow => Some("write"),
            Action::Ask => Some("ask"),
            Action::No => None,
        };
        let panes = self.named_panes().into_iter().filter(|n| n.pane != pane).filter_map(|n| may(&recipient(&n)).map(|m| (n, m))).collect();
        Some(Reach { panes, you: may(&Recipient::You) })
    }

    /// True once when whom the pane may write to changed since it last saw the list.
    fn reachable_changed(&mut self, pane: PaneId, uid: u32, now: u64) -> bool {
        let Some(d) = self.reachable(pane, uid, now).map(|r| r.digest()) else { return false };
        match self.mail.seen_reachable.get_mut(&pane) {
            Some(seen) if *seen != d => {
                *seen = d;
                true
            }
            _ => false,
        }
    }

    /// `v` with `reachable_changed` when the pane's list changed since it last saw it.
    fn noted(&mut self, pane: PaneId, uid: u32, now: u64, mut v: Value) -> Value {
        if self.reachable_changed(pane, uid, now) {
            v["reachable_changed"] = json!(true);
        }
        v
    }

    /// The named panes the caller may write to (or ask to), and you.
    fn agents_for(&self, pane: PaneId, uid: u32, r: Option<Reach>) -> Value {
        let (Some(sender), Some(r)) = (self.sender_of(pane, uid), r) else { return json!({ "error": UNNAMED }) };
        let mut out = Vec::new();
        for (n, may) in r.panes {
            let state = match self.pane(n.pane) {
                Some(x) if x.agent.is_some() => match x.term.state() {
                    crate::term::AgentState::Working => "working",
                    crate::term::AgentState::Idle => "idle",
                    crate::term::AgentState::Blocked => "blocked",
                    crate::term::AgentState::Done => "done",
                    crate::term::AgentState::Failed => "failed",
                    crate::term::AgentState::Unknown => "unknown",
                },
                _ => "shell",
            };
            let place = format!("{}/{}", n.space, n.tab);
            out.push(json!({ "name": n.name, "agent": n.agent, "state": state, "place": place, "unread": self.mail.unread(n.pane), "session_name": null, "pane": n.pane, "may": may }));
        }
        if let Some(may) = r.you {
            out.push(json!({ "name": "you", "agent": null, "state": "-", "place": "-", "unread": 0, "session_name": null, "pane": null, "may": may }));
        }
        json!({ "agents": out, "me": sender.name })
    }

    /// Messages to the pane, oldest first, at most 50; those not read yet become Read.
    fn inbox(&mut self, pane: PaneId, all: bool) -> Value {
        let now = crate::hook::now();
        let queued: Vec<Message> = self.mail.msgs.iter().filter(|m| m.to.pane() == Some(pane) && m.state == State::Queued).cloned().collect();
        for m in &queued {
            self.stopped(m, now);
        }
        let shown = |s: &State| matches!(s, State::Queued | State::Delivered) || (all && matches!(s, State::Read | State::Answered));
        let mine: Vec<Message> = self.mail.msgs.iter().filter(|m| m.to.pane() == Some(pane) && shown(&m.state)).cloned().collect();
        // Unread: the oldest 50, the rest on the next call; with `all`, the newest 50.
        let skip = if all { mine.len().saturating_sub(50) } else { 0 };
        let mine: Vec<Message> = mine.into_iter().skip(skip).take(50).collect();
        for m in mine.iter().filter(|m| matches!(m.state, State::Queued | State::Delivered)) {
            self.mail.set(&m.id, State::Read, Some(Via::Inbox), None);
        }
        json!({ "messages": mine })
    }

    /// The caller's own message, either way; held or refused mail to it stays unseen.
    fn show(&mut self, pane: PaneId, id: &str) -> Value {
        if let Some(m) = self.mail.get(id).filter(|m| m.to.pane() == Some(pane) && m.state == State::Queued).cloned() {
            self.stopped(&m, crate::hook::now());
        }
        let seen = |m: &Message| m.from.pane() == Some(pane) || (m.to.pane() == Some(pane) && matches!(m.state, State::Queued | State::Delivered | State::Read | State::Answered));
        let Some(m) = self.mail.get(id).filter(|m| seen(m)).cloned() else { return json!({ "error": format!("no message {id} for this pane") }) };
        if m.to.pane() == Some(pane) && matches!(m.state, State::Queued | State::Delivered) {
            self.mail.set(id, State::Read, Some(Via::Inbox), None);
        }
        json!({ "messages": [m] })
    }

    fn wait(&mut self, pane: PaneId, id: String, thread: bool, count: usize, timeout: u64, tx: std::sync::mpsc::Sender<String>) {
        let Some(m) = self.mail.get(&id).filter(|m| m.from.pane() == Some(pane) || m.to.pane() == Some(pane)) else {
            let _ = tx.send(line(json!({ "error": format!("no message {id} for this pane") })));
            return;
        };
        if m.state == State::Expired {
            let _ = tx.send(line(json!({ "expired": { "id": id } })));
            return;
        }
        let until = Instant::now() + Duration::from_secs(timeout.min(DAY));
        let mut w = Waiter { pane, thread: thread.then(|| m.thread.clone()), id, left: count.max(1), got: 0, until, tx };
        // Replies that came before the wait count too.
        let early: Vec<Message> = self.mail.msgs.iter().filter(|m| w.matches(m)).cloned().collect();
        for m in &early {
            if !self.hand(&mut w, m) {
                return;
            }
        }
        if self.mail.waiters.len() >= WAITERS {
            self.mail.waiters.remove(0);
        }
        self.mail.waiters.push(w);
    }

    /// Streams `m` to a waiter, which then has read it; false: the wait is over.
    fn hand(&mut self, w: &mut Waiter, m: &Message) -> bool {
        if w.tx.send(line(json!({ "message": m }))).is_err() {
            return false;
        }
        self.mail.set(&m.id, State::Read, Some(Via::Inbox), None);
        w.got += 1;
        w.left -= 1;
        if w.left == 0 {
            let _ = w.tx.send(line(json!({ "done": { "count": w.got } })));
            return false;
        }
        true
    }

    /// A message just went on its way: the waits it answers get it.
    fn wake(&mut self, id: &str) {
        let Some(m) = self.mail.get(id).cloned() else { return };
        let mut ws = std::mem::take(&mut self.mail.waiters);
        ws.retain_mut(|w| !w.matches(&m) || self.hand(w, &m));
        self.mail.waiters = ws;
    }

    fn rename(&mut self, pane: PaneId, name: Option<String>, keep: bool) -> Result<(), String> {
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        if self.pane(pane).is_none() {
            return Err("that pane is gone".into());
        }
        let addr = name.clone().filter(|n| policy::valid_address(n));
        if let Some(n) = &addr
            && let Some(other) = self.pane_by_name(n).filter(|o| *o != pane)
        {
            let (space, tab) = self.place(other);
            return Err(format!("A pane in space {space}, tab {tab} is already called {n}"));
        }
        let (routes, records) = addr.as_deref().map(|n| self.history(n)).unwrap_or_default();
        if !keep && !routes.is_empty() {
            let gone: Vec<Route> = routes.iter().map(|i| policy::current().routes[*i].clone()).collect();
            self.edit_policy(|p| p.routes.retain(|r| !gone.contains(r)))?;
        }
        if let Some(p) = self.pane_mut(pane) {
            p.label = name;
        }
        let own: Vec<(String, State)> = self.mail.msgs.iter().filter(|m| m.to.pane() == Some(pane)).map(|m| (m.id.clone(), m.state.clone())).collect();
        for (id, state) in own {
            match (&addr, state) {
                (Some(n), State::Queued) => {
                    self.mail.update(&id, |m| set_to_name(m, n));
                }
                (Some(n), State::Held(Hold::Unnamed)) => {
                    self.mail.update(&id, |m| set_to_name(m, n));
                    self.mail.set(&id, State::Queued, None, None);
                }
                (None, State::Queued) => {
                    self.mail.set(&id, State::Held(Hold::Unnamed), None, None);
                }
                _ => {}
            }
        }
        for id in records {
            if keep {
                self.mail.update(&id, |m| {
                    if let Party::Pane { pane: p, .. } = &mut m.to {
                        *p = Some(pane);
                    }
                });
                if matches!(self.mail.get(&id).map(|m| &m.state), Some(State::Held(Hold::NoAgent | Hold::Unnamed))) {
                    self.mail.set(&id, State::Queued, None, None);
                }
            } else {
                self.mail.set(&id, State::Dropped, None, Some("renamed without its history".into()));
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// A name's routes (indexes) and the Queued or Held records to it that no live pane holds.
    fn history(&self, name: &str) -> (Vec<usize>, Vec<String>) {
        let p = policy::current();
        let ours = Scope::Pane(name.to_string());
        let routes = p.routes.iter().enumerate().filter(|(_, r)| r.from == ours || r.to == ours).map(|(i, _)| i).collect();
        let records = self
            .mail
            .msgs
            .iter()
            .filter(|m| matches!(m.state, State::Queued | State::Held(_)) && m.to.name() == name && m.to.pane().is_none_or(|p| self.pane(p).is_none()))
            .map(|m| m.id.clone())
            .collect();
        (routes, records)
    }

    /// The labels of the space and tab that hold a pane.
    fn place(&self, pane: PaneId) -> (String, String) {
        self.locate(pane).map_or_else(Default::default, |(si, ti)| (self.spaces[si].label(), self.spaces[si].tabs[ti].label(ti)))
    }

    fn set_isolation(&mut self, target: IsolationTarget, isolation: Option<Isolation>, restart: bool) -> Result<(), String> {
        if let Some(i) = &isolation {
            if let Files::Account(n) = &i.files
                && !policy::current().accounts.iter().any(|a| &a.name == n)
            {
                return Err(format!("{n} is not an admitted account: Settings → Messages → Accounts…"));
            }
            if let policy::Network::Providers(agents) = &i.network {
                crate::isolation::providers_net(agents)?;
            }
        }
        match target {
            IsolationTarget::Pane(id) => {
                let i = isolation.ok_or("a pane always has an isolation")?;
                self.pane_mut(id).ok_or("that pane is gone")?.isolation = i;
                if restart {
                    self.restart_pane(id);
                }
            }
            IsolationTarget::Tab(si, ti) => self.spaces.get_mut(si).and_then(|s| s.tabs.get_mut(ti)).ok_or("that tab is gone")?.isolation_new = isolation,
            IsolationTarget::Space(si) => self.spaces.get_mut(si).ok_or("that space is gone")?.isolation_new = isolation,
        }
        self.dirty = true;
        Ok(())
    }

    pub fn mail_view(&self) -> MailView {
        let p = policy::current();
        let wall = crate::hook::now();
        let named = self.named_panes();
        let card = |n: &NamedPane| Card { name: n.name.clone(), pane: Some(n.pane), tab: n.tab.clone(), agent: n.agent.clone(), uid: n.uid, isolation: n.isolation.clone() };
        let order: Vec<usize> = if self.view.hits.sidebar_spaces.is_empty() { (0..self.spaces.len()).collect() } else { self.view.hits.sidebar_spaces.iter().map(|(_, i)| *i).collect() };
        let mut columns: Vec<MailColumn> = order
            .iter()
            .filter_map(|i| self.spaces.get(*i))
            .map(|sp| {
                let label = sp.label();
                MailColumn { cards: named.iter().filter(|n| n.space == label).map(card).collect(), space: Some(label) }
            })
            .collect();
        let mut last = vec![Card { name: "you".into(), pane: None, tab: String::new(), agent: None, uid: policy::owner_uid(), isolation: Isolation::default() }];
        let mut loose: Vec<String> = p
            .routes
            .iter()
            .flat_map(|r| [&r.from, &r.to])
            .filter_map(|s| if let Scope::Pane(n) = s { Some(n.clone()) } else { None })
            .chain(self.mail.msgs.iter().filter(|m| matches!(m.state, State::Queued | State::Held(_)) && m.to != Party::You).map(|m| m.to.name().to_string()))
            .filter(|n| !named.iter().any(|x| &x.name == n))
            .collect();
        loose.sort();
        loose.dedup();
        last.extend(loose.into_iter().map(|name| Card { name, pane: None, tab: String::new(), agent: None, uid: policy::owner_uid(), isolation: Isolation::default() }));
        columns.push(MailColumn { space: None, cards: last });
        let net = |name: &str| named.iter().find(|n| n.name == name).map(|n| n.isolation.network != policy::Network::None);
        let routes = p
            .routes
            .iter()
            .enumerate()
            .map(|(index, r)| {
                let risk = match (&r.from, &r.to) {
                    (Scope::Pane(a), Scope::Pane(b)) => r.action == Action::Allow && matches!((net(a), net(b)), (Some(x), Some(y)) if x != y),
                    _ => false,
                };
                RouteView { index, route: r.clone(), risk, left: r.until.map(|u| u.saturating_sub(wall)) }
            })
            .collect();
        let mut traffic: Vec<Traffic> = Vec::new();
        for m in &self.mail.msgs {
            let (from, to) = (m.from.name(), m.to.name());
            let i = match traffic.iter().position(|t| t.from == from && t.to == to) {
                Some(i) => i,
                None => {
                    traffic.push(Traffic { from: from.into(), to: to.into(), held: 0, today: 0 });
                    traffic.len() - 1
                }
            };
            traffic[i].held += usize::from(matches!(m.state, State::Held(_)));
            traffic[i].today += usize::from(wall.saturating_sub(m.created) < DAY);
        }
        traffic.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
        let mut today: Vec<Message> = self.mail.msgs.iter().filter(|m| wall.saturating_sub(m.created) < DAY).cloned().collect();
        today.reverse();
        MailView {
            on: p.on,
            mode: p.mode,
            broken: policy::broken(),
            columns,
            routes,
            traffic,
            waiting: self.mail.waiting().cloned().collect(),
            today,
        }
    }


    /// What a message between those names would get now; `to` may be "you".
    #[cfg(test)]
    pub fn mail_between(&self, from: &str, to: &str) -> Verdict {
        Self::mail_between_in(&self.named_panes(), from, to)
    }

    /// As `mail_between`, over named panes taken once, for a window that asks for many pairs.
    pub fn mail_between_in(named: &[NamedPane], from: &str, to: &str) -> Verdict {
        let find = |name: &str| named.iter().find(|n| n.name == name);
        let sender = match find(from) {
            Some(n) => Sender { name: n.name.clone(), space: n.space.clone(), tab: n.tab.clone(), uid: n.uid, agent: n.agent.clone() },
            None => Sender { name: from.into(), space: String::new(), tab: String::new(), uid: policy::owner_uid(), agent: None },
        };
        let recipient = match (to, find(to)) {
            ("you", _) => Recipient::You,
            (_, Some(n)) => Recipient::Pane { name: n.name.clone(), space: n.space.clone(), tab: n.tab.clone(), uid: n.uid, agent: n.agent.clone() },
            (_, None) => Recipient::Pane { name: to.into(), space: String::new(), tab: String::new(), uid: policy::owner_uid(), agent: None },
        };
        policy::decide(&policy::current(), &sender, &recipient, None, crate::hook::now())
    }

    pub fn rename_check(&self, pane: PaneId, name: &str) -> RenameCheck {
        let name = name.trim();
        if !policy::valid_address(name) {
            return RenameCheck::Label;
        }
        if let Some(other) = self.pane_by_name(name).filter(|o| *o != pane) {
            let (space, tab) = self.place(other);
            return RenameCheck::Taken { space, tab };
        }
        let (routes, records) = self.history(name);
        if routes.is_empty() && records.is_empty() {
            return RenameCheck::Address;
        }
        let p = policy::current();
        RenameCheck::History { routes: routes.iter().map(|i| p.routes[*i].said()).collect(), waiting: records.len() }
    }

    /// Expiry once a second, then delivery; true: redraw.
    pub fn tick_mail(&mut self) -> bool {
        let mut changed = false;
        if self.mail.expired_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.mail.expired_at = Some(Instant::now());
            changed = self.mail.expire(crate::hook::now()) > 0;
            self.mail.end_waits();
        }
        self.tick_deliver() || changed
    }

    /// Every pane with an address-shaped label: spaces, then tabs in order, then panes by id.
    pub(crate) fn named_panes(&self) -> Vec<NamedPane> {
        let mut out = Vec::new();
        for sp in &self.spaces {
            for (ti, t) in sp.tabs.iter().enumerate() {
                let mut ids: Vec<&PaneId> = t.panes.keys().collect();
                ids.sort();
                for id in ids {
                    let p = &t.panes[id];
                    let Some(name) = p.label.clone().filter(|l| policy::valid_address(l)) else { continue };
                    out.push(NamedPane { name, pane: *id, space: sp.label(), tab: t.label(ti), agent: p.agent.clone(), uid: self.pane_uid(*id), isolation: p.isolation.clone() });
                }
            }
        }
        out
    }

    pub(crate) fn pane_by_name(&self, name: &str) -> Option<PaneId> {
        self.spaces.iter().flat_map(|s| &s.tabs).flat_map(|t| t.panes.values()).find(|p| p.label.as_deref() == Some(name)).map(|p| p.id)
    }

    /// owner_uid(), or the admitted account's uid for a pane that runs as one.
    pub(crate) fn pane_uid(&self, pane: PaneId) -> u32 {
        match self.pane(pane).map(|p| &p.isolation.files) {
            Some(Files::Account(n)) => policy::current().accounts.iter().find(|a| &a.name == n).map_or_else(policy::owner_uid, |a| a.uid),
            _ => policy::owner_uid(),
        }
    }

    /// None for an unnamed pane; `uid` as the kernel said.
    pub(crate) fn sender_of(&self, pane: PaneId, uid: u32) -> Option<Sender> {
        let name = self.pane(pane)?.label.clone().filter(|l| policy::valid_address(l))?;
        let (space, tab) = self.place(pane);
        Some(Sender { name, space, tab, uid, agent: self.pane(pane)?.agent.clone() })
    }

    /// None when the party names a pane that is gone.
    pub(crate) fn recipient_of(&self, to: &Party) -> Option<Recipient> {
        match to {
            Party::You => Some(Recipient::You),
            Party::Pane { pane, name, .. } => {
                let id = (*pane)?;
                let p = self.pane(id)?;
                let (space, tab) = self.place(id);
                Some(Recipient::Pane { name: name.clone(), space, tab, uid: self.pane_uid(id), agent: p.agent.clone() })
            }
        }
    }

    /// The reply right, from the record `m` answers: sent by `m`'s recipient to `m`'s sender, and let through by the policy.
    pub(crate) fn reply_facts(&self, m: &Message) -> Option<ThreadFacts> {
        let r = self.mail.get(m.reply_to.as_deref()?)?;
        let passed = matches!(r.state, State::Queued | State::Delivered | State::Read | State::Answered);
        (passed && r.to.name() == m.from.name() && r.from.name() == m.to.name()).then(|| ThreadFacts { first_from: r.from.name().into(), first_to: r.to.name().into() })
    }

    /// The delivery re-check: the policy as it is now, unless the owner let it through.
    pub(crate) fn verdict_for(&self, m: &Message, now: u64) -> Verdict {
        if m.passed || m.from == Party::You {
            return Verdict { action: Action::Allow, why: "passed by you".into(), route: None };
        }
        let Some(recipient) = self.recipient_of(&m.to) else {
            return Verdict { action: Action::Ask, why: format!("no pane holds {} now", m.to.name()), route: None };
        };
        let (pane, uid) = match &m.from {
            Party::Pane { pane, uid, .. } => (*pane, *uid),
            Party::You => (None, policy::owner_uid()),
        };
        let live = pane.filter(|p| self.pane(*p).is_some());
        let (space, tab) = live.map(|p| self.place(p)).unwrap_or_default();
        let sender = Sender { name: m.from.name().into(), space, tab, uid, agent: live.and_then(|p| self.pane(p)?.agent.clone()) };
        policy::decide(&policy::current(), &sender, &recipient, self.reply_facts(m).as_ref(), now)
    }

    /// The one way the policy changes: a copy, `f`, expired routes pruned, saved, then swapped in; a failed save changes nothing.
    pub(crate) fn edit_policy(&mut self, f: impl FnOnce(&mut Policy)) -> Result<(), String> {
        let mut p = (*policy::current()).clone();
        f(&mut p);
        let now = crate::hook::now();
        p.routes.retain(|r| r.until.is_none_or(|t| t > now));
        policy::save(&p).map_err(|e| format!("policy.json not written: {e}"))?;
        policy::replace(p);
        Ok(())
    }
}

/// A new message in Queued; the caller sets the rest.
fn message(id: String, from: Party, to: Party, text: String, now: u64) -> Message {
    Message { thread: id.clone(), id, reply_to: None, from, to, text, hops: Vec::new(), auto: false, created: now, state: State::Queued, via: None, events: vec![Step { state: State::Queued, at: now, note: None }], passed: false }
}

/// Its state from the start; a refused record keeps no text.
fn stamp(m: &mut Message, state: State, now: u64) {
    if matches!(state, State::Refused(_)) {
        m.text.clear();
    }
    m.events = vec![Step { state: state.clone(), at: now, note: None }];
    m.state = state;
}

fn checked_text(text: &str) -> Result<String, String> {
    let text = crate::mail::clean(text);
    if text.is_empty() {
        return Err("an empty message".into());
    }
    if text.len() > crate::mail::MESSAGE_BYTES {
        return Err(format!("text over {} bytes", crate::mail::MESSAGE_BYTES));
    }
    Ok(text)
}

fn scope_of(p: &Party) -> Scope {
    match p {
        Party::You => Scope::You,
        Party::Pane { name, .. } => Scope::Pane(name.clone()),
    }
}


fn recipient(n: &NamedPane) -> Recipient {
    Recipient::Pane { name: n.name.clone(), space: n.space.clone(), tab: n.tab.clone(), uid: n.uid, agent: n.agent.clone() }
}

fn set_to_name(m: &mut Message, name: &str) {
    if let Party::Pane { name: n, .. } = &mut m.to {
        *n = name.to_string();
    }
}

/// A macOS account's uid, from the user database.
fn account_uid(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: a zeroed passwd is a valid value; getpwnam_r writes into it and `buf` only, and `out` points at `pw` or is null.
    let mut pw: nix::libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as nix::libc::c_char; 4096];
    let mut out: *mut nix::libc::passwd = std::ptr::null_mut();
    let r = unsafe { nix::libc::getpwnam_r(c.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    (r == 0 && !out.is_null()).then_some(pw.pw_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Layout, Space, Tab};
    use crate::config::Config;

    /// Runs `f` with policy.json in a state folder of its own, the global copy reset after.
    fn with_policy(tag: &str, f: impl FnOnce()) {
        let _l = policy::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        policy::replace(Policy::default());
        crate::session::tests::with_temp_state_dir(tag, f);
        policy::replace(Policy::default());
    }

    /// Two spaces: "work" with panes 1 (planner) and 2 (unnamed), "research" with pane 3 (reader).
    fn app() -> App {
        let mut app = App::new(Config::default());
        let pane = |app: &App, id, label: Option<&str>| {
            let mut p = crate::app::snapshots::agent_pane(app, id, crate::term::AgentState::Idle, 0);
            p.label = label.map(Into::into);
            p
        };
        let mut layout = Layout::new(1);
        layout.split(crate::layout::Dir::Horizontal, 2);
        layout.focus(1);
        let work = Tab { name: Some("main".into()), layout, panes: HashMap::from([(1, pane(&app, 1, Some("planner"))), (2, pane(&app, 2, None))]), isolation_new: None };
        let research = Tab { name: None, layout: Layout::new(3), panes: HashMap::from([(3, pane(&app, 3, Some("reader")))]), isolation_new: None };
        app.spaces.push(Space { name: Some("work".into()), cwd: "/".into(), tabs: vec![work], active_tab: 0, isolation_new: None });
        app.spaces.push(Space { name: Some("research".into()), cwd: "/".into(), tabs: vec![research], active_tab: 0, isolation_new: None });
        app.next_id = 4;
        app
    }

    fn to(pane: Option<PaneId>, name: &str, state: State) -> Message {
        let id = crate::mail::new_id();
        Message {
            thread: id.clone(),
            id,
            reply_to: None,
            from: Party::Pane { pane: Some(1), name: "planner".into(), agent: None, uid: policy::owner_uid() },
            to: Party::Pane { pane, name: name.into(), agent: None, uid: policy::owner_uid() },
            text: "hi".into(),
            hops: vec!["planner".into(), name.into()],
            auto: false,
            created: crate::hook::now(),
            events: vec![Step { state: state.clone(), at: crate::hook::now(), note: None }],
            state,
            via: None,
            passed: false,
        }
    }

    fn every_action() -> Vec<MailAction> {
        let i = Isolation::default();
        vec![
            MailAction::SetRoute { from: Scope::All, to: Scope::All, action: Some(Action::No), until: None },
            MailAction::CycleRoute(0),
            MailAction::SetMode(Mode::Strict),
            MailAction::SetOn(false),
            MailAction::SetAllowFromPanes(true),
            MailAction::AdmitAccount("nobody".into()),
            MailAction::RemoveAccount("scout".into()),
            MailAction::Topology(vec![Route { from: Scope::All, to: Scope::All, action: Action::No, until: None }]),
            MailAction::Rename { pane: 1, name: Some("x".into()), keep: true },
            MailAction::SetIsolation { target: IsolationTarget::Pane(1), isolation: Some(i), restart: false },
            MailAction::Deliver("a".into()),
            MailAction::DeliverAllow("a".into()),
            MailAction::Drop("a".into()),
            MailAction::Reply { id: "a".into(), text: "t".into() },
            MailAction::Send { to: "all".into(), text: "t".into() },
            MailAction::AnswerRequest { id: "a".into(), action: Action::Allow },
        ]
    }

    #[test]
    fn every_action_is_refused_from_a_window_inside_a_pane() {
        with_policy("mail-inside", || {
            let mut app = app();
            let before = policy::current();
            for a in every_action() {
                assert_eq!(app.mail_act(a.clone(), true), Err(INSIDE_REFUSED.to_string()), "{a:?}");
            }
            assert_eq!(*policy::current(), *before);
            assert_eq!(app.pane(1).unwrap().label.as_deref(), Some("planner"));
            assert!(!policy::path().exists(), "nothing written");
        });
    }

    #[test]
    fn a_message_the_agent_has_taken_is_not_unread() {
        let mut mail = MailState::default();
        let m = to(Some(2), "reader", State::Delivered);
        mail.add(m.clone());
        assert_eq!(mail.unread(2), 1, "handed over but not taken yet");
        mail.set(&m.id, State::Delivered, Some(Via::Extension), None);
        assert_eq!(mail.unread(2), 0);
        mail.add(to(Some(2), "reader", State::Queued));
        assert_eq!(mail.unread(2), 1);
    }

    #[test]
    fn a_route_cycles_allow_ask_no_then_goes() {
        with_policy("mail-cycle", || {
            let mut app = app();
            let (from, to) = (Scope::parse("planner").unwrap(), Scope::parse("space:research").unwrap());
            app.mail_act(MailAction::SetRoute { from: from.clone(), to: to.clone(), action: Some(Action::Allow), until: None }, false).unwrap();
            let action = || policy::current().routes.first().map(|r| r.action);
            assert_eq!(action(), Some(Action::Allow));
            app.mail_act(MailAction::CycleRoute(0), false).unwrap();
            assert_eq!(action(), Some(Action::Ask));
            app.mail_act(MailAction::CycleRoute(0), false).unwrap();
            assert_eq!(action(), Some(Action::No));
            assert_eq!(policy::load_from(&policy::path()).unwrap().unwrap().routes[0].action, Action::No, "saved");
            app.mail_act(MailAction::CycleRoute(0), false).unwrap();
            assert_eq!(action(), None);
            assert_eq!(app.mail_act(MailAction::CycleRoute(0), false), Err("that route is gone".into()));
            assert_eq!(app.mail_between("planner", "reader").why, "open mode");
        });
    }

    #[test]
    fn a_duplicate_name_is_refused_with_its_place() {
        with_policy("mail-dup", || {
            let mut app = app();
            let r = app.mail_act(MailAction::Rename { pane: 2, name: Some(" reader ".into()), keep: false }, false);
            assert_eq!(r, Err("A pane in space research, tab 1 is already called reader".into()));
            assert!(matches!(app.rename_check(2, "reader"), RenameCheck::Taken { space, tab } if space == "research" && tab == "1"));
            assert!(matches!(app.rename_check(2, "two words"), RenameCheck::Label));
            assert!(matches!(app.rename_check(2, "scout"), RenameCheck::Address));
            assert!(matches!(app.rename_check(3, "reader"), RenameCheck::Address), "its own name");
            assert_eq!(app.pane(2).unwrap().label, None);
        });
    }

    #[test]
    fn renaming_into_a_name_with_history_keeps_or_drops_its_routes_and_mail() {
        for keep in [true, false] {
            with_policy("mail-history", || {
                let mut app = app();
                let route = MailAction::SetRoute { from: Scope::parse("planner").unwrap(), to: Scope::parse("scout").unwrap(), action: Some(Action::Allow), until: None };
                app.mail_act(route, false).unwrap();
                let held = to(None, "scout", State::Held(Hold::NoAgent));
                let gone = to(Some(99), "scout", State::Queued);
                app.mail.add(held.clone());
                app.mail.add(gone.clone());
                match app.rename_check(2, "scout") {
                    RenameCheck::History { routes, waiting } => assert_eq!((routes, waiting), (vec!["planner → scout allow".to_string()], 2)),
                    _ => panic!("history expected"),
                }
                app.mail_act(MailAction::Rename { pane: 2, name: Some("scout".into()), keep }, false).unwrap();
                assert_eq!(app.pane(2).unwrap().label.as_deref(), Some("scout"));
                let state = |id: &str| app.mail.get(id).unwrap().state.clone();
                if keep {
                    assert_eq!(policy::current().routes.len(), 1);
                    assert_eq!((state(&held.id), state(&gone.id)), (State::Queued, State::Queued));
                    assert_eq!(app.mail.get(&held.id).unwrap().to.pane(), Some(2));
                    assert_eq!(app.mail.unread(2), 2);
                } else {
                    assert!(policy::current().routes.is_empty());
                    assert_eq!((state(&held.id), state(&gone.id)), (State::Dropped, State::Dropped));
                }
            });
        }
    }

    #[test]
    fn clearing_a_name_holds_its_queued_mail() {
        with_policy("mail-clear", || {
            let mut app = app();
            let m = to(Some(3), "reader", State::Queued);
            app.mail.add(m.clone());
            app.mail_act(MailAction::Rename { pane: 3, name: Some("  ".into()), keep: false }, false).unwrap();
            assert_eq!(app.pane(3).unwrap().label, None);
            assert_eq!(app.mail.get(&m.id).unwrap().state, State::Held(Hold::Unnamed));
            assert!(app.mail.waiting().any(|w| w.id == m.id));
            app.mail_act(MailAction::Rename { pane: 3, name: Some("reader2".into()), keep: false }, false).unwrap();
            let back = app.mail.get(&m.id).unwrap();
            assert_eq!((back.state.clone(), back.to.name()), (State::Queued, "reader2"));
        });
    }

    #[test]
    fn a_failed_save_changes_nothing() {
        with_policy("mail-fail", || {
            let mut app = app();
            // A folder where the file should be: the rename over it fails.
            std::fs::create_dir_all(policy::path().join("x")).unwrap();
            let before = policy::current();
            let r = app.mail_act(MailAction::SetMode(Mode::Strict), false);
            assert!(r.as_ref().is_err_and(|e| e.starts_with("policy.json not written: ")), "{r:?}");
            assert_eq!(*policy::current(), *before);
        });
    }

    #[test]
    fn isolation_survives_the_snapshot() {
        let mut app = app();
        let folder = Isolation { files: Files::Folder, network: policy::Network::None };
        let provider = Isolation { files: Files::Everything, network: policy::Network::Providers(vec!["claude".into(), "gemini".into()]) };
        app.pane_mut(3).unwrap().isolation = folder.clone();
        app.spaces[0].tabs[0].isolation_new = Some(provider.clone());
        app.spaces[1].isolation_new = Some(folder.clone());
        let snap = app.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let snap: crate::session::WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        assert!(!json.contains("\"isolation\":{\"files\":\"everything\""), "an open pane writes nothing new");
        let mut back = App::new(Config::default());
        back.rebuild_tabs(&snap, |a, ps| {
            let mut p = crate::app::snapshots::agent_pane(a, ps.id, crate::term::AgentState::Idle, 0);
            p.isolation = ps.isolation.clone();
            Some(p)
        });
        assert_eq!(back.pane(3).unwrap().isolation, folder);
        assert!(back.pane(1).unwrap().isolation.is_open());
        assert_eq!(back.spaces[0].tabs[0].isolation_new, Some(provider));
        assert_eq!(back.spaces[1].isolation_new, Some(folder));
        assert_eq!(back.spaces[0].isolation_new, None);
    }

    #[test]
    fn restart_gives_the_pane_a_new_id_in_the_same_place() {
        let mut app = app();
        let folder = Isolation { files: Files::Folder, network: policy::Network::None };
        app.pane_mut(1).unwrap().isolation = folder.clone();
        let m = to(Some(1), "planner", State::Queued);
        app.mail.add(m.clone());
        assert!(!app.restart_pane(42), "no such pane");
        let new = app.alloc_id();
        let pty = crate::app::snapshots::agent_pane(&app, new, crate::term::AgentState::Idle, 0).pty;
        assert!(app.restarted(1, new, pty, None, "zsh".into()));
        assert!(app.pane(1).is_none());
        let p = app.pane(new).unwrap();
        assert_eq!((p.label.as_deref(), &p.isolation), (Some("planner"), &folder));
        let t = &app.spaces[0].tabs[0];
        assert_eq!(t.layout.pane_ids(), vec![new, 2], "in the same place");
        assert_eq!(t.layout.focused(), new);
        assert_eq!(app.mail.get(&m.id).unwrap().to.pane(), Some(new));
        assert_eq!(app.pane_by_name("planner"), Some(new));
    }

    #[test]
    fn a_restarted_pane_starts_with_the_old_programs_modes_off() {
        let mut app = app();
        let old = &mut app.pane_mut(1).unwrap().term;
        old.feed(b"kept above\r\n\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1h\x1b[?25l");
        assert!(old.wants_mouse());
        let new = app.alloc_id();
        let pty = crate::app::snapshots::agent_pane(&app, new, crate::term::AgentState::Idle, 0).pty;
        assert!(app.restarted(1, new, pty, None, "zsh".into()));
        let term = &mut app.pane_mut(new).unwrap().term;
        let s = term.screen();
        assert!(!term.wants_mouse(), "a mouse move would be typed into the shell");
        assert!(!s.alternate_screen() && !s.bracketed_paste() && !s.app_cursor() && !s.hide_cursor());
        assert!(term.all_lines().iter().any(|l| l == "kept above"), "the text stays");
    }

    fn req(to: &str, text: &str) -> SendReq {
        SendReq { to: to.into(), text: text.into(), reply_to: None, auto: false, ask_route: false }
    }

    fn send(app: &mut App, from: PaneId, to: &str, text: &str) -> SendReply {
        app.mail_send(from, policy::owner_uid(), req(to, text), crate::hook::now())
    }

    fn reply(app: &mut App, from: PaneId, to: &str, text: &str) -> SendReply {
        app.mail_send(from, policy::owner_uid(), SendReq { reply_to: Some(to.into()), ..req("", text) }, crate::hook::now())
    }

    fn id_of(r: &SendReply) -> String {
        match r {
            SendReply::Sent { id } | SendReply::Held { id, .. } => id.clone(),
            SendReply::Refused(why) | SendReply::Unreachable(why) => panic!("refused: {why}"),
        }
    }

    fn route(app: &mut App, from: &str, to: &str, action: Action) {
        app.mail_act(MailAction::SetRoute { from: Scope::parse(from).unwrap(), to: Scope::parse(to).unwrap(), action: Some(action), until: None }, false).unwrap();
    }

    /// A request through the loop's door, and the lines it answered with.
    fn ask(app: &mut App, pane: PaneId, r: Request) -> Vec<Value> {
        let (tx, rx) = std::sync::mpsc::channel();
        app.mail_request(pane, policy::owner_uid(), r, tx);
        rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect()
    }

    #[test]
    fn a_send_is_queued_held_or_refused_by_the_policy() {
        with_policy("mail-send", || {
            let mut app = app();
            let id = id_of(&send(&mut app, 1, "reader", "find X"));
            let m = app.mail.get(&id).unwrap().clone();
            assert_eq!((m.state.clone(), m.from.name(), m.from.pane(), m.to.pane()), (State::Queued, "planner", Some(1), Some(3)));
            assert_eq!((m.thread.clone(), m.hops.clone()), (id.clone(), vec!["planner".to_string(), "reader".to_string()]));
            route(&mut app, "planner", "reader", Action::Ask);
            let held = send(&mut app, 1, "reader", "again");
            assert!(matches!(&held, SendReply::Held { why, .. } if why == "route 1: planner → reader ask"), "{held:?}");
            assert_eq!(app.mail.get(&id_of(&held)).unwrap().state, State::Held(Hold::Route));
            route(&mut app, "planner", "reader", Action::No);
            assert_eq!(send(&mut app, 1, "reader", "third"), SendReply::Unreachable("route 1: planner → reader no".into()));
            let refused = app.mail.msgs.last().unwrap();
            assert_eq!((refused.state.clone(), refused.text.as_str()), (State::Refused("route 1: planner → reader no".into()), ""), "a refused record keeps no text");
            assert_eq!(send(&mut app, 2, "reader", "hi"), SendReply::Refused(UNNAMED.into()));
            assert_eq!(send(&mut app, 1, "nobody", "hi"), SendReply::Unreachable("no pane is called nobody (the owner names one with Rename pane…)".into()));
            // only an unknown name or a route lists whom it may write to; other refusals, even to that name, do not
            assert_eq!(send(&mut app, 1, "planner", "hi"), SendReply::Refused("a pane cannot write to itself".into()));
            assert_eq!(send(&mut app, 1, "reader", "\x07"), SendReply::Refused("an empty message".into()));
            assert_eq!(send(&mut app, 1, "reader", &"x".repeat(crate::mail::MESSAGE_BYTES + 1)), SendReply::Refused("text over 65536 bytes".into()));
            app.mail_act(MailAction::SetOn(false), false).unwrap();
            assert_eq!(send(&mut app, 1, "reader", "hi"), SendReply::Refused("messaging is off".into()));
        });
    }

    #[test]
    fn limits_hold_and_duplicates_refuse() {
        with_policy("mail-limits", || {
            let mut app = app();
            id_of(&send(&mut app, 1, "reader", "one"));
            assert_eq!(send(&mut app, 1, "reader", "one"), SendReply::Refused("the same text to reader within 60 s".into()));
            for i in 0..4 {
                id_of(&send(&mut app, 1, "reader", &format!("n{i}")));
            }
            let held = send(&mut app, 1, "reader", "seventh");
            assert!(matches!(&held, SendReply::Held { why, .. } if why == "limit per_pair_per_minute"), "{held:?}");
            assert_eq!(app.mail.get(&id_of(&held)).unwrap().state, State::Held(Hold::Limit("per_pair_per_minute".into())));
            let before = app.mail.msgs.len();
            assert!(matches!(send(&mut app, 1, "reader", "one"), SendReply::Refused(_)));
            assert_eq!(app.mail.msgs.len(), before, "a refusal past the pair limit is not written");
            for _ in 0..crate::mail::HELD_TOTAL {
                app.mail.add(to(Some(1), "planner", State::Held(Hold::Route)));
            }
            assert_eq!(send(&mut app, 3, "planner", "late"), SendReply::Refused("the owner already has 100 held messages; try later".into()));
        });
    }

    #[test]
    fn a_broadcast_holds_past_eight() {
        with_policy("mail-all", || {
            let mut app = app();
            for i in 0..10 {
                let mut p = crate::app::snapshots::agent_pane(&app, 10 + i, crate::term::AgentState::Idle, 0);
                p.label = Some(format!("w{i}"));
                app.spaces[1].tabs[0].panes.insert(10 + i, p);
            }
            let r = send(&mut app, 1, "all", "standup");
            let thread = id_of(&r);
            assert_eq!(r, SendReply::Held { id: thread.clone(), why: "3 of 11 held: limit broadcast".into() });
            let all: Vec<&Message> = app.mail.msgs.iter().filter(|m| m.thread == thread).collect();
            assert_eq!(all.len(), 11);
            assert_eq!(all.iter().filter(|m| m.state == State::Queued).count(), 8);
            assert_eq!(all.iter().filter(|m| m.state == State::Held(Hold::Limit("broadcast".into()))).count(), 3);
            assert!(all.iter().all(|m| m.to.pane() != Some(1) && m.to.pane() != Some(2)), "not the sender, not an unnamed pane");
            route(&mut app, "planner", "*", Action::No);
            assert_eq!(send(&mut app, 1, "all", "again"), SendReply::Refused("no pane you may write to".into()));
        });
    }

    #[test]
    fn a_reply_goes_to_the_sender_in_the_same_thread_and_answers_it() {
        with_policy("mail-reply", || {
            let mut app = app();
            let q = id_of(&send(&mut app, 1, "reader", "find X"));
            let r = SendReq { reply_to: Some(q.clone()), auto: true, ..req("", "found") };
            let a = id_of(&app.mail_send(3, policy::owner_uid(), r, crate::hook::now()));
            let m = app.mail.get(&a).unwrap();
            assert_eq!((m.to.pane(), m.thread.as_str(), m.reply_to.as_deref(), m.auto, m.state.clone()), (Some(1), q.as_str(), Some(q.as_str()), true, State::Queued));
            assert_eq!(m.hops, vec!["planner", "reader", "planner"]);
            assert_eq!(app.mail.get(&q).unwrap().state, State::Answered);
            assert_eq!(reply(&mut app, 1, &q, "mine"), SendReply::Refused(format!("no message {q} to this pane")));
        });
    }

    #[test]
    fn a_reply_to_a_gone_sender_is_held() {
        with_policy("mail-gone", || {
            let mut app = app();
            let q = id_of(&send(&mut app, 1, "reader", "find X"));
            app.spaces[0].tabs[0].panes.remove(&1);
            let r = reply(&mut app, 3, &q, "found");
            assert!(matches!(&r, SendReply::Held { why, .. } if why == "no pane holds the name now"), "{r:?}");
            let m = app.mail.get(&id_of(&r)).unwrap();
            assert_eq!((m.to.pane(), m.to.name(), m.state.clone()), (None, "planner", State::Held(Hold::NoAgent)));
        });
    }

    #[test]
    fn messages_to_you_wait_for_the_owner() {
        with_policy("mail-you", || {
            let mut app = app();
            let id = id_of(&send(&mut app, 1, "you", "done"));
            assert_eq!(app.mail.get(&id).unwrap().state, State::Delivered);
            assert!(app.mail.waiting().any(|m| m.id == id));
            app.mail_act(MailAction::Reply { id: id.clone(), text: "thanks".into() }, false).unwrap();
            let back = app.mail.msgs.last().unwrap();
            assert_eq!((back.from.clone(), back.to.pane(), back.passed, back.reply_to.as_deref()), (Party::You, Some(1), true, Some(id.as_str())));
            assert_eq!(app.mail.get(&id).unwrap().state, State::Answered);
            let other = id_of(&send(&mut app, 3, "you", "also"));
            app.mail_act(MailAction::Drop(other.clone()), false).unwrap();
            assert_eq!(app.mail.get(&other).unwrap().state, State::Read);
            assert!(!app.mail.waiting().any(|m| m.id == other));
            assert_eq!(app.mail_act(MailAction::Drop(other.clone()), false), Err(format!("message {other} is not waiting")));
            app.mail_act(MailAction::Send { to: "all".into(), text: "hello all".into() }, false).unwrap();
            let sent: Vec<&Message> = app.mail.msgs.iter().filter(|m| m.text == "hello all").collect();
            assert_eq!(sent.len(), 2, "planner and reader");
            assert!(sent.iter().all(|m| m.from == Party::You && m.passed && m.state == State::Queued && m.thread == sent[0].id));
            assert_eq!(app.mail_act(MailAction::Send { to: "nobody".into(), text: "x".into() }, false), Err("no pane is called nobody".into()));
        });
    }

    #[test]
    fn deliver_and_allow_writes_a_route_for_an_hour() {
        with_policy("mail-deliver", || {
            let mut app = app();
            route(&mut app, "planner", "reader", Action::Ask);
            let id = id_of(&send(&mut app, 1, "reader", "find X"));
            app.mail_act(MailAction::DeliverAllow(id.clone()), false).unwrap();
            let m = app.mail.get(&id).unwrap();
            assert_eq!((m.state.clone(), m.passed), (State::Queued, true));
            let p = policy::current();
            assert_eq!(p.routes.len(), 1, "the ask route with the same ends was replaced");
            let r = &p.routes[0];
            assert_eq!((r.from.text(), r.to.text(), r.action), ("planner".to_string(), "reader".to_string(), Action::Allow));
            assert!(r.until.is_some_and(|u| u.abs_diff(crate::hook::now() + 3600) <= 2), "{:?}", r.until);
            assert_eq!(app.mail_act(MailAction::Deliver(id.clone()), false), Err(format!("message {id} is not waiting")));
        });
    }

    #[test]
    fn a_route_request_answer_writes_the_route() {
        with_policy("mail-request", || {
            let mut app = app();
            app.mail_act(MailAction::SetMode(Mode::Strict), false).unwrap();
            assert_eq!(send(&mut app, 1, "reader", "hi"), SendReply::Unreachable("no route planner → reader, strict mode".into()));
            let r = app.mail_send(1, policy::owner_uid(), SendReq { ask_route: true, ..req("reader", "") }, crate::hook::now());
            assert!(matches!(&r, SendReply::Held { why, .. } if why == "asked the owner for a route to reader"), "{r:?}");
            let id = id_of(&r);
            let m = app.mail.get(&id).unwrap();
            assert_eq!((m.state.clone(), m.text.as_str()), (State::Held(Hold::RouteRequest), ""));
            let again = app.mail_send(1, policy::owner_uid(), SendReq { ask_route: true, ..req("reader", "") }, crate::hook::now());
            assert_eq!(id_of(&again), id, "asked once");
            assert_eq!(app.mail_act(MailAction::Deliver(id.clone()), false), Err(format!("message {id} is not waiting")), "a request carries no message");
            app.mail_act(MailAction::AnswerRequest { id: id.clone(), action: Action::Allow }, false).unwrap();
            assert_eq!(app.mail.get(&id).unwrap().state, State::Answered);
            let r = policy::current().routes[0].clone();
            assert_eq!((r.from.text(), r.to.text(), r.action, r.until), ("planner".to_string(), "reader".to_string(), Action::Allow, None));
            assert!(matches!(send(&mut app, 1, "reader", "now"), SendReply::Sent { .. }));
        });
    }

    #[test]
    fn wait_ends_on_count_timeout_or_expiry() {
        with_policy("mail-wait", || {
            let mut app = app();
            let q = id_of(&send(&mut app, 1, "reader", "find X"));
            let (tx, rx) = std::sync::mpsc::channel();
            app.mail_request(1, policy::owner_uid(), Request::Wait { id: q.clone(), thread: false, count: 1, timeout: 600 }, tx);
            assert!(rx.try_recv().is_err(), "nothing yet");
            let a = id_of(&reply(&mut app, 3, &q, "found"));
            let got: Vec<Value> = rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
            assert_eq!(got.len(), 2, "{got:?}");
            assert_eq!((got[0]["message"]["id"].clone(), got[1].clone()), (json!(a), json!({"done": {"count": 1}})));
            assert!(rx.recv().is_err(), "the wait let go of its line");
            assert_eq!(app.mail.get(&a).unwrap().state, State::Read);

            let early = id_of(&send(&mut app, 3, "planner", "ping"));
            let pong = id_of(&reply(&mut app, 1, &early, "pong"));
            let got = ask(&mut app, 3, Request::Wait { id: early.clone(), thread: false, count: 1, timeout: 600 });
            assert_eq!(got, vec![json!({"message": app.mail.get(&pong).map(|m| Message { state: State::Queued, events: m.events[..1].to_vec(), via: None, ..m.clone() })}), json!({"done": {"count": 1}})], "a reply that came first");

            let (tx, rx) = std::sync::mpsc::channel();
            app.mail_request(1, policy::owner_uid(), Request::Wait { id: q.clone(), thread: true, count: 5, timeout: 0 }, tx);
            app.mail.end_waits();
            assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec!["{\"timeout\":{}}\n".to_string()]);

            let lost = id_of(&send(&mut app, 1, "reader", "never read"));
            let (tx, rx) = std::sync::mpsc::channel();
            app.mail_request(1, policy::owner_uid(), Request::Wait { id: lost.clone(), thread: false, count: 1, timeout: 600 }, tx);
            app.mail.set(&lost, State::Expired, None, None);
            app.mail.end_waits();
            assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![format!("{{\"expired\":{{\"id\":\"{lost}\"}}}}\n")]);
            assert_eq!(ask(&mut app, 2, Request::Wait { id: q.clone(), thread: false, count: 1, timeout: 1 }), vec![json!({"error": format!("no message {q} for this pane")})]);
        });
    }

    #[test]
    fn a_lone_pane_is_named_from_its_space_row_by_mouse_and_then_sends() {
        use crate::app::{Ask, MenuCtx, Overlay};
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind::Down};
        use ratatui::layout::Rect;
        with_policy("mail-lone", || {
            let mut app = app();
            app.screen = Rect::new(0, 0, 120, 40);
            app.spaces[1].tabs[0].panes.get_mut(&3).unwrap().label = None;
            assert_eq!(ask(&mut app, 3, Request::Agents {}), vec![json!({"error": UNNAMED})]);
            // a space of one pane draws no pane row: its own row is the only way in
            app.view.hits.sidebar_spaces = vec![(Rect::new(0, 5, 20, 1), 1)];
            let click = |app: &mut App, b, x, y| {
                let screen = app.screen;
                app.mouse(MouseEvent { kind: Down(b), column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE }, screen)
            };
            click(&mut app, MouseButton::Right, 2, 5);
            let Overlay::Menu(m, MenuCtx::Space(1)) = &app.overlay else { panic!("the space's menu") };
            let i = m.items.iter().position(|it| it.label == "Rename pane…").expect("the space's menu names its one pane");
            let r = m.rect(app.screen);
            click(&mut app, MouseButton::Left, r.x + 2, r.y + 1 + i as u16);
            assert!(matches!(&app.overlay, Overlay::Prompt(_, Ask::RenamePane(3))));
            "scout".chars().for_each(|c| app.key(KeyEvent::from(KeyCode::Char(c))));
            app.key(KeyEvent::from(KeyCode::Enter));
            assert_eq!(app.pane(3).and_then(|p| p.label.as_deref()), Some("scout"));
            assert_eq!(ask(&mut app, 3, Request::Agents {})[0]["me"], json!("scout"), "the caller's own name, apart from you");
            assert!(matches!(send(&mut app, 3, "planner", "hi"), SendReply::Sent { .. }));
            app.spaces[1].tabs[0].layout.split(crate::layout::Dir::Horizontal, 4);
            let four = crate::app::snapshots::agent_pane(&app, 4, crate::term::AgentState::Idle, 0);
            app.spaces[1].tabs[0].panes.insert(4, four);
            let Some(Overlay::Menu(m, _)) = app.space_menu(1, (1, 1)) else { panic!("a menu") };
            assert!(m.items.iter().all(|it| it.label != "Rename pane…"), "two panes: each has its own row");
        });
    }

    #[test]
    fn agents_lists_only_who_the_caller_may_write_to() {
        with_policy("mail-agents", || {
            let mut app = app();
            let names = |v: &[Value]| -> Vec<(String, String)> { v[0]["agents"].as_array().unwrap().iter().map(|a| (a["name"].as_str().unwrap().to_string(), a["may"].as_str().unwrap().to_string())).collect() };
            let pair = |n: &str, m: &str| (n.to_string(), m.to_string());
            let got = ask(&mut app, 1, Request::Agents {});
            assert_eq!(names(&got), vec![pair("reader", "write"), pair("you", "write")]);
            let reader = &got[0]["agents"][0];
            assert_eq!((reader["place"].clone(), reader["pane"].clone(), reader["session_name"].clone()), (json!("research/1"), json!(3), Value::Null));
            route(&mut app, "planner", "reader", Action::Ask);
            assert_eq!(names(&ask(&mut app, 1, Request::Agents {}))[0], pair("reader", "ask"));
            route(&mut app, "planner", "reader", Action::No);
            assert_eq!(names(&ask(&mut app, 1, Request::Agents {})), vec![pair("you", "write")]);
            assert_eq!(ask(&mut app, 2, Request::Agents {}), vec![json!({"error": UNNAMED})]);
        });
    }

    #[test]
    fn a_changed_list_is_said_once_and_a_refused_name_gets_the_list() {
        with_policy("mail-reachable", || {
            let mut app = app();
            let send = |app: &mut App, to: &str, text: &str| ask(app, 1, Request::Send { to: to.into(), text: text.into(), reply_to: None, auto: false, ask_route: false }).remove(0);
            let inbox = |app: &mut App| ask(app, 1, Request::Inbox { all: false }).remove(0);
            route(&mut app, "planner", "reader", Action::Ask);
            assert_eq!(inbox(&mut app).get("reachable_changed"), None, "a pane that never listed has nothing gone stale");
            ask(&mut app, 1, Request::Agents {});
            assert_eq!(inbox(&mut app).get("reachable_changed"), None, "nothing changed");
            app.pane_mut(3).unwrap().label = Some("scout".into());
            assert_eq!(inbox(&mut app)["reachable_changed"], json!(true), "a rename");
            assert_eq!(inbox(&mut app).get("reachable_changed"), None, "once per change");
            app.spaces[1].name = Some("lab".into());
            assert_eq!(send(&mut app, "scout", "a")["reachable_changed"], json!(true), "a space renamed");
            route(&mut app, "planner", "scout", Action::Ask);
            let waited = ask(&mut app, 1, Request::Wait { id: "nope".into(), thread: false, count: 1, timeout: 1 });
            assert_eq!(waited[0], json!({"reachable_changed": true}), "access changed, said before the wait's own lines");
            route(&mut app, "planner", "scout", Action::No);
            let r = send(&mut app, "scout", "b");
            assert!(r["refused"].as_str().unwrap().ends_with("; may write to: you"), "{r}");
            assert_eq!(r.get("reachable_changed"), None, "the refusal already showed the list");
            let r = send(&mut app, "nobody", "c");
            assert!(r["refused"].as_str().unwrap().starts_with("no pane is called nobody") && r["refused"].as_str().unwrap().ends_with("; may write to: you"), "{r}");
            route(&mut app, "planner", "you", Action::Ask);
            assert_eq!(inbox(&mut app)["reachable_changed"], json!(true), "a route to you");
            assert_eq!(ask(&mut app, 1, Request::Agents {})[0]["agents"], json!([{"name": "you", "agent": null, "state": "-", "place": "-", "unread": 0, "session_name": null, "pane": null, "may": "ask"}]));
            route(&mut app, "planner", "you", Action::No);
            assert_eq!(ask(&mut app, 1, Request::Agents {})[0]["agents"], json!([]), "you is not listed when a route says no");
            assert!(send(&mut app, "nobody", "d")["refused"].as_str().unwrap().ends_with("; may write to: nobody"));
        });
    }

    #[test]
    fn inbox_and_show_never_hand_out_held_mail() {
        with_policy("mail-inbox", || {
            let mut app = app();
            route(&mut app, "planner", "reader", Action::Ask);
            app.mail_act(MailAction::Send { to: "reader".into(), text: "find X".into() }, false).unwrap();
            let q = app.mail.msgs.last().unwrap().id.clone();
            let held = id_of(&send(&mut app, 1, "reader", "secret"));
            assert_eq!(ask(&mut app, 3, Request::Show { id: held.clone() }), vec![json!({"error": format!("no message {held} for this pane")})]);
            let got = ask(&mut app, 3, Request::Inbox { all: false });
            let ids: Vec<&str> = got[0]["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
            assert_eq!(ids, vec![q.as_str()]);
            assert_eq!(app.mail.get(&q).unwrap().state, State::Read);
            assert_eq!(ask(&mut app, 3, Request::Inbox { all: false })[0]["messages"], json!([]));
            assert_eq!(ask(&mut app, 3, Request::Inbox { all: true })[0]["messages"].as_array().unwrap().len(), 1);
            assert_eq!(ask(&mut app, 1, Request::Show { id: held.clone() })[0]["messages"][0]["id"], json!(held), "the sender sees its own");
        });
    }

    #[test]
    fn inbox_checks_the_policy_again() {
        with_policy("mail-inbox-recheck", || {
            let mut app = app();
            let q = id_of(&send(&mut app, 1, "reader", "find X"));
            route(&mut app, "planner", "reader", Action::No);
            assert_eq!(ask(&mut app, 3, Request::Show { id: q.clone() }), vec![json!({"error": format!("no message {q} for this pane")})]);
            assert_eq!(ask(&mut app, 3, Request::Inbox { all: false })[0]["messages"], json!([]));
            assert!(matches!(app.mail.get(&q).unwrap().state, State::Refused(_)), "a route added after the send stops it");
        });
    }

    #[test]
    fn a_flood_stays_inside_the_limits() {
        use crate::mail::{COMPACT_BYTES, HELD_TOTAL, QUEUED_PER_RECIPIENT};
        with_policy("mail-flood", || {
            let mut app = App::new(Config::default());
            let tabs = (1..=10)
                .map(|id| {
                    let mut p = crate::app::snapshots::agent_pane(&app, id, crate::term::AgentState::Idle, 0);
                    p.label = Some(format!("p{id}"));
                    Tab { name: None, layout: Layout::new(id), panes: HashMap::from([(id, p)]), isolation_new: None }
                })
                .collect();
            app.spaces.push(Space { name: None, cwd: "/".into(), tabs, active_tab: 0, isolation_new: None });
            app.next_id = 11;
            app.mail = MailState::open(&crate::mail::path());
            let text = |i: usize| format!("{i:08} {}", "x".repeat(24 * 1024));
            let (mut now, mut appended, mut kinds) = (crate::hook::now(), 0usize, [0usize; 3]);
            for i in 0..1000 {
                now += 1 + (i as u64 % 7) * 3;
                let r = app.mail_send(1 + i % 9, policy::owner_uid(), req("p10", &text(i)), now);
                match &r {
                    SendReply::Sent { .. } => kinds[0] += 1,
                    SendReply::Held { why, .. } => {
                        assert!(!why.is_empty(), "a held send says why");
                        kinds[1] += 1;
                    }
                    SendReply::Refused(why) | SendReply::Unreachable(why) => {
                        assert!(!why.is_empty(), "a refused send says why");
                        kinds[2] += 1;
                    }
                }
                if !matches!(r, SendReply::Refused(_)) {
                    appended += 1;
                }
                let queued = app.mail.msgs.iter().filter(|m| m.state == State::Queued && m.to.pane() == Some(10)).count();
                let held = app.mail.msgs.iter().filter(|m| matches!(m.state, State::Held(_))).count();
                assert!(queued <= QUEUED_PER_RECIPIENT && held <= HELD_TOTAL, "call {i}: {queued} queued, {held} held");
                // The recipient reads its mail now and then; finished records older than a week leave memory, as they would.
                if i % 100 == 99 {
                    let ids: Vec<String> = app.mail.msgs.iter().filter(|m| m.state == State::Queued).map(|m| m.id.clone()).collect();
                    appended += ids.len();
                    for id in ids {
                        app.mail.set(&id, State::Read, None, None);
                    }
                    app.mail.expire(now + crate::mail::HELD_TTL_SECS + 1);
                }
            }
            assert!(kinds.iter().all(|&k| k > 0), "sent, held and refused all happened: {kinds:?}");
            assert!(appended * 24 * 1024 > COMPACT_BYTES as usize, "the flood wrote enough to need compacting: {appended} records");
            let size = std::fs::metadata(crate::mail::path()).unwrap().len();
            // Compaction comes COMPACT_BYTES past the live set it last wrote.
            let live: u64 = app.mail.msgs.iter().map(|m| serde_json::to_vec(m).unwrap().len() as u64 + 1).sum();
            assert!(size < live + COMPACT_BYTES + 25 * 1024, "messages.jsonl is {size} bytes, the live set {live}");
        });
    }

    #[test]
    fn nobody_is_not_an_account_to_admit() {
        with_policy("mail-nobody", || {
            let mut app = app();
            assert_eq!(app.mail_act(MailAction::AdmitAccount("nobody".into()), false), Err("nobody is a system account".into()));
        });
    }

    #[test]
    fn a_refused_or_held_message_is_no_channel_back() {
        with_policy("mail-noback", || {
            let mut app = app();
            app.mail_act(MailAction::SetMode(Mode::Strict), false).unwrap();
            assert!(matches!(send(&mut app, 1, "reader", "hi"), SendReply::Unreachable(_)));
            let refused = app.mail.msgs.last().unwrap().id.clone();
            assert_eq!(reply(&mut app, 3, &refused, "back"), SendReply::Refused(format!("message {refused} is not one this pane can answer")));
            assert!(matches!(app.mail.get(&refused).unwrap().state, State::Refused(_)), "not answered");
            assert_eq!(send(&mut app, 3, "planner", "plain"), SendReply::Unreachable("no route reader → planner, strict mode".into()));

            app.mail_act(MailAction::SetMode(Mode::Open), false).unwrap();
            app.mail_act(MailAction::SetRoute { from: Scope::parse("planner").unwrap(), to: Scope::parse("reader").unwrap(), action: Some(Action::Ask), until: None }, false).unwrap();
            let held = id_of(&send(&mut app, 1, "reader", "wait"));
            for _ in 0..=crate::mail::PER_PAIR_PER_MINUTE {
                assert_eq!(reply(&mut app, 3, &held, "back"), SendReply::Refused(format!("message {held} is not one this pane can answer")), "not counted as a guess");
            }
            assert_eq!(app.mail.get(&held).unwrap().state, State::Held(Hold::Route), "still the owner's to decide");
            let (SendReply::Refused(h), SendReply::Refused(r)) = (reply(&mut app, 3, &held, "back"), reply(&mut app, 3, &refused, "back")) else { panic!() };
            assert_eq!(h.replace(&held, "ID"), r.replace(&refused, "ID"), "held and refused read the same");
            assert_eq!(reply(&mut app, 1, &held, "back"), SendReply::Refused(format!("no message {held} to this pane")), "the sender's own message is not to it");

            // Another account's pane: its message is held until the owner answers, and gives no way back either.
            policy::replace(Policy { accounts: vec![Account { name: "scout".into(), uid: 9999 }], ..(*policy::current()).clone() });
            app.pane_mut(2).unwrap().label = Some("vault".into());
            app.pane_mut(2).unwrap().isolation.files = Files::Account("scout".into());
            let cross = app.mail_send(1, policy::owner_uid(), req("vault", "secret"), crate::hook::now());
            assert!(matches!(&cross, SendReply::Held { why, .. } if why.contains("another account")), "{cross:?}");
            let cross = id_of(&cross);
            let back = app.mail_send(2, 9999, SendReq { reply_to: Some(cross.clone()), ..req("", "pwned") }, crate::hook::now());
            assert_eq!(back, SendReply::Refused(format!("message {cross} is not one this pane can answer")));

            for _ in 0..crate::mail::PER_PAIR_PER_MINUTE {
                let _ = reply(&mut app, 3, "01NOSUCHID0000000000000000", "guess");
            }
            assert!(matches!(send(&mut app, 3, "planner", "a fair one"), SendReply::Sent { .. }), "plain sends go on");
            assert_eq!(reply(&mut app, 3, &held, "x"), SendReply::Refused("too many replies to messages this pane did not get; try later".into()));
        });
    }

    #[test]
    fn one_pane_cannot_fill_the_held_list() {
        with_policy("mail-flood", || {
            let mut app = app();
            for i in 0..110 {
                let _ = send(&mut app, 1, "reader", &format!("flood {i}"));
            }
            let held = |app: &App, name: &str| app.mail.msgs.iter().filter(|m| matches!(m.state, State::Held(_)) && m.from.name() == name).count();
            assert_eq!(held(&app, "planner"), crate::mail::HELD_PER_SENDER);
            assert_eq!(send(&mut app, 1, "reader", "more"), SendReply::Refused("this pane already has 20 held messages; wait for the owner".into()));
            assert!(matches!(send(&mut app, 3, "planner", "still works"), SendReply::Sent { .. }));
            let you = id_of(&send(&mut app, 1, "you", "help"));
            assert_eq!(app.mail.get(&you).unwrap().state, State::Delivered, "writing to you is always allowed");
            for _ in 0..crate::mail::HELD_TOTAL {
                app.mail.add(to(Some(1), "planner", State::Held(Hold::Route)));
            }
            let you = id_of(&send(&mut app, 3, "you", "full list"));
            assert_eq!(app.mail.get(&you).unwrap().state, State::Delivered, "not even a full list shuts that door");
        });
    }

    #[test]
    fn the_reply_right_comes_from_the_message_answered() {
        with_policy("mail-facts", || {
            let mut app = app();
            let q = id_of(&send(&mut app, 1, "reader", "find X"));
            let a = id_of(&reply(&mut app, 3, &q, "found"));
            let facts = app.reply_facts(app.mail.get(&a).unwrap()).unwrap();
            assert_eq!((facts.first_from.as_str(), facts.first_to.as_str()), ("planner", "reader"));
            app.mail.msgs.retain(|m| m.id != q);
            assert!(app.reply_facts(app.mail.get(&a).unwrap()).is_none(), "the answered message gone: no right, never the reply's direction");
            let fresh = id_of(&send(&mut app, 1, "reader", "plain"));
            assert!(app.reply_facts(app.mail.get(&fresh).unwrap()).is_none(), "a message that answers nothing");
        });
    }

    #[test]
    fn each_recipient_of_a_broadcast_may_reply_in_strict_mode() {
        with_policy("mail-allreply", || {
            let mut app = app();
            for i in 0..2 {
                let mut p = crate::app::snapshots::agent_pane(&app, 10 + i, crate::term::AgentState::Idle, 0);
                p.label = Some(format!("w{i}"));
                app.spaces[1].tabs[0].panes.insert(10 + i, p);
            }
            app.mail_act(MailAction::SetMode(Mode::Strict), false).unwrap();
            route(&mut app, "planner", "space:research", Action::Allow);
            route(&mut app, "planner", "w1", Action::Ask);
            let thread = id_of(&send(&mut app, 1, "all", "standup"));
            let copy = |app: &App, pane: PaneId| app.mail.msgs.iter().find(|m| m.thread == thread && m.to.pane() == Some(pane)).unwrap().id.clone();
            for pane in [3, 10] {
                let c = copy(&app, pane);
                let r = reply(&mut app, pane, &c, "here");
                assert!(matches!(r, SendReply::Sent { .. }), "pane {pane}: {r:?}");
            }
            assert_ne!(copy(&app, 10), thread, "w0 got a later copy: the case the thread opener alone missed");
            let held = copy(&app, 11);
            assert_eq!(app.mail.get(&held).unwrap().state, State::Held(Hold::Route));
            assert_eq!(reply(&mut app, 11, &held, "here"), SendReply::Refused(format!("message {held} is not one this pane can answer")), "a held copy opens no way back");
            assert_eq!(send(&mut app, 10, "planner", "plain"), SendReply::Unreachable("no route w0 → planner, strict mode".into()), "only as a reply");
            route(&mut app, "w0", "planner", Action::No);
            let c = copy(&app, 10);
            assert_eq!(reply(&mut app, 10, &c, "again"), SendReply::Refused("route 3: w0 → planner no".into()), "an explicit No still wins");
        });
    }
}
