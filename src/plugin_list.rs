//! Menu > Plugins…: the installed plugins and the catalog's, what each can do, and where to add one.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::render::Theme;

/// Rows shown at most; the rest scroll.
const ROWS: usize = 12;
/// Rows that are not a plugin; a plugin's name never starts with a control character.
pub const ADD: &str = "\u{0}add";
pub const REFRESH: &str = "\u{0}refresh";
/// A catalog plugin that is not installed: this, then its name.
const OFFERED: &str = "\u{0}catalog:";

/// One installed plugin, as read from its folder, the lock, the off file, the catalog and the running server.
pub struct Entry {
    pub name: String,
    pub title: String,
    pub version: Option<String>,
    /// Where it came from; `None` when it was put in the folder by hand.
    pub pin: Option<crate::install::Pin>,
    pub risk: u8,
    pub abilities: Vec<String>,
    /// One word or two for the table.
    pub state: String,
    /// Why it is off, when it is.
    pub off: Option<String>,
    /// Why it does not load, when it does not.
    pub problems: Vec<String>,
    /// `reviewed`, `checked`, `not in catalog` or `by hand`.
    pub mark: String,
}

/// A folder put in by hand that the server has not loaded: it starts only after Reload plugins and agents.
pub const WAITS: &str = "waits for Reload";

/// Every folder under the state root's `plugins/`, by name.
pub fn entries(catalog: &crate::catalog::Catalog, running: impl Fn(&str) -> &'static str) -> Vec<Entry> {
    let root = crate::paths::state_root();
    let pins = crate::install::read_lock().unwrap_or_default();
    let off = crate::install::read_off();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join("plugins")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("plugin.json").is_file()).collect();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let pin = pins.iter().find(|p| p.name == name).cloned();
            let c = if pin.is_some() { crate::agents::check_installed(dir) } else { crate::agents::check_folder(dir, false) };
            let off = match &off {
                Ok(o) => o.get(&name).cloned(),
                Err(e) => Some(e.clone()),
            };
            let state = match (&off, c.problems.is_empty()) {
                (Some(w), _) if w.starts_with("revoked") => "revoked".into(),
                (Some(_), _) => "off".into(),
                (None, false) => "not loaded".into(),
                // An installed plugin loads on its own; a dropped folder only on the user's Reload.
                (None, true) if pin.is_none() && crate::agents::get(&name).is_none() => WAITS.into(),
                (None, true) => running(&name).into(),
            };
            let warned = pin.as_ref().and_then(|p| catalog.revoked(&p.name, &p.commit)).is_some_and(|r| r.level == "soft");
            let mark = if warned { "warned".into() } else { catalog.mark(pin.as_ref()).to_string() };
            Entry { title: c.title.clone(), version: c.version.clone(), mark, pin, risk: c.risk, abilities: c.abilities.clone(), state, off, problems: c.problems.clone(), name }
        })
        .collect()
}

/// `https://github.com/me/x.git` as `github.com/me/x`: the words a person recognises.
pub fn short(source: &str) -> String {
    let s = source.strip_prefix("https://").unwrap_or(source);
    crate::render::unspoofed(s.strip_suffix(".git").unwrap_or(s))
}

/// How long ago, in the largest whole unit.
pub(crate) fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match s {
        0..120 => "just now".into(),
        120..7200 => format!("{} minutes ago", s / 60),
        7200..172_800 => format!("{} hours ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// What a button does to the chosen row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Add,
    Refresh,
    Install,
    Off,
    On,
    Update,
    Log,
    Remove,
    Cancel,
    Reload,
}

/// A git fetch the window started, run hidden by the broker outside the sandbox; one at a time.
#[derive(Clone)]
pub struct Job {
    /// Add, Refresh, Install or Update.
    pub act: Act,
    /// The plugin's name; the address for Add, empty for Refresh.
    pub name: String,
    pub exec: usize,
    /// `None` while git runs; then what it said, or why it failed.
    pub done: Option<Result<String, String>>,
}

impl Job {
    /// The row it is shown on.
    pub fn row(&self) -> String {
        match self.act {
            Act::Add => ADD.into(),
            Act::Refresh => REFRESH.into(),
            Act::Install => format!("{OFFERED}{}", self.name),
            _ => self.name.clone(),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self.done, Some(Err(_)))
    }

    /// Its row's State while it runs or after it failed.
    fn state(&self) -> Option<&'static str> {
        match (&self.done, self.act) {
            (None, Act::Install) => Some("installing…"),
            (None, _) => Some("updating…"),
            (Some(Err(_)), Act::Install) => Some("install failed"),
            (Some(Err(_)), _) => Some("update failed"),
            _ => None,
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let (doing, verb) = match self.act {
            Act::Add => (format!("Adding {}", self.name), "Add"),
            Act::Install => (format!("Installing {}", self.name), "Install"),
            Act::Update => (format!("Updating {}", self.name), "Update"),
            _ => ("Refreshing the catalog".into(), "Refresh"),
        };
        match &self.done {
            None if self.act == Act::Refresh => vec![format!("{doing}: fetching with git…")],
            None => vec![format!("{doing}: fetching with git…"), "Nothing is installed until you say yes to what it fetched.".into()],
            Some(Ok(said)) => vec![said.clone()],
            Some(Err(why)) => vec![format!("{verb} failed: {why}"), "Show log has git's full output.".into()],
        }
    }
}

/// git's last words as a person reads them. GitHub asks for a login where no public repository is.
pub fn failure(why: &str) -> String {
    let has = |ws: &[&str]| ws.iter().any(|w| why.contains(w));
    if has(&["terminal prompts disabled", "Authentication failed", "could not read Username", "could not read Password", "Repository not found"]) {
        "GitHub has no public repository at that address; nerd installs public repositories only.".into()
    } else if has(&["Could not resolve host", "Network is unreachable", "Failed to connect", "timed out", "Connection refused"]) {
        "git could not reach GitHub; is the network up?".into()
    } else {
        why.into()
    }
}

pub enum Out {
    None,
    Close,
    /// A button, a key or Enter on the chosen row: its plugin's name (empty for Add and Refresh), and what to do.
    Act(String, Act),
    /// A right click on a plugin's row: its menu, installed or offered, at this cell.
    Menu(String, bool, (u16, u16)),
}

pub struct PluginList {
    list: Vec<Entry>,
    /// Catalog plugins not installed here.
    offered: Vec<crate::catalog::Entry>,
    fetched: Option<std::time::SystemTime>,
    /// GitHub stars by plugin name, only for the catalog's own releases, and when a count last changed.
    stars: std::collections::BTreeMap<String, u32>,
    changed: String,
    view: crate::panel::View,
    pub job: Option<Job>,
}

pub type Drawn = crate::panel::Drawn;

impl PluginList {
    pub fn new(list: Vec<Entry>, catalog: crate::catalog::Catalog) -> Self {
        let (mut stars, mut changed) = (std::collections::BTreeMap::new(), String::new());
        // An installed plugin counts only by its own address; a catalog entry only when no plugin of its name is installed.
        let installed = list.iter().filter_map(|e| e.pin.as_ref().map(|p| (&e.name, &p.source)));
        let offered = catalog.entries.iter().filter(|e| !list.iter().any(|i| i.name == e.name)).map(|e| (&e.name, &e.url));
        for (name, url) in installed.chain(offered) {
            if let Some((n, date)) = catalog.stars(name, url) {
                stars.insert(name.clone(), n);
                changed = date.to_string();
            }
        }
        let offered = catalog.entries.into_iter().filter(|e| !list.iter().any(|i| i.name == e.name)).collect();
        Self { list, offered, fetched: catalog.fetched, stars, changed, view: crate::panel::View::new(ROWS, false), job: None }
    }

    /// The window read afresh, with the same row chosen.
    pub fn renew(&mut self, mut fresh: Self) {
        std::mem::swap(&mut fresh.view, &mut self.view);
        *self = fresh;
    }

    /// The job's row is chosen while it is shown.
    pub fn with_job(mut self, job: Option<Job>) -> Self {
        if let Some(j) = &job {
            self.view.select_id(&j.row());
        }
        self.job = job;
        self
    }

    fn content(&self, screen: Rect, theme: &Theme, selected: Option<usize>) -> crate::panel::Content {
        let w = crate::panel::inner(screen);
        let ground = Style::default().bg(theme.ground);
        let text = ground.fg(theme.text);
        let muted = ground.fg(theme.muted);
        let n = self.list.len();
        let installed = match n {
            0 => "No plugin is installed".to_string(),
            1 => "One plugin is installed".into(),
            _ => format!("{n} plugins are installed"),
        };
        let catalog = match (self.fetched, self.offered.len()) {
            (None, _) => "the catalog was never fetched".to_string(),
            (Some(t), m) => format!("{m} more in the catalog, fetched {}", ago(t)),
        };
        let head = format!("{installed}; {catalog}. git reaches the network only when you add, update or refresh.");
        let mut head: Vec<crate::panel::Spans> = crate::menu::wrap(&head, w).into_iter().map(|l| vec![(0, l, muted)]).collect();
        head.push(Vec::new());

        let stars = |name: &str| self.stars.get(name).map(u32::to_string).unwrap_or_default();
        let mut cols: Vec<[String; 6]> = self
            .list
            .iter()
            .map(|e| [e.name.clone(), e.version.clone().unwrap_or_else(|| "no version".into()), crate::install::RISK[e.risk.min(3) as usize].to_string(), e.mark.clone(), stars(&e.name), e.state.clone()])
            .collect();
        cols.extend(self.offered.iter().map(|e| [e.name.clone(), e.version.clone(), crate::install::RISK[e.risk.min(3) as usize].to_string(), e.review.clone(), stars(&e.name), "not installed".into()]));
        let job = self.job.as_ref();
        for (i, c) in cols.iter_mut().enumerate() {
            let id = if i < n { c[0].clone() } else { format!("{OFFERED}{}", c[0]) };
            if let Some(s) = job.filter(|j| j.row() == id).and_then(Job::state) {
                c[5] = s.into();
            }
        }
        let risks: Vec<u8> = self.list.iter().map(|e| e.risk).chain(self.offered.iter().map(|e| e.risk)).collect();
        // No Stars column until the catalog has counted some.
        let titles = ["Plugin", "Version", "Risk", "Mark", if self.stars.is_empty() { "" } else { "Stars" }, "State"];
        let widths: Vec<usize> = (0..6).map(|i| cols.iter().map(|c| c[i].chars().count()).chain([titles[i].chars().count()]).max().unwrap_or(0)).collect();
        let xs: Vec<usize> = widths
            .iter()
            .scan(0, |x, w| {
                let at = *x;
                *x += if *w == 0 { 0 } else { w + 2 };
                Some(at)
            })
            .collect();
        // Rows may be wider than the wrapped text: the window grows to them, up to the screen.
        let room = crate::chrome::limits(screen).0.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let cells = |texts: &[String; 6], style: [Style; 6]| -> crate::panel::Spans { (0..6).map(|i| (xs[i], fit(&texts[i], widths[i].min(room.saturating_sub(xs[i]))), style[i])).collect() };
        if !cols.is_empty() {
            head.push(cells(&titles.map(String::from), [muted; 6]));
        }

        let mut rows = vec![
            crate::panel::Row { id: ADD.into(), base: text, spans: vec![(0, "Add from GitHub…".into(), text)], parent: false },
            crate::panel::Row { id: REFRESH.into(), base: text, spans: vec![(0, "Refresh catalog".into(), text)], parent: false },
        ];
        for (i, c) in cols.iter().enumerate() {
            let installed = i < n;
            let warn = self.list.get(i).is_some_and(|e| e.off.is_some() || !e.problems.is_empty());
            let state = if warn { ground.fg(theme.warn) } else if installed { text } else { muted };
            let mark = if c[3] == "not in catalog" || c[3] == "warned" { ground.fg(theme.warn) } else { muted };
            let risk = if risks[i] >= 2 { ground.fg(theme.warn) } else { text };
            let id = if installed { c[0].clone() } else { format!("{OFFERED}{}", c[0]) };
            rows.push(crate::panel::Row { id, base: text, spans: cells(c, [text, muted, risk, mark, muted, state]), parent: false });
        }

        let details = |i: usize| -> Vec<String> {
            let wrap = |s: &str| crate::menu::wrap(s, w);
            let starred = |name: &str| self.stars.get(name).map(|n| format!("{n} {} on GitHub; the count last changed on {}.", if *n == 1 { "star" } else { "stars" }, self.changed));
            match i {
                0 => wrap("Fetches a public GitHub repository with git, then shows what it can do; nothing is installed until you say so."),
                1 => wrap(&format!("Fetches the catalog from {} with git. A release the catalog revokes is turned off.", short(crate::catalog::URL))),
                _ if i - 2 < n => {
                    let e = &self.list[i - 2];
                    let mut out = wrap(&format!("{}. Risk: {}.", e.title, crate::install::RISK[e.risk.min(3) as usize]));
                    out.extend(e.abilities.iter().flat_map(|a| wrap(&format!("  {a}"))));
                    match &e.pin {
                        Some(p) => {
                            let release = p.tag.as_ref().map(|t| format!("release {}, ", crate::render::unspoofed(t))).unwrap_or_default();
                            out.extend(wrap(&format!("From {}, {release}commit {}; {}.", crate::render::unspoofed(&p.source), &p.commit[..p.commit.len().min(12)], e.mark)));
                        }
                        None => out.extend(wrap("Put in the plugins folder by hand; it is never updated.")),
                    }
                    if e.state == WAITS {
                        out.extend(wrap("Not loaded yet: a folder put there by hand starts only after Reload plugins and agents, so nothing can slip a plugin in unseen. Enter reloads them now."));
                    }
                    out.extend(starred(&e.name).iter().flat_map(|s| wrap(s)));
                    if e.state == "needs a server restart" {
                        out.extend(wrap("It starts once the server restarts: Stop this server… in the main menu, then nerd. The server kept the part that starts plugins from a build before them."));
                    }
                    // The State column already says off; only a revocation has more to say.
                    if let Some(off) = e.off.as_deref().filter(|o| o.starts_with("revoked")) {
                        out.extend(wrap(&format!("Off: {}", crate::render::unspoofed(off))));
                    }
                    out.extend(e.problems.iter().flat_map(|p| wrap(&format!("Not loaded: {p}"))));
                    out
                }
                _ => match self.offered.get(i - 2 - n) {
                    Some(e) => {
                        let mut out = wrap(&format!("{}. {}", crate::render::unspoofed(&e.title), crate::render::unspoofed(&e.summary)));
                        out.extend(wrap(&format!("Risk: {}; {} by the catalog.", crate::install::RISK[e.risk.min(3) as usize], e.review)));
                        out.extend(wrap(&format!("From {}, release {}.", crate::render::unspoofed(&e.url), e.tag)));
                        out.extend(starred(&e.name).iter().flat_map(|s| wrap(s)));
                        out
                    }
                    None => Vec::new(),
                },
            }
        };
        // Sized for every row's details at once, so choosing another row neither moves nor resizes the window.
        let all: Vec<Vec<String>> = (0..rows.len()).map(details).collect();
        // The job's words go first on its row, in the room the tallest details already take.
        let said: Vec<String> = job.map(|j| j.lines().iter().flat_map(|l| crate::menu::wrap(l, w)).collect()).unwrap_or_default();
        let body_h = all.iter().map(Vec::len).max().unwrap_or(0).max(said.len());
        // "turn on" is a cell shorter than "turn off": counted as the longer, the window stays put when it flips.
        let labels = |acts: Vec<(char, String, Act)>| -> Vec<String> { acts.into_iter().enumerate().map(|(i, (k, label, _))| if i == 0 { format!(" [Enter] {label} ") } else { format!(" [{k}] {label} ") }).collect() };
        // Counted with and without a fetch's buttons, so starting one does not resize the window.
        let own = rows.iter().map(|r| crate::panel::bottom_w(&labels(self.own_actions(&r.id))) + usize::from(self.own_actions(&r.id).first().is_some_and(|a| a.2 == Act::On)));
        let now = rows.iter().map(|r| crate::panel::bottom_w(&labels(self.actions(&r.id))));
        let widest = all.iter().flatten().map(|l| crate::slots::cells(l)).chain(own).chain(now).max().unwrap_or(0);
        let body = selected
            .and_then(|i| {
                let mine = job.is_some_and(|j| rows.get(i).is_some_and(|r| r.id == j.row()));
                all.get(i).map(|d| match mine {
                    // Its details follow only when they fit whole.
                    true if said.len() + d.len() <= body_h => said.iter().chain(d).cloned().collect(),
                    true => said.clone(),
                    false => d.clone(),
                })
            })
            .unwrap_or_default()
            .into_iter()
            .take(body_h)
            .map(|l| vec![(0, l, text)])
            .collect();
        let full = (widest, rows.len());
        let buttons = selected.and_then(|i| rows.get(i)).map(|r| labels(self.actions(&r.id))).unwrap_or_default();
        crate::panel::Content { head, rows, body, body_h, full, buttons, ..Default::default() }
    }

    /// What the row with this id offers, first the one Enter does: a key, its words, and the act.
    fn actions(&self, id: &str) -> Vec<(char, String, Act)> {
        let job = self.job.as_ref().filter(|j| j.row() == id);
        if job.is_some_and(|j| j.done.is_none()) {
            return vec![('\n', "cancel".into(), Act::Cancel)];
        }
        let mut out = self.own_actions(id);
        if job.is_some_and(Job::failed) && !out.iter().any(|a| a.2 == Act::Log) {
            out.push(('l', "show log".into(), Act::Log));
        }
        out
    }

    fn own_actions(&self, id: &str) -> Vec<(char, String, Act)> {
        match id {
            ADD => vec![('\n', "add from GitHub…".into(), Act::Add)],
            REFRESH => vec![('\n', "refresh catalog".into(), Act::Refresh)],
            _ => match id.strip_prefix(OFFERED) {
                Some(_) => vec![('\n', "install…".into(), Act::Install)],
                None => {
                    let Some(e) = self.list.iter().find(|e| e.name == id) else { return Vec::new() };
                    let mut out = Vec::new();
                    if e.state == WAITS {
                        out.push(('\n', "reload plugins".into(), Act::Reload));
                    }
                    match e.off.as_deref() {
                        // Only an update to the catalog's release lifts a revocation.
                        Some(w) if w.starts_with("revoked") => {}
                        Some(_) => out.push(('o', "turn on".into(), Act::On)),
                        None => out.push(('o', "turn off".into(), Act::Off)),
                    }
                    if e.pin.is_some() {
                        out.push(('u', "update…".into(), Act::Update));
                    }
                    out.push(('l', "show log".into(), Act::Log));
                    out.push(('d', "remove…".into(), Act::Remove));
                    out
                }
            },
        }
    }

    /// The plugin's name a row stands for; empty for Add and Refresh.
    fn name_of(id: &str) -> String {
        match id {
            ADD | REFRESH => String::new(),
            _ => id.strip_prefix(OFFERED).unwrap_or(id).to_string(),
        }
    }

    /// Chooses a plugin's row, as the window opens again after acting on it.
    pub fn choose(&mut self, name: &str) {
        let id = if self.list.iter().any(|e| e.name == name) { name.to_string() } else { format!("{OFFERED}{name}") };
        self.view.select_id(&id);
    }

    fn act(&self, c: &crate::panel::Content, i: usize) -> Out {
        let Some(id) = self.view_id(c) else { return Out::None };
        match self.actions(&id).get(i) {
            Some((_, _, a)) => Out::Act(Self::name_of(&id), *a),
            None => Out::None,
        }
    }

    fn view_id(&self, c: &crate::panel::Content) -> Option<String> {
        c.rows.get(self.view.selected(c)).map(|r| r.id.clone())
    }

    fn laid_out(&self, screen: Rect, theme: &Theme) -> crate::panel::Content {
        let c = self.content(screen, theme, None);
        self.content(screen, theme, Some(self.view.selected(&c)))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn {
        self.view.draw(buf, screen, theme, "Plugins", &self.laid_out(screen, theme))
    }

    /// Enter does the chosen row's first act; a key its own; the rest moves the choice.
    pub fn key(&mut self, k: crossterm::event::KeyEvent, screen: Rect, _drawn: &Drawn) -> Out {
        let c = self.laid_out(screen, &Theme::dark());
        if let crossterm::event::KeyCode::Char(ch) = crate::keys::normalize(k).code
            && let Some(id) = self.view_id(&c)
            && let Some(i) = self.actions(&id).iter().position(|(key, _, _)| *key == ch)
        {
            return self.act(&c, i);
        }
        match self.view.key(k, screen, &c) {
            crate::panel::Hit::Close => Out::Close,
            crate::panel::Hit::Enter(_) => self.act(&c, 0),
            _ => Out::None,
        }
    }

    /// A click on a row chooses it and the window stays; a button acts on it; a right click opens its menu there.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, drawn: &Drawn) -> Out {
        use crossterm::event::{MouseButton, MouseEventKind};
        let c = self.laid_out(screen, &Theme::dark());
        let at = (ev.column, ev.row).into();
        if ev.kind == MouseEventKind::Down(MouseButton::Right)
            && let Some(&(_, i)) = drawn.rows.iter().find(|(r, _)| r.contains(at))
            && let Some(id) = c.rows.get(i).map(|r| r.id.clone())
            && id != ADD
            && id != REFRESH
        {
            self.view.select_id(&id);
            return Out::Menu(Self::name_of(&id), !id.starts_with(OFFERED), (ev.column, ev.row + 1));
        }
        match self.view.mouse(ev, screen, &c, drawn) {
            crate::panel::Hit::Close => Out::Close,
            crate::panel::Hit::Button(i) => self.act(&c, i),
            _ => Out::None,
        }
    }
}

fn fit(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    match n {
        0 => String::new(),
        _ => format!("{}…", s.chars().take(n - 1).collect::<String>()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_another_row_neither_moves_nor_resizes_the_window() {
        let entry = |name: &str, abilities: Vec<&str>| Entry {
            name: name.into(),
            title: name.into(),
            version: Some("1.0.0".into()),
            // Pinned: the widest bottom line, update included.
            pin: Some(crate::install::Pin { name: name.into(), source: "https://x/p.git".into(), tag: None, commit: "c".into() }),
            risk: 0,
            abilities: abilities.into_iter().map(String::from).collect(),
            state: "running".into(),
            off: None,
            problems: Vec::new(),
            mark: "by hand".into(),
        };
        let mut w = PluginList::new(vec![entry("a", vec!["Short"]), entry("b", vec!["A longer line of what this one can do"])], Default::default());
        let screen = Rect::new(0, 0, 120, 40);
        let frame = |w: &PluginList| {
            let mut buf = Buffer::empty(screen);
            w.draw(&mut buf, screen, &Theme::dark());
            let corners: Vec<(u16, u16)> = (0..screen.height).flat_map(|y| (0..screen.width).map(move |x| (x, y))).filter(|&(x, y)| ["┌", "┘", "╭", "╯"].contains(&buf[(x, y)].symbol())).collect();
            corners
        };
        let first = frame(&w);
        assert!(!first.is_empty());
        let down = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Down, crossterm::event::KeyModifiers::NONE);
        for _ in 0..3 {
            w.key(down, screen, &Drawn::default());
            assert_eq!(frame(&w), first);
        }
    }

    #[test]
    fn a_row_wider_than_the_text_is_shown_whole() {
        let e = Entry {
            name: "waiting".into(),
            title: "Waiting".into(),
            version: Some("1.0.0".into()),
            pin: None,
            risk: 0,
            abilities: Vec::new(),
            state: "needs a server restart".into(),
            off: None,
            problems: Vec::new(),
            mark: "not in catalog".into(),
        };
        let w = PluginList::new(vec![e], Default::default());
        let screen = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(screen);
        w.draw(&mut buf, screen, &Theme::dark());
        let text: String = (0..screen.height).map(|y| (0..screen.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
        assert!(text.contains("only shows things  not in catalog  needs a server restart"), "{text}");
    }

    #[test]
    fn only_a_plugin_put_in_by_hand_has_its_files_walked_each_time() {
        crate::session::tests::with_temp_state_dir("plugin-list-walk", || {
            let root = crate::paths::state_root();
            std::fs::create_dir_all(root.join("outside")).unwrap();
            for name in ["hand", "pinned"] {
                let dir = root.join("plugins").join(name);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("plugin.json"), serde_json::json!({ "name": name, "title": name, "exec": { "command": "sh" }, "badges": ["tree"] }).to_string()).unwrap();
                std::os::unix::fs::symlink(root.join("outside"), dir.join("out")).unwrap();
            }
            let pin = crate::install::Pin { name: "pinned".into(), source: "https://x/p.git".into(), tag: None, commit: "c".repeat(40) };
            std::fs::write(root.join(crate::install::LOCK), serde_json::to_string(&[pin]).unwrap()).unwrap();
            let e = entries(&crate::catalog::Catalog::default(), |_| "running");
            let problems: Vec<(&str, &[String])> = e.iter().map(|e| (e.name.as_str(), e.problems.as_slice())).collect();
            assert_eq!(problems, [("hand", &["out links outside the plugin's folder".to_string()][..]), ("pinned", &[][..])], "the install walked it");
        });
    }

    #[test]
    fn a_catalog_plugin_shows_its_github_stars_and_never_a_namesakes() {
        let dir = std::env::temp_dir().join(format!("nerd-stars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        let entry = serde_json::json!({ "title": "Todos", "url": "https://github.com/me/todos.git", "tag": "v1.0.0", "commit": "a".repeat(40), "version": "1.0.0", "risk": 0, "review": "checked" });
        std::fs::write(dir.join("plugins/todos.json"), entry.to_string()).unwrap();
        std::fs::write(dir.join("stars.json"), r#"{"changed":"2026-10-01","stars":{"todos":128}}"#).unwrap();
        let w = PluginList::new(Vec::new(), crate::catalog::read_at(&dir).0);
        let screen = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(screen);
        w.draw(&mut buf, screen, &Theme::dark());
        let text: String = (0..screen.height).map(|y| (0..screen.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
        assert!(text.contains("Stars") && text.contains("128"), "{text}");
        let details = w.content(screen, &Theme::dark(), Some(2)).body.iter().map(|l| l.iter().map(|s| s.1.as_str()).collect::<String>()).collect::<Vec<_>>().join(" ");
        assert!(details.contains("128 stars on GitHub; the count last changed on 2026-10-01."), "{details}");
        let namesake = |source: &str| Entry {
            name: "todos".into(),
            title: "Todos".into(),
            version: Some("1.0.0".into()),
            pin: Some(crate::install::Pin { name: "todos".into(), source: source.into(), tag: None, commit: "a".repeat(40) }),
            risk: 0,
            abilities: Vec::new(),
            state: "running".into(),
            off: None,
            problems: Vec::new(),
            mark: "not in catalog".into(),
        };
        let w = PluginList::new(vec![namesake("https://github.com/evil/todos.git")], crate::catalog::read_at(&dir).0);
        let details = w.content(screen, &Theme::dark(), Some(2)).body.iter().map(|l| l.iter().map(|s| s.1.as_str()).collect::<String>()).collect::<Vec<_>>().join(" ");
        assert!(w.stars.is_empty() && !details.contains("stars on GitHub"), "another address never wears the catalog's stars: {details}");
        let w = PluginList::new(vec![namesake("https://github.com/me/todos.git")], crate::catalog::read_at(&dir).0);
        assert_eq!(w.stars.get("todos"), Some(&128), "the catalog's own release keeps them once installed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_reads_as_host_and_path() {
        assert_eq!(short("https://github.com/me/x.git"), "github.com/me/x");
    }
}
