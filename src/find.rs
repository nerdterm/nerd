//! Main menu > Search…: one field over panes, menu commands, settings, plugins, files, help and pane text.
//! The window only finds; each result acts through the code its origin uses, in `app::find`.

use std::cell::Cell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::layout::PaneId;
use crate::render::Theme;
use crate::term::AgentState;

/// Rows a group shows under All before "N more in <group>".
const CAP: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Panes,
    Commands,
    Settings,
    Plugins,
    Files,
    Help,
    Text,
}

pub const GROUPS: [Group; 7] = [Group::Panes, Group::Commands, Group::Settings, Group::Plugins, Group::Files, Group::Help, Group::Text];

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Panes => "Panes",
            Group::Commands => "Commands",
            Group::Settings => "Settings",
            Group::Plugins => "Plugins",
            Group::Files => "Files",
            Group::Help => "Help",
            Group::Text => "Text",
        }
    }
}

/// The Panes tab's filter by state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    All,
    Blocked,
    Working,
    Idle,
    Done,
}

const CHIPS: [Chip; 5] = [Chip::All, Chip::Blocked, Chip::Working, Chip::Idle, Chip::Done];

impl Chip {
    fn name(self) -> &'static str {
        match self {
            Chip::All => "all",
            Chip::Blocked => "blocked",
            Chip::Working => "working",
            Chip::Idle => "idle",
            Chip::Done => "done",
        }
    }

    /// `Unknown` is only in all: a fresh pane is neither blocked nor working.
    fn fits(self, s: AgentState) -> bool {
        match self {
            Chip::All => true,
            Chip::Blocked => matches!(s, AgentState::Blocked | AgentState::Failed),
            Chip::Working => s == AgentState::Working,
            Chip::Idle => s == AgentState::Idle,
            Chip::Done => s == AgentState::Done,
        }
    }
}

/// The word a pane's state is found by, the chips' own.
pub fn state_word(s: AgentState) -> &'static str {
    CHIPS.iter().skip(1).find(|c| c.fits(s)).map_or("", |c| c.name())
}

/// What a command acts on: a pane taken when the window opened, found again when it acts.
#[derive(Clone, Debug, PartialEq)]
pub enum Ctx {
    Global,
    /// The space and the tab of this pane.
    Space(PaneId),
    Tab(PaneId),
    Pane(PaneId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Pane(PaneId),
    Command(crate::app::Action, Ctx),
    /// A Settings row by its label; true for the switches that change in the row.
    Setting(&'static str, bool),
    /// A plugin by name; true when it is installed.
    Plugin(String, bool),
    File(PathBuf),
    /// A section of `help::sections()`.
    Help(usize),
    /// A line of a pane's text, by its row from the top of the live screen; history is negative.
    Line(PaneId, isize),
}

pub struct Hit {
    pub group: Group,
    pub name: String,
    pub place: String,
    pub value: String,
    /// A greyed menu item: shown, does nothing.
    pub enabled: bool,
    /// Listed before anything is typed.
    pub top: bool,
    pub target: Target,
    /// A pane's space (index and name), tab, state, and whether it has the keys.
    space: (usize, String),
    tab: String,
    state: Option<AgentState>,
    current: bool,
    extra: String,
    name_lc: String,
    hay: String,
}

impl Hit {
    pub fn new(group: Group, name: impl AsRef<str>, place: impl AsRef<str>, target: Target) -> Self {
        Self {
            group,
            name: printable(name.as_ref()),
            place: printable(place.as_ref()),
            value: String::new(),
            enabled: true,
            top: true,
            target,
            space: (0, String::new()),
            tab: String::new(),
            state: None,
            current: false,
            extra: String::new(),
            name_lc: String::new(),
            hay: String::new(),
        }
    }

    pub fn value(mut self, v: impl AsRef<str>) -> Self {
        self.value = printable(v.as_ref());
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn top(mut self, top: bool) -> Self {
        self.top = top;
        self
    }

    /// Words it is also found by, never shown.
    pub fn extra(mut self, words: impl AsRef<str>) -> Self {
        self.extra = words.as_ref().to_string();
        self
    }

    pub fn pane(mut self, space: (usize, String), tab: String, state: AgentState, current: bool) -> Self {
        self.space = (space.0, printable(&space.1));
        self.tab = printable(&tab);
        self.state = Some(state);
        self.current = current;
        self
    }
}

/// Every row's text: a control character from a file, a plugin or a pane would move the cursor.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[derive(Clone, Debug, PartialEq)]
enum Row {
    Head(Group),
    /// A space's header over its panes in the tree, by its first pane.
    Space(usize),
    /// A pane in the tree: the hit, the last of its space, its space has more than one tab.
    Tree(usize, bool, bool),
    Hit(usize),
    More(Group, usize),
    /// "Search pane text for …": the text is read only on this.
    AskText,
    Note(String),
}

impl Row {
    fn acts(&self) -> bool {
        matches!(self, Row::Tree(..) | Row::Hit(_) | Row::More(..) | Row::AskText)
    }
}

pub enum Outcome {
    None,
    Close,
    /// A result chosen, and the name of what it acts on, for the notice when that is gone.
    Act(Target, String),
    /// Read every pane's text for the query.
    SearchText,
    /// A help page's address.
    Open(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Click {
    Tab(Option<Group>),
    Chip(Chip),
    /// By its index into the rows drawn.
    Row(usize),
    Open,
}

/// Where the last frame put the window's tabs, chips, rows and buttons.
#[derive(Default)]
pub struct Drawn {
    pub hits: Vec<(Rect, Click)>,
}

pub struct Find {
    pub query: String,
    /// None is All.
    pub tab: Option<Group>,
    pub chip: Chip,
    hits: Vec<Hit>,
    /// Index into `rows()`, on a row that acts when there is one.
    selected: usize,
    /// A help section shown in the window, and how far it is scrolled.
    help: Option<(usize, usize)>,
    /// How far the help page can scroll at the size last drawn.
    help_max: Cell<usize>,
    /// Plugins and Help were read; Files were walked. Once per window, on the first query that needs them.
    pub lazy: bool,
    pub files: bool,
    pub files_note: Option<String>,
    /// The query the Text hits were found for, and how far the search got when it stopped early.
    text: Option<String>,
    pub text_note: Option<String>,
}

impl Default for Find {
    fn default() -> Self {
        Self::new()
    }
}

/// One key the window answers; the handler dispatches on these rows and the strip prints them.
pub struct Key {
    pub code: KeyCode,
    pub label: &'static str,
    pub word: &'static str,
    pub what: &'static str,
    act: fn(&mut Find) -> Outcome,
}

pub const KEYS: &[Key] = &[
    Key { code: KeyCode::Up, label: "↑↓", word: "", what: "up and down the results", act: |f| { f.step(-1); Outcome::None } },
    Key { code: KeyCode::Down, label: "", word: "", what: "up and down the results", act: |f| { f.step(1); Outcome::None } },
    Key { code: KeyCode::Enter, label: "enter", word: "open", what: "go to, run, change or open the chosen result", act: |f| f.act(f.selected) },
    Key { code: KeyCode::Tab, label: "tab", word: "next group", what: "the next group; with shift, the one before", act: |f| { f.cycle(1); Outcome::None } },
    Key { code: KeyCode::BackTab, label: "", word: "", what: "the next group; with shift, the one before", act: |f| { f.cycle(-1); Outcome::None } },
    Key { code: KeyCode::Esc, label: "esc", word: "close", what: "close; from a help page, back", act: |_| Outcome::Close },
];

impl Find {
    pub fn new() -> Self {
        Self { query: String::new(), tab: None, chip: Chip::All, hits: Vec::new(), selected: 0, help: None, help_max: Cell::new(0), lazy: false, files: false, files_note: None, text: None, text_note: None }
    }

    pub fn add(&mut self, hits: Vec<Hit>) {
        for mut h in hits {
            h.name_lc = h.name.to_lowercase();
            h.hay = format!("{}\n{}\n{}", h.name, h.place, h.extra).to_lowercase();
            self.hits.push(h);
        }
        self.clamp();
    }

    /// One group's hits read afresh: a setting or a plugin the window changed shows its new value.
    pub fn replace(&mut self, group: Group, hits: Vec<Hit>) {
        self.hits.retain(|h| h.group != group);
        self.add(hits);
    }

    #[cfg(test)]
    pub fn of(&self, group: Group) -> Vec<&Hit> {
        self.hits.iter().filter(|h| h.group == group).collect()
    }

    pub fn wants_lazy(&self) -> bool {
        !self.lazy && (!self.words().is_empty() || matches!(self.tab, Some(Group::Plugins | Group::Help)))
    }

    pub fn wants_files(&self) -> bool {
        !self.files && !self.words().is_empty()
    }

    /// The query's words, lowercase: a result has every one.
    pub fn words(&self) -> Vec<String> {
        self.query.to_lowercase().split_whitespace().map(String::from).collect()
    }

    /// What the Text search found for the query now; the Text tab shows it.
    pub fn set_text(&mut self, hits: Vec<Hit>, note: Option<String>) {
        self.replace(Group::Text, hits);
        self.text = Some(self.query.trim().to_lowercase());
        self.text_note = note;
        self.set_tab(Some(Group::Text));
    }

    /// The window is as large as a window gets, so it never moves while typing.
    pub fn rect(&self, screen: Rect) -> Rect {
        crate::chrome::fit(screen, u16::MAX, u16::MAX)
    }

    /// A hit's rank for the query, best first; None when it lacks a word.
    fn rank(h: &Hit, q: &str, words: &[String]) -> Option<u8> {
        if !words.iter().all(|w| h.hay.contains(w.as_str())) {
            return None;
        }
        if h.name_lc.starts_with(q) {
            return Some(0);
        }
        let starts = h.name_lc.split(|c: char| !c.is_alphanumeric()).any(|nw| words.iter().any(|w| nw.starts_with(w.as_str())));
        Some(if starts { 1 } else { 2 })
    }

    /// One group's hits for the query, best first, ties in source order.
    /// ceiling: words matched as substrings, not fuzzy; a typo finds nothing. A permissive-licence fuzzy scorer if people miss it.
    fn matches(&self, group: Group, q: &str, words: &[String]) -> Vec<usize> {
        let chip = if self.tab == Some(Group::Panes) { self.chip } else { Chip::All };
        let mut m: Vec<(u8, usize)> = (0..self.hits.len())
            .filter(|&i| self.hits[i].group == group && self.hits[i].state.is_none_or(|s| chip.fits(s)))
            .filter_map(|i| Self::rank(&self.hits[i], q, words).map(|r| (r, i)))
            .collect();
        m.sort_by_key(|(r, _)| *r);
        m.into_iter().map(|(_, i)| i).collect()
    }

    fn rows(&self) -> Vec<Row> {
        let q = self.query.trim().to_lowercase();
        let words = self.words();
        let note = |s: &str| Row::Note(s.to_string());
        if words.is_empty() {
            return match self.tab {
                None | Some(Group::Panes) => self.tree(),
                Some(Group::Files) => vec![note("Type to search the names of the files under the file tree's folder.")],
                Some(Group::Text) => vec![note("Type, then press Enter to search the text of every pane.")],
                Some(g) => {
                    let rows: Vec<Row> = (0..self.hits.len()).filter(|&i| self.hits[i].group == g && self.hits[i].top).map(Row::Hit).collect();
                    if rows.is_empty() { vec![note("Nothing here.")] } else { rows }
                }
            };
        }
        let mut out = Vec::new();
        match self.tab {
            None => {
                for g in GROUPS.into_iter().filter(|g| *g != Group::Text) {
                    let m = self.matches(g, &q, &words);
                    if m.is_empty() {
                        continue;
                    }
                    out.push(Row::Head(g));
                    out.extend(m.iter().take(CAP).map(|&i| Row::Hit(i)));
                    if m.len() > CAP {
                        out.push(Row::More(g, m.len() - CAP));
                    }
                }
                if out.is_empty() {
                    out.push(note("Nothing found."));
                }
                out.push(Row::AskText);
            }
            Some(Group::Text) if self.text.as_deref() == Some(q.as_str()) => {
                out.extend(self.matches(Group::Text, &q, &words).into_iter().map(Row::Hit));
                if out.is_empty() {
                    out.push(note("No pane has this text."));
                }
                out.extend(self.text_note.as_deref().map(note));
            }
            Some(Group::Text) => out.push(Row::AskText),
            Some(g) => {
                out.extend(self.matches(g, &q, &words).into_iter().map(Row::Hit));
                if out.is_empty() {
                    out.push(note("Nothing found."));
                }
                if g == Group::Files {
                    out.extend(self.files_note.as_deref().map(note));
                }
            }
        }
        out
    }

    /// Every pane under its space, as they are laid out.
    fn tree(&self) -> Vec<Row> {
        let chip = if self.tab == Some(Group::Panes) { self.chip } else { Chip::All };
        let panes: Vec<usize> = (0..self.hits.len()).filter(|&i| self.hits[i].group == Group::Panes && self.hits[i].state.is_some_and(|s| chip.fits(s))).collect();
        if panes.is_empty() {
            return vec![Row::Note("No panes.".into())];
        }
        let mut out = Vec::new();
        for (k, &i) in panes.iter().enumerate() {
            let space = self.hits[i].space.0;
            if k == 0 || self.hits[panes[k - 1]].space.0 != space {
                out.push(Row::Space(i));
            }
            let same: Vec<&Hit> = panes.iter().map(|&j| &self.hits[j]).filter(|h| h.space.0 == space).collect();
            let tabs = same.iter().any(|h| h.tab != same[0].tab);
            let last = panes.get(k + 1).is_none_or(|&j| self.hits[j].space.0 != space);
            out.push(Row::Tree(i, last, tabs));
        }
        out
    }

    fn clamp(&mut self) {
        let rows = self.rows();
        if !rows.get(self.selected).is_some_and(Row::acts) {
            self.selected = rows.iter().position(Row::acts).unwrap_or(0);
        }
    }

    fn typed(&mut self) {
        self.selected = 0;
        self.clamp();
    }

    pub fn set_tab(&mut self, tab: Option<Group>) {
        self.tab = tab;
        self.typed();
    }

    fn cycle(&mut self, d: isize) {
        let tabs: Vec<Option<Group>> = std::iter::once(None).chain(GROUPS.map(Some)).collect();
        let at = tabs.iter().position(|t| *t == self.tab).unwrap_or(0) as isize;
        self.set_tab(tabs[(at + d).rem_euclid(tabs.len() as isize) as usize]);
    }

    fn step(&mut self, d: isize) {
        let acting: Vec<usize> = self.rows().iter().enumerate().filter(|(_, r)| r.acts()).map(|(i, _)| i).collect();
        if acting.is_empty() {
            return;
        }
        let at = acting.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        self.selected = acting[(at + d).rem_euclid(acting.len() as isize) as usize];
    }

    fn act(&mut self, i: usize) -> Outcome {
        match self.rows().get(i).cloned() {
            Some(Row::Tree(h, ..) | Row::Hit(h)) => {
                let hit = &self.hits[h];
                if !hit.enabled {
                    return Outcome::None;
                }
                match hit.target {
                    Target::Help(s) => {
                        self.help = Some((s, 0));
                        Outcome::None
                    }
                    _ => {
                        let what = if matches!(hit.group, Group::Commands | Group::Text) { hit.place.clone() } else { hit.name.clone() };
                        Outcome::Act(hit.target.clone(), what)
                    }
                }
            }
            Some(Row::More(g, _)) => {
                self.set_tab(Some(g));
                Outcome::None
            }
            Some(Row::AskText) => Outcome::SearchText,
            _ => Outcome::None,
        }
    }

    fn page(&self) -> Option<String> {
        self.help.and_then(|(s, _)| crate::help::sections().get(s)?.url.clone())
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        if let Some((s, scroll)) = self.help {
            // A help page's field takes no keys: `o` is the button's.
            match crate::keys::normalize(k).code {
                KeyCode::Esc => self.help = None,
                KeyCode::Char('o') if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => return self.page().map_or(Outcome::None, Outcome::Open),
                KeyCode::Up => self.help = Some((s, scroll.min(self.help_max.get()).saturating_sub(1))),
                KeyCode::Down => self.help = Some((s, (scroll + 1).min(self.help_max.get()))),
                _ => {}
            }
            return Outcome::None;
        }
        match k.code {
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.query.push(c);
                self.typed();
                Outcome::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.typed();
                Outcome::None
            }
            code => KEYS.iter().find(|r| r.code == code).map_or(Outcome::None, |r| (r.act)(self)),
        }
    }

    pub fn mouse(&mut self, ev: MouseEvent, screen: Rect, drawn: &Drawn) -> Outcome {
        use crossterm::event::{MouseButton, MouseEventKind::*};
        let at = (ev.column, ev.row).into();
        let hit = drawn.hits.iter().find(|(r, _)| r.contains(at)).map(|(_, c)| *c);
        match ev.kind {
            Moved => {
                if let Some(Click::Row(i)) = hit
                    && self.rows().get(i).is_some_and(Row::acts)
                {
                    self.selected = i;
                }
                Outcome::None
            }
            Down(MouseButton::Left) => {
                let r = self.rect(screen);
                if !r.contains(at) {
                    return Outcome::Close;
                }
                if crate::chrome::close_rect(r).contains(at) {
                    if self.help.take().is_some() {
                        return Outcome::None;
                    }
                    return Outcome::Close;
                }
                match hit {
                    Some(Click::Tab(t)) => self.set_tab(t),
                    Some(Click::Chip(c)) => {
                        self.chip = c;
                        self.typed();
                    }
                    Some(Click::Row(i)) if self.rows().get(i).is_some_and(Row::acts) => {
                        self.selected = i;
                        return self.act(i);
                    }
                    Some(Click::Open) => return self.page().map_or(Outcome::None, Outcome::Open),
                    _ => {}
                }
                Outcome::None
            }
            ScrollUp | ScrollDown => {
                let d = if ev.kind == ScrollUp { -3 } else { 3 };
                match self.help.as_mut() {
                    Some((_, scroll)) => *scroll = scroll.saturating_add_signed(d).min(self.help_max.get()),
                    None => self.step(d),
                }
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    /// The filter's time for this query, for the log.
    pub fn log_filter(&self) {
        let t = std::time::Instant::now();
        let n = self.rows().len();
        crate::log::debug(&format!("search: {n} rows for a {}-character query in {} µs", self.query.chars().count(), t.elapsed().as_micros()));
    }

    /// Where the typing goes, while the field takes keys.
    pub fn cursor(&self, screen: Rect) -> Option<(u16, u16)> {
        if self.help.is_some() {
            return None;
        }
        let inner = self.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let x = inner.x + crate::slots::cells(&self.query) as u16;
        Some((x.min(inner.right().saturating_sub(1)), inner.y))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn {
        let mut d = Drawn::default();
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return d;
        }
        crate::chrome::dialog(buf, r, Some("Search"), theme.border_focus, theme, crate::chrome::Moat::All);
        let inner = if self.help.is_some() {
            let b = crate::chrome::close_rect(r);
            buf.set_stringn(b.x, b.y, " [Esc] back ", b.width as usize, Style::default().bg(theme.ground).fg(theme.text));
            let inner = r.inner(crate::chrome::DIALOG_FRAME);
            Rect { height: inner.height.saturating_sub(2), ..inner }
        } else {
            crate::chrome::close_button(buf, r, theme)
        };
        if inner.width == 0 || inner.height == 0 {
            return d;
        }
        match self.help {
            Some((s, scroll)) => self.draw_help(buf, inner, s, scroll, theme, &mut d),
            None => self.draw_list(buf, inner, theme, &mut d),
        }
        d
    }

    fn draw_help(&self, buf: &mut Buffer, inner: Rect, s: usize, scroll: usize, theme: &Theme, d: &mut Drawn) {
        let Some(sec) = crate::help::sections().get(s) else { return };
        let w = inner.width as usize;
        buf.set_stringn(inner.x, inner.y, &sec.name, w, Style::default().fg(theme.text).add_modifier(Modifier::BOLD));
        let mut bottom = inner.bottom();
        if sec.url.is_some() {
            bottom -= 1;
            let label = " [o] open on nerdterm.dev ";
            buf.set_stringn(inner.x, bottom, label, w, Style::default().fg(theme.text));
            d.hits.push((Rect::new(inner.x, bottom, (label.chars().count() as u16).min(inner.width), 1), Click::Open));
        }
        let lines: Vec<String> = sec.body.lines().flat_map(|l| if l.is_empty() { vec![String::new()] } else { crate::menu::wrap(l, w) }).collect();
        let rows = bottom.saturating_sub(1).saturating_sub(inner.y + 2) as usize;
        self.help_max.set(lines.len().saturating_sub(rows));
        for (y, line) in (inner.y + 2..bottom.saturating_sub(1)).zip(lines.iter().skip(scroll.min(self.help_max.get()))) {
            buf.set_stringn(inner.x, y, line, w, Style::default().fg(theme.text));
        }
    }

    fn draw_list(&self, buf: &mut Buffer, inner: Rect, theme: &Theme, d: &mut Drawn) {
        let w = inner.width as usize;
        let mut y = inner.y;
        if self.query.is_empty() {
            buf.set_stringn(inner.x, y, "Type to search panes, commands, settings, plugins, files, help and pane text", w, Style::default().fg(theme.faint));
        } else {
            let shown = crate::slots::cells(&self.query).saturating_sub(w.saturating_sub(1));
            let tail: String = self.query.chars().skip(shown).collect();
            buf.set_stringn(inner.x, y, tail, w, Style::default().fg(theme.text));
        }
        y += 1;
        // The tabs, then the Panes tab's chips: words and chips to click, a key for each only as Tab.
        let mut x = inner.x;
        for t in std::iter::once(None).chain(GROUPS.map(Some)) {
            let word = t.map_or("All", Group::name);
            let n = word.chars().count() as u16;
            if x + n > inner.right() {
                break;
            }
            let style = if t == self.tab { theme.chosen() } else { Style::default().fg(theme.muted) };
            buf.set_stringn(x, y, word, n as usize, style);
            d.hits.push((Rect::new(x, y, n, 1), Click::Tab(t)));
            x += n + 2;
        }
        y += 1;
        if self.tab == Some(Group::Panes) && y < inner.bottom() {
            let mut x = inner.x;
            for c in CHIPS {
                let cw = crate::chrome::chip_width(c.name(), &[]);
                if x + cw > inner.right() {
                    break;
                }
                let rect = crate::chrome::draw_chip(buf, x, y, c.name(), &[], c == self.chip, false, theme);
                d.hits.push((rect, Click::Chip(c)));
                x = rect.right() + 1;
            }
            y += 1;
        }
        let strip = crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), w);
        let bottom = crate::chrome::draw_hint(buf, inner, &strip, theme).max(y);

        let rows = self.rows();
        let room = bottom.saturating_sub(y) as usize;
        let skip = (self.selected + 1).saturating_sub(room);
        let words = self.words();
        let name_w = (w * 2 / 5).max(12);
        for (i, row) in rows.iter().enumerate().skip(skip) {
            if y >= bottom {
                break;
            }
            let line = Rect::new(inner.x, y, inner.width, 1);
            let text = Style::default().fg(theme.text);
            match row {
                Row::Head(g) => {
                    buf.set_stringn(inner.x, y, g.name(), w, Style::default().fg(theme.muted));
                }
                Row::Space(h) => {
                    buf.set_stringn(inner.x, y, format!("{} ▾", self.hits[*h].space.1), w, text.add_modifier(Modifier::BOLD));
                }
                Row::Tree(h, last, tabs) => {
                    let hit = &self.hits[*h];
                    let state = hit.state.unwrap_or(AgentState::Unknown);
                    let mut s = format!("{}{} ", if *last { "└── " } else { "├── " }, state.mark());
                    let at = s.chars().count() as u16;
                    s.push_str(&hit.name);
                    if *tabs {
                        s.push_str(" · ");
                        s.push_str(&hit.tab);
                    }
                    buf.set_stringn(inner.x, y, &s, w, text);
                    if at > 1 && at - 2 < inner.width {
                        buf.set_stringn(inner.x + at - 2, y, state.mark().to_string(), 1, Style::default().fg(crate::sidebar::state_color(state, theme)));
                    }
                    if hit.current && at < inner.width {
                        buf.set_stringn(inner.x + at, y, &hit.name, (inner.width - at) as usize, theme.chosen());
                    }
                    self.draw_value(buf, line, s.chars().count(), &hit.value, theme);
                }
                Row::Hit(h) => {
                    let hit = &self.hits[*h];
                    let style = if hit.enabled { text } else { Style::default().fg(theme.faint) };
                    buf.set_stringn(inner.x + 2, y, &hit.name, name_w.min(w.saturating_sub(2)), style);
                    let value = if hit.group == Group::Help { snippet(*h, &self.hits, &words) } else { hit.value.clone() };
                    let used = self.draw_value(buf, line, 2 + name_w + 2, &value, theme);
                    let px = inner.x + (2 + name_w + 2) as u16;
                    if px < inner.right() {
                        buf.set_stringn(px, y, &hit.place, used.saturating_sub(2 + name_w + 3), Style::default().fg(theme.muted));
                    }
                }
                Row::More(g, n) => {
                    buf.set_stringn(inner.x + 2, y, format!("{n} more in {}", g.name()), w.saturating_sub(2), Style::default().fg(theme.muted));
                }
                Row::AskText => {
                    buf.set_stringn(inner.x + 2, y, format!("Search pane text for \"{}\"", self.query.trim()), w.saturating_sub(2), text);
                }
                Row::Note(s) => {
                    buf.set_stringn(inner.x + 2, y, s, w.saturating_sub(2), Style::default().fg(theme.faint));
                }
            }
            if row.acts() {
                if i == self.selected {
                    buf.set_style(line, theme.cursor());
                }
                d.hits.push((line, Click::Row(i)));
            }
            y += 1;
        }
    }

    /// Draws `value` right-aligned after `from` columns, cut with "…" when long; returns the columns left before it.
    fn draw_value(&self, buf: &mut Buffer, line: Rect, from: usize, value: &str, theme: &Theme) -> usize {
        let w = line.width as usize;
        let room = w.saturating_sub(from + 2);
        if value.is_empty() || room < 2 {
            return w;
        }
        let value = if crate::slots::cells(value) > room { format!("{}…", value.chars().take(room - 1).collect::<String>()) } else { value.to_string() };
        let vw = crate::slots::cells(&value).min(room);
        buf.set_stringn(line.x + (w - vw) as u16, line.y, &value, vw, Style::default().fg(theme.faint));
        w - vw
    }
}

/// The help section's first line with the query's first word, as its value.
fn snippet(h: usize, hits: &[Hit], words: &[String]) -> String {
    let Target::Help(s) = hits[h].target else { return String::new() };
    let (Some(sec), Some(w)) = (crate::help::sections().get(s), words.first()) else { return String::new() };
    sec.body.lines().find(|l| l.to_lowercase().contains(w.as_str())).map(|l| format!("\"{}\"", printable(l.trim()))).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Action;
    use crossterm::event::{MouseButton, MouseEventKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(f: &mut Find, s: &str) {
        for c in s.chars() {
            f.key(key(KeyCode::Char(c)));
        }
    }

    fn command(name: &str) -> Hit {
        Hit::new(Group::Commands, name, "Main menu", Target::Command(Action::NewTab, Ctx::Global))
    }

    fn pane(id: PaneId, space: usize, name: &str, state: AgentState) -> Hit {
        Hit::new(Group::Panes, name, format!("s{space} › 1"), Target::Pane(id)).extra(state_word(state)).pane((space, format!("s{space}")), "1".into(), state, false)
    }

    /// The names the window lists now, with "Head" rows as `#Group` and more rows as `+N`.
    fn listed(f: &Find) -> Vec<String> {
        f.rows()
            .iter()
            .map(|r| match r {
                Row::Head(g) => format!("#{}", g.name()),
                Row::Space(h) => format!("^{}", f.hits[*h].space.1),
                Row::Tree(h, ..) | Row::Hit(h) => f.hits[*h].name.clone(),
                Row::More(g, n) => format!("+{n} {}", g.name()),
                Row::AskText => "?text".into(),
                Row::Note(s) => format!("({s})"),
            })
            .collect()
    }

    fn chosen(f: &Find) -> String {
        listed(f)[f.selected].clone()
    }

    #[test]
    fn every_word_must_match_and_the_best_name_comes_first() {
        let mut f = Find::new();
        f.add(vec![command("Rewrite history"), command("Don't wrap long lines"), command("Wrap long lines"), command("Split right")]);
        f.tab = Some(Group::Commands);
        f.query = "wr".into();
        assert_eq!(listed(&f), ["Wrap long lines", "Don't wrap long lines", "Rewrite history"], "name starts, a word starts, anywhere; ties in source order");
        f.query = "LONG wrap".into();
        assert_eq!(listed(&f), ["Don't wrap long lines", "Wrap long lines"], "every word, any order, any case");
        f.query = "wrap right".into();
        assert_eq!(listed(&f), ["(Nothing found.)"]);
        f.query = "main menu".into();
        assert_eq!(listed(&f).len(), 4, "the place is searched too");
    }

    #[test]
    fn all_caps_each_group_and_its_more_row_opens_the_group() {
        let mut f = Find::new();
        f.add((0..7).map(|i| command(&format!("New thing {i}"))).collect());
        f.add(vec![pane(1, 0, "new-agent", AgentState::Idle)]);
        typed(&mut f, "new");
        assert_eq!(listed(&f), ["#Panes", "new-agent", "#Commands", "New thing 0", "New thing 1", "New thing 2", "New thing 3", "New thing 4", "+2 Commands", "?text"]);
        assert_eq!(chosen(&f), "new-agent", "the first result is chosen");
        for _ in 0..6 {
            f.key(key(KeyCode::Down));
        }
        assert_eq!(chosen(&f), "+2 Commands", "headers are skipped");
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::None));
        assert_eq!(f.tab, Some(Group::Commands));
        assert_eq!(listed(&f).len(), 7, "the group shows all of its results");
        f.key(key(KeyCode::Down));
        f.key(key(KeyCode::Up));
        f.key(key(KeyCode::Up));
        assert_eq!(chosen(&f), "New thing 6", "the arrows wrap");
    }

    #[test]
    fn digits_and_letters_type_and_tab_changes_group() {
        let mut f = Find::new();
        f.add(vec![command("Close tab…")]);
        typed(&mut f, "v0.16 abiwd/");
        assert_eq!(f.query, "v0.16 abiwd/", "no key but the strip's is a command");
        f.key(key(KeyCode::Tab));
        assert_eq!(f.tab, Some(Group::Panes));
        f.key(key(KeyCode::BackTab));
        f.key(key(KeyCode::BackTab));
        assert_eq!(f.tab, Some(Group::Text), "back from All is the last group");
        f.key(key(KeyCode::Backspace));
        assert_eq!(f.query, "v0.16 abiwd");
        assert!(matches!(f.key(key(KeyCode::Esc)), Outcome::Close));
        assert!(matches!(f.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT)), Outcome::None));
        assert_eq!(f.query, "v0.16 abiwd", "a chord types nothing");
    }

    #[test]
    fn panes_are_found_by_state_word_and_filtered_by_chip() {
        let mut f = Find::new();
        f.add(vec![pane(1, 0, "claude", AgentState::Working), pane(2, 0, "bun", AgentState::Failed), pane(3, 1, "codex", AgentState::Blocked)]);
        assert_eq!(listed(&f), ["^s0", "claude", "bun", "^s1", "codex"], "no query: the tree");
        typed(&mut f, "blocked");
        assert_eq!(listed(&f), ["#Panes", "bun", "codex", "?text"], "a failed turn waits for you too");
        f.query.clear();
        f.set_tab(Some(Group::Panes));
        f.chip = Chip::Working;
        assert_eq!(listed(&f), ["^s0", "claude"]);
        assert!(matches!(f.act(f.selected), Outcome::Act(Target::Pane(1), ref n) if n == "claude"));
    }

    #[test]
    fn a_greyed_command_is_listed_and_does_nothing() {
        let mut f = Find::new();
        f.add(vec![command("Move up").enabled(false)]);
        typed(&mut f, "move");
        assert_eq!(listed(&f)[1], "Move up");
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::None));
    }

    #[test]
    fn text_is_searched_only_when_asked() {
        let mut f = Find::new();
        typed(&mut f, "error");
        assert_eq!(listed(&f), ["(Nothing found.)", "?text"]);
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::SearchText), "the last row of All");
        f.set_text(vec![Hit::new(Group::Text, "error: no such file", "Pane claude", Target::Line(1, -3))], Some("searched 1 of 2 panes".into()));
        assert_eq!(f.tab, Some(Group::Text));
        assert_eq!(listed(&f), ["error: no such file", "(searched 1 of 2 panes)"]);
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::Act(Target::Line(1, -3), ref n) if n == "Pane claude"));
        typed(&mut f, "s");
        assert_eq!(listed(&f), ["?text"], "another query asks again");
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::SearchText));
    }

    #[test]
    fn a_help_result_opens_its_section_in_the_window_and_its_field_takes_no_keys() {
        let mut f = Find::new();
        let at = crate::help::sections().iter().position(|s| s.name == "Privacy and security › What leaves nerd").unwrap();
        f.add(vec![Hit::new(Group::Help, "Privacy and security › What leaves nerd", "", Target::Help(at)).extra(&crate::help::sections()[at].body)]);
        typed(&mut f, "leaves");
        f.key(key(KeyCode::Enter));
        assert!(f.help.is_some());
        assert!(matches!(f.key(key(KeyCode::Char('o'))), Outcome::Open(u) if u == "https://nerdterm.dev/security/#what-leaves-nerd"));
        assert_eq!(f.query, "leaves", "o is the button's");
        assert!(matches!(f.key(key(KeyCode::Esc)), Outcome::None), "Esc goes back");
        assert!(f.help.is_none());
    }

    #[test]
    fn a_chord_in_a_help_view_opens_nothing_and_its_scroll_ends_at_its_last_wrapped_line() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        let at = crate::help::sections().iter().position(|s| s.name == "Privacy and security › What leaves nerd").unwrap();
        f.help = Some((at, 0));
        for m in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            assert!(matches!(f.key(KeyEvent::new(KeyCode::Char('o'), m)), Outcome::None), "{m:?}+o");
        }
        let draw = |f: &Find| f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        draw(&f);
        let max = f.help_max.get();
        assert!(max > crate::help::sections()[at].body.lines().count().saturating_sub(40), "wrapped lines count, not raw ones");
        for _ in 0..max + 20 {
            f.key(key(KeyCode::Down));
        }
        assert_eq!(f.help, Some((at, max)), "the last wrapped line is reached and no further");
        let d = draw(&f);
        let wheel = MouseEvent { kind: MouseEventKind::ScrollDown, column: 5, row: 5, modifiers: KeyModifiers::NONE };
        f.mouse(wheel, screen, &d);
        assert_eq!(f.help, Some((at, max)), "the wheel stops at the end");
    }

    #[test]
    fn the_window_keeps_its_size_and_a_click_picks_a_tab_a_chip_or_a_row() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        f.add(vec![pane(1, 0, "claude", AgentState::Working), pane(2, 0, "bun", AgentState::Idle), command("Wrap long lines")]);
        let r = f.rect(screen);
        f.query = "zzz".into();
        assert_eq!(f.rect(screen), r, "typing does not move it");
        f.query.clear();
        let click = |f: &mut Find, (x, y): (u16, u16)| {
            let mut buf = Buffer::empty(screen);
            let d = f.draw(&mut buf, screen, &Theme::dark());
            f.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }, screen, &d)
        };
        let drawn = |f: &Find| {
            let mut buf = Buffer::empty(screen);
            f.draw(&mut buf, screen, &Theme::dark())
        };
        let at = |d: &Drawn, c: Click| d.hits.iter().find(|(_, h)| *h == c).map(|(r, _)| (r.x, r.y)).unwrap_or_else(|| panic!("{c:?} drawn"));
        let d = drawn(&f);
        click(&mut f, at(&d, Click::Tab(Some(Group::Panes))));
        assert_eq!(f.tab, Some(Group::Panes));
        let d = drawn(&f);
        click(&mut f, at(&d, Click::Chip(Chip::Idle)));
        assert_eq!(listed(&f), ["^s0", "bun"]);
        let d = drawn(&f);
        assert!(matches!(click(&mut f, at(&d, Click::Row(1))), Outcome::Act(Target::Pane(2), _)));
        assert!(matches!(click(&mut f, (0, 0)), Outcome::Close), "a click outside closes");
        let b = crate::chrome::close_rect(r);
        assert!(matches!(click(&mut f, (b.x, b.y)), Outcome::Close));
    }
}
