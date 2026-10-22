//! Menu > Servers…: every server on this Mac, the machines in config.json, and what can be done to each.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::config::RemoteEntry;
use crate::proto::RemoteInstance;
use crate::render::Theme;
use crate::servers::{Server, Stray};

/// Rows shown at most; the rest scroll.
const ROWS: usize = 14;
/// Row ids that are not a server: `s:<id>` is one of this Mac, `p:<pid>` a stray, `m:<name>` a machine and
/// `r:<name>:<id>` a server on it; a machine's name has no `:`.
const MAC: &str = "mac";
const NEW: &str = "new";
const STOPPED: &str = "stopped";
const CONFIG: &str = "config";
/// A row that only says something: waiting, failed, empty.
const NOTE: &str = "note";

/// What a button does to the chosen row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Toggle,
    Open,
    OpenNew,
    Rename,
    Update,
    Stop,
    Remove,
    New,
    RemoveAll,
    EditConfig,
}

/// A machine from config.json, as far as it was listed.
pub enum Machine {
    Closed,
    Asking,
    /// `window`: the client can open a terminal for one of them.
    Listed(Vec<RemoteInstance>, bool),
    Failed(String),
}

pub enum Out {
    None,
    Close,
    /// Ask the client to list this machine over ssh.
    List(RemoteEntry),
    /// A server of this Mac; `true` in a new window.
    Open(Server, bool),
    Rename(Server),
    Update(Server),
    Stop(Server),
    Remove(Server),
    StopStray(Stray),
    New,
    /// Every stopped server, as listed.
    RemoveAll(Vec<Server>),
    /// A server on a machine: its name, the server, and whether the client opens windows.
    OpenRemote(String, RemoteInstance, bool),
    EditConfig,
}

pub struct ServerList {
    /// The id of the server this session shows.
    own: String,
    /// `None` until the servers have answered.
    servers: Option<(Vec<Server>, Vec<Stray>)>,
    /// None for a session that cannot open servers or list machines: one from another Mac, or an older client.
    pub machines: Vec<(RemoteEntry, Machine)>,
    connects: bool,
    switches: bool,
    windows: bool,
    /// From another Mac, as opposed to an older client.
    elsewhere: bool,
    view: crate::panel::View,
}

pub type Drawn = crate::panel::Drawn;

/// The jobs the window hands to its worker, off the server's loop: each may wait seconds on another server.
pub enum Job {
    /// Every server, `own` not asked.
    List(String),
    Stop(String),
    Update(String),
    /// These servers' folders; `own` is refused.
    Remove(Vec<String>, String),
}

pub enum Found {
    Listed(Vec<Server>, Vec<Stray>),
    /// What each job said: a line to show, or its refusal.
    Done(Vec<Result<String, String>>),
}

pub fn work(jobs: Vec<Job>) -> Vec<Found> {
    jobs.into_iter()
        .map(|j| match j {
            Job::List(own) => {
                let (list, strays) = crate::servers::servers(Some(&own));
                Found::Listed(list, strays)
            }
            Job::Stop(id) => Found::Done(vec![crate::servers::stop(&id, &crate::servers::socket_of(&id))]),
            Job::Update(id) => Found::Done(vec![crate::servers::update(&id, &crate::servers::socket_of(&id))]),
            Job::Remove(ids, own) => Found::Done(ids.iter().map(|id| crate::servers::remove(id, Some(&own))).collect()),
        })
        .collect()
}

pub fn failed(_: &Job) -> Option<Found> {
    Some(Found::Done(vec![Err("the servers could not be asked; daemon.log says why".into())]))
}

/// "work (3)" when a name hides the id.
fn shown_as(label: &str, id: &str) -> String {
    if label == id || id.is_empty() { label.to_string() } else { format!("{label} ({id})") }
}

/// Text from a file or another machine, safe to draw.
fn clean(s: &str) -> String {
    crate::render::unspoofed(&crate::log::escape(s))
}

fn folder_names<'a>(folders: impl Iterator<Item = &'a str>) -> String {
    folders.map(|f| clean(f.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("/"))).collect::<Vec<_>>().join(", ")
}

fn when(t: Option<std::time::SystemTime>) -> String {
    t.map_or("never".into(), crate::plugin_list::ago)
}

/// The sandbox in a word; the details give the whole line.
fn sandbox_word(s: &str) -> &str {
    match s {
        _ if s.starts_with("enabled") || s.starts_with("inherited") => "on",
        _ if s.starts_with("disabled") => "off",
        _ if s.starts_with("did NOT") => "failed",
        _ => "unknown",
    }
}

impl ServerList {
    /// `machines` only for a session that `connects`; `elsewhere` says why one does not.
    pub fn new(own: String, machines: Vec<RemoteEntry>, connects: bool, elsewhere: bool) -> Self {
        let mut view = crate::panel::View::new(ROWS, false);
        view.open.insert(MAC.into());
        let machines = if connects { machines.into_iter().map(|m| (m, Machine::Closed)).collect() } else { Vec::new() };
        Self { own, servers: None, machines, connects, elsewhere, view, switches: true, windows: true }
    }

    /// What this session can do at once; without it an open asks or gives the command first, so its label gets "…".
    pub fn caps(mut self, switches: bool, windows: bool) -> Self {
        (self.switches, self.windows) = (switches, windows);
        self
    }

    /// The servers as they answered; the chosen row stays.
    pub fn set(&mut self, mut list: Vec<Server>, strays: Vec<Stray>) {
        // The current one first, then by last use, the never used by number.
        let num = |s: &Server| s.id.parse::<u64>().unwrap_or(u64::MAX);
        list.sort_by(|a, b| (b.id == self.own).cmp(&(a.id == self.own)).then(b.last_used.cmp(&a.last_used)).then(num(a).cmp(&num(b))));
        self.servers = Some((list, strays));
    }

    /// The client's answer about machine `name`; one for a machine not open now is dropped.
    pub fn listed(&mut self, name: &str, m: Machine) {
        let open = self.view.open.contains(&format!("m:{name}"));
        if let Some((_, at)) = self.machines.iter_mut().find(|(r, _)| open && r.name == name) {
            *at = m;
        }
    }

    fn server(&self, id: &str) -> Option<&Server> {
        self.servers.as_ref()?.0.iter().find(|s| s.id == id)
    }

    fn stray(&self, pid: &str) -> Option<&Stray> {
        self.servers.as_ref()?.1.iter().find(|s| s.pid.to_string() == pid)
    }

    fn remote(&self, row: &str) -> Option<(&RemoteEntry, &RemoteInstance, bool)> {
        let (name, id) = row.strip_prefix("r:")?.split_once(':')?;
        self.machines.iter().find_map(|(r, m)| match m {
            Machine::Listed(list, window) if r.name == name => list.iter().find(|i| i.id == id).map(|i| (r, i, *window)),
            _ => None,
        })
    }

    fn stopped(&self) -> Vec<Server> {
        self.servers.as_ref().map(|(l, _)| l.iter().filter(|s| !s.running && !s.id.is_empty()).cloned().collect()).unwrap_or_default()
    }

    /// What the row with this id offers, first the one Enter does: a key, its words, and the act.
    fn actions(&self, id: &str) -> Vec<(char, &'static str, Act)> {
        let open = |o: bool| if o { "collapse" } else { "list servers" };
        if id == MAC {
            return vec![('\n', if self.view.open.contains(MAC) { "collapse" } else { "expand" }, Act::Toggle)];
        }
        match id {
            NEW => return vec![('n', "new server…", Act::New)],
            STOPPED => return vec![('d', "remove all stopped…", Act::RemoveAll)],
            CONFIG => return vec![('e', "edit config.json…", Act::EditConfig)],
            _ => {}
        }
        if id.starts_with("m:") {
            return vec![('\n', open(self.view.open.contains(id)), Act::Toggle)];
        }
        if let Some(pid) = id.strip_prefix("p:") {
            return if self.stray(pid).is_some_and(|s| s.other_home) { Vec::new() } else { vec![('s', "stop…", Act::Stop)] };
        }
        if id.starts_with("r:") {
            return vec![('o', "open", Act::Open)];
        }
        let Some(s) = id.strip_prefix("s:").and_then(|i| self.server(i)) else { return Vec::new() };
        let mut out = Vec::new();
        if s.id != self.own && self.connects {
            let free = s.shown == Some(false) || !s.running;
            out.push(('o', if free && self.switches { "open" } else { "open…" }, Act::Open));
            if s.running {
                out.push(('w', if free && self.windows { "open in a new window" } else { "open in a new window…" }, Act::OpenNew));
            }
        }
        // The legacy server has no folder of its own to name, and none to remove.
        if !s.id.is_empty() {
            out.push(('r', "rename…", Act::Rename));
        }
        if s.running {
            out.push(('u', "update…", Act::Update));
            out.push(('s', "stop…", Act::Stop));
        } else if !s.id.is_empty() {
            out.push(('d', "remove…", Act::Remove));
        }
        out
    }

    fn act(&mut self, id: &str, i: usize) -> Out {
        let Some(&(_, _, a)) = self.actions(id).get(i) else { return Out::None };
        let server = || id.strip_prefix("s:").and_then(|i| self.server(i)).cloned();
        match a {
            Act::Toggle => {
                if !self.view.open.remove(id) {
                    self.view.open.insert(id.into());
                    if let Some((r, m)) = self.machines.iter_mut().find(|(r, _)| id.strip_prefix("m:") == Some(r.name.as_str())) {
                        *m = match r.check() {
                            Ok(()) => Machine::Asking,
                            Err(e) => Machine::Failed(format!("not usable: {e}")),
                        };
                        if matches!(m, Machine::Asking) {
                            return Out::List(r.clone());
                        }
                    }
                }
                Out::None
            }
            Act::Open if id.starts_with("r:") => self.remote(id).map_or(Out::None, |(r, i, w)| Out::OpenRemote(r.name.clone(), i.clone(), w)),
            Act::Open | Act::OpenNew => server().map_or(Out::None, |s| Out::Open(s, a == Act::OpenNew)),
            Act::Rename => server().map_or(Out::None, Out::Rename),
            Act::Update => server().map_or(Out::None, Out::Update),
            Act::Stop if id.starts_with("p:") => self.stray(&id[2..]).cloned().map_or(Out::None, Out::StopStray),
            Act::Stop => server().map_or(Out::None, Out::Stop),
            Act::Remove => server().map_or(Out::None, Out::Remove),
            Act::New => Out::New,
            Act::RemoveAll => Out::RemoveAll(self.stopped()),
            Act::EditConfig => Out::EditConfig,
        }
    }

    fn content(&self, screen: Rect, theme: &Theme, selected: Option<usize>) -> crate::panel::Content {
        let w = crate::panel::inner(screen);
        let ground = Style::default().bg(theme.ground);
        let (text, muted, faint, warn) = (ground.fg(theme.text), ground.fg(theme.muted), ground.fg(theme.faint), ground.fg(theme.warn));
        let wrap = |s: &str| crate::menu::wrap(s, w);
        let note = match (self.connects, self.elsewhere) {
            (true, _) => "A server shows one session at a time. Machines in config.json are listed over ssh when you open them.",
            (false, true) => "This session shows nerd on another Mac: these are that Mac's servers. Open them from a terminal there.",
            (false, false) => "This session's nerd is older than the server: it cannot open servers or list machines. Close it and open nerd again.",
        };
        let mut head: Vec<crate::panel::Spans> = wrap(note).into_iter().map(|l| vec![(0, l, muted)]).collect();
        head.push(Vec::new());

        // The table: servers of this Mac, strays, servers on machines. Each row's cells, styles, and id.
        let mut table: Vec<([String; 6], [Style; 6], String)> = Vec::new();
        let mac_open = self.view.open.contains(MAC);
        if let Some((list, strays)) = self.servers.as_ref().filter(|_| mac_open) {
            for s in list {
                let me = s.id == self.own;
                let state = match (me, s.running, s.shown) {
                    (true, ..) => "current",
                    (_, false, _) => "stopped",
                    (_, true, Some(true)) => "open in another session",
                    _ => "running",
                };
                let sessions = match (me, s.running, s.shown) {
                    (true, ..) | (_, true, Some(true)) => "1",
                    (_, true, None) => "?",
                    _ => "0",
                };
                let sandbox = sandbox_word(&s.sandbox);
                let cells = [format!("  {}", clean(&shown_as(&s.label, &s.id))), state.into(), sessions.into(), when(s.last_used), sandbox.into(), folder_names(s.folders.iter().filter_map(|f| f.to_str()))];
                let lit = if me { ground.fg(theme.accent) } else { text };
                let state_style = if me { lit } else if s.running { text } else { muted };
                table.push((cells, [lit, state_style, muted, muted, if sandbox == "failed" { warn } else { muted }, muted], format!("s:{}", s.id)));
            }
            for s in strays {
                let cells = [format!("  pid {}", s.pid), "stray".into(), String::new(), when(s.started), String::new(), clean(&s.note)];
                table.push((cells, [warn, warn, muted, muted, muted, muted], format!("p:{}", s.pid)));
            }
        }
        for (r, m) in &self.machines {
            if let Machine::Listed(list, _) = m
                && self.view.open.contains(&format!("m:{}", r.name))
            {
                for i in list {
                    let state = match (i.running, i.shown) {
                        (false, _) => "stopped",
                        (true, Some(true)) => "open in another session",
                        _ => "running",
                    };
                    let sessions = match (i.running, i.shown) {
                        (true, Some(true)) => "1",
                        (true, None) => "?",
                        _ => "0",
                    };
                    let used = when(i.last_used.map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s)));
                    let cells = [format!("  {}", clean(&shown_as(&i.label, &i.id))), state.into(), sessions.into(), used, String::new(), folder_names(i.folders.iter().map(String::as_str))];
                    table.push((cells, [text, if i.running { text } else { muted }, muted, muted, muted, muted], format!("r:{}:{}", r.name, i.id)));
                }
            }
        }
        let titles = ["Server", "State", "Sessions", "Last used", "Sandbox", "Folders"];
        let widths: Vec<usize> = (0..6).map(|i| table.iter().map(|c| crate::slots::cells(&c.0[i])).chain([titles[i].len()]).max().unwrap_or(0)).collect();
        let xs: Vec<usize> = widths.iter().scan(0, |x, w| Some(std::mem::replace(x, *x + w + 2))).collect();
        let room = crate::chrome::limits(screen).0.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let fit = |s: &str, n: usize| if crate::slots::cells(s) <= n { s.to_string() } else if n == 0 { String::new() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) };
        let cells = |texts: &[String; 6], style: &[Style; 6]| -> crate::panel::Spans { (0..6).filter(|&i| xs[i] < room).map(|i| (xs[i], fit(&texts[i], widths[i].min(room - xs[i])), style[i])).collect() };
        head.push(cells(&titles.map(String::from), &[muted; 6]));

        let line = |id: &str, label: String, style: Style| crate::panel::Row { id: id.into(), base: text, spans: vec![(0, label, style)], parent: false };
        let arrow = |open: bool| if open { "\u{25be}" } else { "\u{25b8}" };
        let mut rows = vec![crate::panel::Row { parent: true, ..line(MAC, format!("{} this Mac", arrow(mac_open)), text) }];
        let mut table = table.into_iter().peekable();
        let mut take = |rows: &mut Vec<crate::panel::Row>, prefix: &str| {
            while let Some((c, s, id)) = table.next_if(|t| t.2.starts_with(prefix)) {
                rows.push(crate::panel::Row { id, base: text, spans: cells(&c, &s), parent: false });
            }
        };
        if mac_open {
            if self.servers.is_none() {
                rows.push(line(NOTE, "  asking the servers of this Mac…".into(), faint));
            }
            take(&mut rows, "s:");
            if self.connects {
                rows.push(line(NEW, "  New server…".into(), text));
            }
            rows.push(line(STOPPED, "  Remove all stopped…".into(), text));
            take(&mut rows, "p:");
        }
        for (r, m) in &self.machines {
            let id = format!("m:{}", r.name);
            let open = self.view.open.contains(&id);
            let host = match r.check() {
                Ok(()) => clean(&r.host),
                Err(e) => format!("not usable: {e}"),
            };
            rows.push(crate::panel::Row { parent: true, spans: vec![(0, format!("{} {}", arrow(open), clean(&r.name)), text), (xs[1], fit(&host, room.saturating_sub(xs[1])), muted)], ..line(&id, String::new(), text) });
            if !open {
                continue;
            }
            match m {
                Machine::Closed => {}
                Machine::Asking => rows.push(line(NOTE, format!("  asking {} for its servers…", clean(&r.name)), faint)),
                Machine::Failed(e) => rows.push(line(NOTE, format!("  {}", fit(&clean(e), room.saturating_sub(2))), warn)),
                Machine::Listed(list, _) if list.is_empty() => rows.push(line(NOTE, "  no servers; start one there with nerd".into(), faint)),
                Machine::Listed(..) => take(&mut rows, &format!("r:{}:", r.name)),
            }
        }
        rows.push(line(CONFIG, "Edit config.json…".into(), text));

        let details = |id: &str| -> Vec<String> { self.details(id).iter().flat_map(|l| wrap(l)).collect() };
        // Sized for every row's details and buttons at once, so choosing another row neither moves nor resizes the window.
        let all: Vec<Vec<String>> = rows.iter().map(|r| details(&r.id)).collect();
        let body_h = all.iter().map(Vec::len).max().unwrap_or(0);
        let labels = |id: &str| -> Vec<String> { self.actions(id).iter().enumerate().map(|(i, (k, l, _))| if i == 0 { format!(" [Enter] {l} ") } else { format!(" [{k}] {l} ") }).collect() };
        let bottoms: Vec<Vec<String>> = rows.iter().map(|r| labels(&r.id)).collect();
        let widest = bottoms.iter().map(|b| crate::panel::bottom_w(b)).max().unwrap_or(0).min(room);
        let spans_w = |l: &crate::panel::Spans| l.iter().map(|(x, t, _)| x + crate::slots::cells(t)).max().unwrap_or(0);
        let inner = head.iter().chain(rows.iter().map(|r| &r.spans)).map(spans_w).chain(all.iter().flatten().map(|l| crate::slots::cells(l))).chain([widest]).max().unwrap_or(0);
        let inner = inner.max(crate::chrome::MIN_W.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize).min(room);
        let button_lines = bottoms.iter().map(|b| crate::panel::button_lines(b, inner).len()).max().unwrap_or(1);
        let body = selected.and_then(|i| all.get(i)).cloned().unwrap_or_default().into_iter().map(|l| vec![(0, l, text)]).collect();
        let buttons = selected.and_then(|i| bottoms.get(i)).cloned().unwrap_or_default();
        crate::panel::Content { head, rows, body, body_h, full: (widest, 0), buttons, button_lines }
    }

    /// What the window says about the row with this id, unwrapped.
    fn details(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        match id {
            MAC => out.push("Every server of this Mac, the one this session shows first.".into()),
            NEW => out.push("Makes a new server and moves this session to it; this server keeps running.".into()),
            STOPPED => {
                let n = self.stopped().len();
                out.push(format!("Removes every stopped server ({n}): {} go; {}.", crate::servers::GOES, crate::servers::STAYS));
            }
            CONFIG if self.machines.is_empty() && self.connects => out.push(NO_REMOTES.into()),
            CONFIG => out.push("Machines are added to config.json as \"remotes\", with ssh keys set up for each host.".into()),
            _ => {}
        }
        if let Some(name) = id.strip_prefix("m:")
            && let Some((r, _)) = self.machines.iter().find(|(r, _)| r.name == name)
        {
            out.push(format!("{} at {}: its servers are listed over ssh when you open it, and one opens in a terminal window of its own.", clean(&r.name), clean(&r.host)));
        }
        if let Some((r, i, _)) = self.remote(id) {
            let folders: Vec<String> = i.folders.iter().map(|f| clean(f)).collect();
            out.push(if folders.is_empty() { "No spaces.".into() } else { format!("Folders: {}.", folders.join(", ")) });
            if !i.running {
                out.push(format!("Stopped: start it on {} with nerd --resume {}.", clean(&r.name), clean(&i.id)));
            }
        }
        if let Some(s) = id.strip_prefix("p:").and_then(|p| self.stray(p)) {
            let exe = clean(&s.exe.display().to_string());
            out.push(format!("A nerd server no folder here leads to: {}. Started {}, from {exe}.", clean(&s.note), when(s.started)));
            out.push("Stop… sends it SIGTERM once it is checked again to be a nerd server of yours.".into());
        }
        if let Some(s) = id.strip_prefix("s:").and_then(|i| self.server(i)) {
            let folders: Vec<String> = s.folders.iter().map(|f| clean(&f.display().to_string())).collect();
            out.push(if folders.is_empty() { "No spaces.".into() } else { format!("Folders: {}.", folders.join(", ")) });
            if !s.sandbox.is_empty() {
                out.push(format!("Sandbox: {}.", clean(&s.sandbox)));
            }
            if let Some(v) = &s.version {
                out.push(format!("Build: {}.", clean(v)));
            }
            out.push(match (s.id == self.own, s.running, s.shown) {
                (true, ..) => "This session shows it.".into(),
                (_, true, Some(true)) => "Open in another session: opening it here asks before taking it over.".into(),
                (_, true, None) => "It did not say whether a session shows it, so opening it asks first.".into(),
                (_, true, _) => "Running; no session shows it.".into(),
                (_, false, _) if s.reserved_only => "Its server never started; Remove… takes the folder without asking.".into(),
                (_, false, _) if s.untouched => "Stopped; nothing was ever done in it.".into(),
                (_, false, _) => "Stopped: Open starts it again with its spaces.".into(),
            });
            if s.id.is_empty() {
                out.push("The legacy server keeps its state in nerd's own folder; it is never named or removed.".into());
            }
        }
        out
    }

    #[cfg(test)]
    pub fn choose(&mut self, id: &str) {
        self.view.select_id(id);
    }

    fn view_id(&self, c: &crate::panel::Content) -> Option<String> {
        c.rows.get(self.view.selected(c)).map(|r| r.id.clone())
    }

    fn laid_out(&self, screen: Rect, theme: &Theme) -> crate::panel::Content {
        let c = self.content(screen, theme, None);
        self.content(screen, theme, Some(self.view.selected(&c)))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn {
        self.view.draw(buf, screen, theme, "Servers", &self.laid_out(screen, theme))
    }

    /// Enter does the chosen row's first act; a key its own; the rest moves the choice.
    pub fn key(&mut self, k: crossterm::event::KeyEvent, screen: Rect) -> Out {
        let c = self.laid_out(screen, &Theme::dark());
        let Some(id) = self.view_id(&c) else {
            return if self.view.key(k, screen, &c) == crate::panel::Hit::Close { Out::Close } else { Out::None };
        };
        if let crossterm::event::KeyCode::Char(ch) = crate::keys::normalize(k).code
            && let Some(i) = self.actions(&id).iter().position(|(key, _, _)| *key == ch)
        {
            return self.act(&id, i);
        }
        match self.view.key(k, screen, &c) {
            crate::panel::Hit::Close => Out::Close,
            crate::panel::Hit::Enter(_) => self.act(&id, 0),
            _ => Out::None,
        }
    }

    /// A click on a section opens or closes it; on a server it chooses it; a button acts on the chosen row.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, drawn: &Drawn) -> Out {
        let c = self.laid_out(screen, &Theme::dark());
        let id = self.view_id(&c);
        match self.view.mouse(ev, screen, &c, drawn) {
            crate::panel::Hit::Close => Out::Close,
            crate::panel::Hit::Button(i) => id.map_or(Out::None, |id| self.act(&id, i)),
            crate::panel::Hit::Row(i) if c.rows.get(i).is_some_and(|r| r.parent) => {
                let id = c.rows[i].id.clone();
                self.act(&id, 0)
            }
            _ => Out::None,
        }
    }
}

const NO_REMOTES: &str = "No machines yet. Add them to config.json (nerd --config prints its path) as \
    \"remotes\": [{\"name\": \"studio\", \"host\": \"me@studio.local\"}], with ssh keys set up for the host. \
    Update running nerd servers first: an older one drops the key when it saves settings. \
    In a remote session, links, pbcopy and opening files act on that Mac.";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    pub(crate) fn server(id: &str, label: &str, running: bool, shown: Option<bool>) -> Server {
        Server {
            id: id.into(),
            label: label.into(),
            running,
            shown,
            sessions: u8::from(shown == Some(true)),
            folders: vec![PathBuf::from(format!("/Users/me/src/proj{id}"))],
            last_used: Some(SystemTime::now() - Duration::from_secs(3600 * id.parse::<u64>().unwrap_or(0))),
            sandbox: if running { "enabled, network denied to the daemon".into() } else { String::new() },
            version: running.then(|| "nerd 0.15.0".into()),
            untouched: false,
            reserved_only: false,
        }
    }

    /// The current 1, 2 shown elsewhere, 3 running and free, 4 stopped, a stray, and a collapsed machine.
    pub(crate) fn fixture(connects: bool) -> ServerList {
        let studio = RemoteEntry { name: "studio".into(), host: "me@studio.local".into() };
        let mut w = ServerList::new("1".into(), vec![studio], connects, !connects);
        let stray = Stray { pid: 4242, started: Some(SystemTime::now() - Duration::from_secs(7200)), exe: "/Users/me/.local/bin/nerd".into(), note: "no server folder leads to it".into(), other_home: false };
        w.set(vec![server("4", "old", false, Some(false)), server("2", "work", true, Some(true)), server("1", "main", true, None), server("3", "3", true, Some(false))], vec![stray]);
        w
    }

    /// The ids of the rows shown, in order.
    pub(crate) fn ids(w: &ServerList) -> Vec<String> {
        w.content(Rect::new(0, 0, 200, 60), &Theme::dark(), None).rows.into_iter().map(|r| r.id).collect()
    }

    fn offered(w: &ServerList, id: &str) -> Vec<Act> {
        w.actions(id).into_iter().map(|a| a.2).collect()
    }

    #[test]
    fn each_row_offers_what_its_state_allows() {
        use Act::*;
        let w = fixture(true);
        assert_eq!(ids(&w), ["mac", "s:1", "s:2", "s:3", "s:4", "new", "stopped", "p:4242", "m:studio", "config"], "the current one first, then by last use");
        assert_eq!(offered(&w, "s:1"), [Rename, Update, Stop], "current");
        assert_eq!(offered(&w, "s:2"), [Open, OpenNew, Rename, Update, Stop], "open in another session");
        assert_eq!(offered(&w, "s:3"), [Open, OpenNew, Rename, Update, Stop], "running");
        assert_eq!(offered(&w, "s:4"), [Open, Rename, Remove], "stopped");
        assert_eq!(offered(&w, "p:4242"), [Stop], "stray");
        let mut other = fixture(true);
        let (list, mut strays) = other.servers.clone().unwrap();
        strays[0].other_home = true;
        other.set(list, strays);
        assert!(offered(&other, "p:4242").is_empty(), "another home's server is never stopped from here");
        assert_eq!(offered(&w, "new"), [New]);
        assert_eq!(offered(&w, "stopped"), [RemoveAll]);
        assert_eq!(offered(&w, "m:studio"), [Toggle]);
        assert_eq!(offered(&w, "config"), [EditConfig]);
        let mut legacy = fixture(true);
        legacy.set(vec![server("", "legacy", true, Some(false))], Vec::new());
        assert_eq!(offered(&legacy, "s:"), [Open, OpenNew, Update, Stop], "the legacy server is never named");
    }

    #[test]
    fn a_session_from_another_mac_sees_this_macs_servers_and_opens_none() {
        let w = fixture(false);
        assert_eq!(ids(&w), ["mac", "s:1", "s:2", "s:3", "s:4", "stopped", "p:4242", "config"], "no machines, no New server…");
        assert!(w.machines.is_empty());
        for id in ["s:2", "s:3", "s:4"] {
            assert!(!offered(&w, id).iter().any(|a| matches!(a, Act::Open | Act::OpenNew)), "{id}");
        }
    }

    #[test]
    fn a_machine_is_listed_when_opened_and_its_servers_open_through_the_client() {
        let mut w = fixture(true);
        let Out::List(r) = w.act("m:studio", 0) else { panic!("opening asks the client") };
        assert_eq!(r.name, "studio");
        assert!(ids(&w).contains(&NOTE.to_string()), "asking… while it lists");
        let one = RemoteInstance { id: "7".into(), label: "lab".into(), running: true, spaces: 1, ..Default::default() };
        w.listed("studio", Machine::Listed(vec![one], true));
        assert!(ids(&w).contains(&"r:studio:7".to_string()));
        let Out::OpenRemote(name, i, true) = w.act("r:studio:7", 0) else { panic!("open") };
        assert_eq!((name.as_str(), i.id.as_str()), ("studio", "7"));
        assert!(matches!(w.act("m:studio", 0), Out::None), "closing asks nothing");
        assert!(!ids(&w).contains(&"r:studio:7".to_string()));
        w.listed("studio", Machine::Failed("late".into()));
        assert!(matches!(w.machines[0].1, Machine::Listed(..)), "an answer for a closed machine changes nothing");
        let mut bad = ServerList::new("1".into(), vec![RemoteEntry { name: "x".into(), host: "-oProxyCommand=x".into() }], true, false);
        assert!(matches!(bad.act("m:x", 0), Out::None), "a bad entry never reaches the client");
        assert!(matches!(&bad.machines[0].1, Machine::Failed(e) if e.starts_with("not usable")));
    }

    #[test]
    fn remove_all_takes_the_stopped_ones_only() {
        let mut w = fixture(true);
        let Out::RemoveAll(list) = w.act(STOPPED, 0) else { panic!() };
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["4"]);
    }
}
