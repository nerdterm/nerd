//! Main menu > Search…: one field over panes, menu commands, settings, plugins, files, help and pane text.
//! The window only finds; each result acts through the code its origin uses, in `app::find`.

use std::cell::Cell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};

use crate::layout::PaneId;
use crate::render::Theme;
use crate::term::AgentState;

/// Rows a group shows under All before "N more in <group>".
const CAP: usize = 5;
/// The window's width inside its frame: the tabs fit on one line.
const WIDTH: u16 = 64;
/// Result rows shown at most; more scroll.
const ROWS: u16 = 14;
/// Bytes of a name, place or value kept to show: a pane can print a line of megabytes.
const SHOWN: usize = 512;

/// A field as a row shows it: invisible characters dropped, at most `SHOWN` bytes, cut between characters.
pub fn shown(s: &str) -> String {
    let at = (0..=SHOWN.min(s.len())).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    crate::render::unspoofed(&s[..at])
}

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
    /// A line of a pane's text: its row from the top of the live screen (history is negative), what the pane search
    /// looks for there (the phrase when the line has it, else the first word), and the line, trimmed, as `shown` keeps it.
    Line(PaneId, isize, String, String),
}

pub struct Hit {
    pub group: Group,
    pub name: String,
    pub place: String,
    pub value: String,
    /// A greyed menu item: shown, does nothing.
    pub enabled: bool,
    pub target: Target,
    /// A pane's state.
    state: Option<AgentState>,
    extra: String,
    name_lc: String,
    hay: String,
}

impl Hit {
    pub fn new(group: Group, name: impl AsRef<str>, place: impl AsRef<str>, target: Target) -> Self {
        Self {
            group,
            name: shown(name.as_ref()),
            place: shown(place.as_ref()),
            value: String::new(),
            enabled: true,
            target,
            state: None,
            extra: String::new(),
            name_lc: String::new(),
            hay: String::new(),
        }
    }

    pub fn value(mut self, v: impl AsRef<str>) -> Self {
        self.value = shown(v.as_ref());
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Words it is also found by, never shown.
    pub fn extra(mut self, words: impl AsRef<str>) -> Self {
        self.extra = words.as_ref().to_string();
        self
    }

    pub fn state(mut self, state: AgentState) -> Self {
        self.state = Some(state);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Row {
    Head(Group),
    Hit(usize),
    More(Group, usize),
    /// "Search pane text for …": the text is read only on this.
    AskText,
    Note(String),
}

impl Row {
    fn acts(&self) -> bool {
        matches!(self, Row::Hit(_) | Row::More(..) | Row::AskText)
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
    /// The first result row in view: it moves only to keep the chosen row in view, never under the pointer.
    scroll: Cell<usize>,
    /// Plugins and Help were read; Files were walked. Once per window, on the first query that needs them.
    pub lazy: bool,
    pub files: bool,
    pub files_note: Option<String>,
    /// The query the Text hits were found for, and how far the search got when it stopped early.
    text: Option<String>,
    pub text_note: Option<String>,
    /// What the last result did while the window stayed, red when refused; a notice card would sit under the window.
    pub note: Option<(bool, String)>,
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
    Key { code: KeyCode::Up, label: "↑↓", word: "", what: "up and down the results", act: |f| { f.step(-1, true); Outcome::None } },
    Key { code: KeyCode::Down, label: "", word: "", what: "up and down the results", act: |f| { f.step(1, true); Outcome::None } },
    Key { code: KeyCode::Enter, label: "enter", word: "open", what: "go to, run, change or open the chosen result", act: |f| f.act(f.selected) },
    Key { code: KeyCode::Tab, label: "tab", word: "next group", what: "the next group; with shift, the one before", act: |f| { f.cycle(1); Outcome::None } },
    Key { code: KeyCode::BackTab, label: "", word: "", what: "the next group; with shift, the one before", act: |f| { f.cycle(-1); Outcome::None } },
    Key { code: KeyCode::Esc, label: "esc", word: "close", what: "close", act: |_| Outcome::Close },
];

/// The keys of a help page in the window: its field takes none.
pub const HELP_KEYS: &[Key] = &[
    Key { code: KeyCode::Char('o'), label: "o", word: "open on nerdterm.dev", what: "open the page on nerdterm.dev", act: |f| f.page().map_or(Outcome::None, Outcome::Open) },
    Key { code: KeyCode::Up, label: "↑↓", word: "", what: "scroll the page", act: |f| { f.scroll(-1); Outcome::None } },
    Key { code: KeyCode::Down, label: "", word: "", what: "scroll the page", act: |f| { f.scroll(1); Outcome::None } },
    Key { code: KeyCode::Esc, label: "esc", word: "back", what: "back to the results", act: |f| { f.help = None; Outcome::None } },
];

/// Its label in the close button's place while a help page shows.
const BACK: &str = " [Esc] back ";

impl Find {
    pub fn new() -> Self {
        Self { query: String::new(), tab: None, chip: Chip::All, hits: Vec::new(), selected: 0, help: None, help_max: Cell::new(0), scroll: Cell::new(0), lazy: false, files: false, files_note: None, text: None, text_note: None, note: None }
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

    /// Nothing is read before a letter is typed: nothing is listed then.
    pub fn wants_lazy(&self) -> bool {
        !self.lazy && !self.words().is_empty()
    }

    /// Two letters, or one on the Files tab: one letter elsewhere is too little to walk a folder for.
    pub fn wants_files(&self) -> bool {
        !self.files && (self.query.trim().chars().count() >= 2 || (self.tab == Some(Group::Files) && !self.words().is_empty()))
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

    /// As high as its rows, the top where the tallest list would put it, so typing only moves the bottom; a help page
    /// takes that tallest size.
    pub fn rect(&self, screen: Rect) -> Rect {
        self.rect_n(screen, self.rows().len())
    }

    /// The window for `n` result rows.
    fn rect_n(&self, screen: Rect, n: usize) -> Rect {
        let w = crate::chrome::fit(screen, WIDTH, 0).inner(crate::chrome::DIALOG_FRAME).width;
        let strip = crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), w as usize).len() as u16;
        // the field's box, the tabs, a blank row over the strip
        let fixed = 3 + lines(&tab_widths(), 2, w) + 1 + strip;
        let chips = if self.tab == Some(Group::Panes) { lines(&chip_widths(), 1, w) } else { 0 };
        let list = |n: u16| if n == 0 { 0 } else { 1 + n.min(ROWS) };
        let full = crate::chrome::fit(screen, WIDTH, fixed + lines(&chip_widths(), 1, w) + list(ROWS));
        if self.help.is_some() {
            return full;
        }
        let note = self.note.as_ref().map_or(0, |(_, t)| crate::menu::wrap(t, w as usize).len() as u16);
        let r = crate::chrome::fit(screen, WIDTH, fixed + chips + note + list(n.min(ROWS as usize) as u16));
        Rect { y: full.y, ..r }
    }

    /// The button in the close button's place: back from a help page, close otherwise.
    pub fn button(&self, screen: Rect) -> Rect {
        crate::chrome::button_rect(self.rect(screen), if self.help.is_some() { BACK } else { crate::chrome::CLOSE })
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
        let chip = self.chip();
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
        // Nothing before a letter is typed: the sidebar already lists the panes. A state chip lists its panes.
        if words.is_empty() {
            if self.chip() == Chip::All {
                return Vec::new();
            }
            let rows: Vec<Row> = (0..self.hits.len()).filter(|&i| self.hits[i].state.is_some_and(|s| self.chip.fits(s))).map(Row::Hit).collect();
            return if rows.is_empty() { vec![note("No pane is in this state.")] } else { rows };
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
                out.extend(self.text_note.iter().flat_map(|t| notes(t)));
                // The panes print on: the same query can be searched again.
                out.push(Row::AskText);
            }
            Some(Group::Text) => out.push(Row::AskText),
            Some(g) => {
                out.extend(self.matches(g, &q, &words).into_iter().map(Row::Hit));
                if out.is_empty() {
                    out.push(note("Nothing found."));
                }
                if g == Group::Files {
                    out.extend(self.files_note.iter().flat_map(|t| notes(t)));
                }
            }
        }
        out
    }

    /// The state filter in force: the chips are only on the Panes tab.
    fn chip(&self) -> Chip {
        if self.tab == Some(Group::Panes) { self.chip } else { Chip::All }
    }

    /// Keeps the choice on a row that acts; returns how many rows there are.
    fn clamp(&mut self) -> usize {
        let rows = self.rows();
        if !rows.get(self.selected).is_some_and(Row::acts) {
            self.selected = rows.iter().position(Row::acts).unwrap_or(0);
        }
        rows.len()
    }

    fn typed(&mut self) {
        self.selected = 0;
        self.scroll.set(0);
        let t = std::time::Instant::now();
        let n = self.clamp();
        crate::log::debug(&format!("search: {n} rows for a {}-character query in {} µs", self.query.chars().count(), t.elapsed().as_micros()));
    }

    /// A paste's line into the field, as typing it would; a help page's field takes none.
    pub fn paste(&mut self, line: &str) {
        if self.help.is_none() && !line.is_empty() {
            self.query.push_str(line);
            self.note = None;
            self.typed();
        }
    }

    /// The chosen row, by its index into the rows.
    pub fn chosen(&self) -> usize {
        self.selected
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

    /// The arrows wrap around the list; the wheel stops at its ends.
    fn step(&mut self, d: isize, wrap: bool) {
        let acting: Vec<usize> = self.rows().iter().enumerate().filter(|(_, r)| r.acts()).map(|(i, _)| i).collect();
        if acting.is_empty() {
            return;
        }
        let at = acting.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        let n = acting.len() as isize;
        self.selected = acting[if wrap { (at + d).rem_euclid(n) } else { (at + d).clamp(0, n - 1) } as usize];
    }

    fn scroll(&mut self, d: isize) {
        let max = self.help_max.get();
        if let Some((_, s)) = self.help.as_mut() {
            *s = (*s).min(max).saturating_add_signed(d).min(max);
        }
    }

    fn act(&mut self, i: usize) -> Outcome {
        match self.rows().get(i).cloned() {
            Some(Row::Hit(h)) => {
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
                        let what = match hit.group {
                            Group::Commands => hit.place.clone(),
                            Group::Text => format!("Pane {}", hit.place),
                            _ => hit.name.clone(),
                        };
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
        let chord = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.help.is_some() {
            let code = crate::keys::normalize(k).code;
            return HELP_KEYS.iter().find(|r| r.code == code && !(chord && matches!(code, KeyCode::Char(_)))).map_or(Outcome::None, |r| (r.act)(self));
        }
        match k.code {
            KeyCode::Char(c) if !chord => {
                self.query.push(c);
                self.note = None;
                self.typed();
                Outcome::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.note = None;
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
            // Only rows that act are drawn as `Click::Row`.
            Moved => {
                if let Some(Click::Row(i)) = hit {
                    self.selected = i;
                }
                Outcome::None
            }
            Down(MouseButton::Left) => {
                let r = self.rect(screen);
                if !r.contains(at) {
                    return Outcome::Close;
                }
                if self.button(screen).contains(at) {
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
                    Some(Click::Row(i)) => {
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
                if self.help.is_some() { self.scroll(d) } else { self.step(d, false) }
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    /// Where the typing goes, while the field takes keys.
    pub fn cursor(&self, screen: Rect) -> Option<(u16, u16)> {
        if self.help.is_some() {
            return None;
        }
        // The field sits at the top, which the window keeps whatever its height.
        let inner = self.rect_n(screen, 0).inner(crate::chrome::DIALOG_FRAME);
        let room = inner.width.saturating_sub(5);
        let col = (crate::slots::cells(tail(&self.query, room as usize)) as u16).min(room);
        Some((inner.x + 2 + col, inner.y + 1))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn {
        let mut d = Drawn::default();
        let rows = self.rows();
        let r = self.rect_n(screen, rows.len()).intersection(buf.area);
        if r.is_empty() {
            return d;
        }
        crate::chrome::dialog(buf, r, Some("Search"), theme.border_focus, theme, crate::chrome::Moat::All);
        let inner = crate::chrome::button(buf, r, if self.help.is_some() { BACK } else { crate::chrome::CLOSE }, theme);
        if inner.width == 0 || inner.height == 0 {
            return d;
        }
        match self.help {
            Some((s, scroll)) => self.draw_help(buf, inner, s, scroll, theme, &mut d),
            None => self.draw_list(buf, inner, &rows, theme, &mut d),
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
        bottom = self.draw_note(buf, inner, bottom, inner.y + 2, theme);
        let lines: Vec<String> = sec.body.lines().flat_map(|l| if l.is_empty() { vec![String::new()] } else { crate::menu::wrap(l, w) }).collect();
        let rows = bottom.saturating_sub(1).saturating_sub(inner.y + 2) as usize;
        self.help_max.set(lines.len().saturating_sub(rows));
        for (y, line) in (inner.y + 2..bottom.saturating_sub(1)).zip(lines.iter().skip(scroll.min(self.help_max.get()))) {
            buf.set_stringn(inner.x, y, line, w, Style::default().fg(theme.text));
        }
    }

    fn draw_list(&self, buf: &mut Buffer, inner: Rect, rows: &[Row], theme: &Theme, d: &mut Drawn) {
        let w = inner.width as usize;
        let mut y = inner.y;
        // The field in a box of its own: its frame is what makes it stand out.
        let field = Rect::new(inner.x, y, inner.width, 3.min(inner.height));
        Block::bordered().style(Style::default().fg(theme.accent)).render(field, buf);
        let room = w.saturating_sub(4);
        if self.query.is_empty() {
            buf.set_stringn(inner.x + 2, y + 1, "Type to search", room, Style::default().fg(theme.faint));
        } else {
            // The end of the query: the cursor sits after it.
            buf.set_stringn(inner.x + 2, y + 1, tail(&self.query, room.saturating_sub(1)), room, Style::default().fg(theme.text));
        }
        y += 3;
        // The tabs, then the Panes tab's chips: words and chips to click, a key for each only as Tab; what does not fit
        // goes on the next line.
        let tabs: Vec<Option<Group>> = std::iter::once(None).chain(GROUPS.map(Some)).collect();
        for (t, (x, line)) in tabs.iter().zip(flow(&tab_widths(), 2, inner.width)) {
            let (word, at) = (t.map_or("All", Group::name), Rect::new(inner.x + x, y + line, 0, 1));
            if at.y >= inner.bottom() {
                break;
            }
            let style = if *t == self.tab { theme.chosen() } else { Style::default().fg(theme.muted) };
            let n = (word.chars().count() as u16).min(inner.width);
            buf.set_stringn(at.x, at.y, word, n as usize, style);
            d.hits.push((Rect { width: n, ..at }, Click::Tab(*t)));
        }
        y += lines(&tab_widths(), 2, inner.width);
        if self.tab == Some(Group::Panes) {
            for (c, (x, line)) in CHIPS.into_iter().zip(flow(&chip_widths(), 1, inner.width)) {
                if y + line >= inner.bottom() {
                    break;
                }
                let rect = crate::chrome::draw_chip(buf, inner.x + x, y + line, c.name(), &[], c == self.chip, false, theme);
                d.hits.push((rect, Click::Chip(c)));
            }
            y += lines(&chip_widths(), 1, inner.width);
        }
        let strip = crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), w);
        let bottom = crate::chrome::draw_hint(buf, inner, &strip, theme).max(y);
        let bottom = self.draw_note(buf, inner, bottom, y, theme);

        // a blank row over the results and under them
        let (top, bottom) = (y + 1, bottom.saturating_sub(1));
        let visible = bottom.saturating_sub(top) as usize;
        let skip = self.scroll.get().min(self.selected).max((self.selected + 1).saturating_sub(visible));
        self.scroll.set(skip);
        for ((i, row), y) in rows.iter().enumerate().skip(skip).zip(top..bottom) {
            let line = Rect::new(inner.x, y, inner.width, 1);
            match row {
                Row::Head(g) => {
                    buf.set_stringn(inner.x, y, g.name(), w, Style::default().fg(theme.muted));
                }
                Row::Hit(h) => self.draw_hit(buf, Rect { x: line.x + 2, width: line.width.saturating_sub(2), ..line }, &self.hits[*h], theme),
                Row::More(g, n) => {
                    buf.set_stringn(inner.x + 2, y, format!("{n} more in {}", g.name()), w.saturating_sub(2), Style::default().fg(theme.muted));
                }
                Row::AskText => {
                    buf.set_stringn(inner.x + 2, y, format!("Search pane text for \"{}\"", self.query.trim()), w.saturating_sub(2), Style::default().fg(theme.text));
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
        }
    }

    /// One result on one line: its name on the left, one faint value on the right.
    fn draw_hit(&self, buf: &mut Buffer, line: Rect, hit: &Hit, theme: &Theme) {
        let faint = Style::default().fg(theme.faint);
        let w = line.width as usize;
        let value = match hit.group {
            Group::Panes | Group::Text => hit.place.clone(),
            Group::Files => fit_left(&hit.place, w / 2),
            Group::Help => String::new(),
            _ => hit.value.clone(),
        };
        let vw = crate::slots::cells(&value).min(w / 2);
        if vw > 0 {
            buf.set_stringn(line.right() - vw as u16, line.y, crate::slots::fit(&value, vw), vw, faint);
        }
        let room = w.saturating_sub(vw + 2);
        let mut x = line.x;
        if let Some(state) = hit.state {
            buf.set_stringn(x, line.y, state.mark().to_string(), room, Style::default().fg(crate::sidebar::state_color(state, theme)));
            x += 2;
        }
        let room = room.saturating_sub((x - line.x) as usize);
        let name = crate::slots::fit(&hit.name, room);
        let style = if hit.enabled { Style::default().fg(theme.text) } else { faint };
        buf.set_stringn(x, line.y, &name, room, style);
        // Two commands of one label are told apart by where they act.
        if hit.group == Group::Commands && self.hits.iter().filter(|h| h.group == Group::Commands && h.name == hit.name).count() > 1 {
            let used = crate::slots::cells(&name) + 2;
            if used < room {
                buf.set_stringn(x + used as u16, line.y, &hit.place, room - used, faint);
            }
        }
    }

    /// The note, wrapped, on the rows above `bottom` and no higher than `top`; returns where it starts.
    fn draw_note(&self, buf: &mut Buffer, inner: Rect, bottom: u16, top: u16, theme: &Theme) -> u16 {
        let Some((bad, text)) = &self.note else { return bottom };
        let lines = crate::menu::wrap(text, inner.width as usize);
        let start = bottom.saturating_sub(lines.len() as u16).max(top);
        let style = Style::default().fg(if *bad { theme.bad } else { theme.faint });
        for (y, line) in (start..bottom).zip(&lines) {
            buf.set_stringn(inner.x, y, line, inner.width as usize, style);
        }
        start
    }

}

/// The tabs' widths, in their order.
fn tab_widths() -> Vec<u16> {
    std::iter::once("All").chain(GROUPS.map(Group::name)).map(|w| w.chars().count() as u16).collect()
}

fn chip_widths() -> Vec<u16> {
    CHIPS.map(|c| crate::chrome::chip_width(c.name(), &[])).to_vec()
}

/// Where each of `widths` goes on lines `w` wide, `gap` apart: its column and line.
fn flow(widths: &[u16], gap: u16, w: u16) -> Vec<(u16, u16)> {
    let (mut x, mut line) = (0, 0);
    widths
        .iter()
        .map(|&n| {
            if x > 0 && x + n.min(w) > w {
                (x, line) = (0, line + 1);
            }
            let at = (x, line);
            x += n.min(w) + gap;
            at
        })
        .collect()
}

fn lines(widths: &[u16], gap: u16, w: u16) -> u16 {
    flow(widths, gap, w).last().map_or(0, |(_, l)| l + 1)
}

/// The end of `s` in at most `room` cells, cut between graphemes.
fn tail(s: &str, room: usize) -> &str {
    let mut over = crate::slots::cells(s).saturating_sub(room);
    if over == 0 {
        return s;
    }
    let mut at = s.len();
    crate::slots::graphemes(s, |i, _, w| {
        if over == 0 {
            at = i;
            return false;
        }
        over = over.saturating_sub(w);
        true
    });
    &s[at..]
}

/// The end of `s` in `room` cells, "…" in front when cut: a folder's own name is at its end.
fn fit_left(s: &str, room: usize) -> String {
    if crate::slots::cells(s) <= room {
        return s.to_string();
    }
    format!("…{}", tail(s, room.saturating_sub(1)))
}

/// A note in rows of the window's width.
fn notes(text: &str) -> impl Iterator<Item = Row> {
    crate::menu::wrap(text, WIDTH as usize - 2).into_iter().map(Row::Note)
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
        Hit::new(Group::Panes, name, format!("s{space} › 1"), Target::Pane(id)).extra(state_word(state)).state(state)
    }

    /// The names the window lists now, with "Head" rows as `#Group` and more rows as `+N`.
    fn listed(f: &Find) -> Vec<String> {
        f.rows()
            .iter()
            .map(|r| match r {
                Row::Head(g) => format!("#{}", g.name()),
                Row::Hit(h) => f.hits[*h].name.clone(),
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
        assert!(listed(&f).is_empty(), "no query: nothing, the sidebar lists the panes");
        typed(&mut f, "blocked");
        assert_eq!(listed(&f), ["#Panes", "bun", "codex", "?text"], "a failed turn waits for you too");
        f.query.clear();
        f.set_tab(Some(Group::Panes));
        assert!(listed(&f).is_empty(), "nor the Panes tab");
        f.chip = Chip::Working;
        assert_eq!(listed(&f), ["claude"], "a state chip lists its panes");
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
        f.set_text(vec![Hit::new(Group::Text, "error: no such file", "claude", Target::Line(1, -3, "error".into(), "error: no such file".into()))], Some("searched 1 of 2 panes".into()));
        assert_eq!(f.tab, Some(Group::Text));
        assert_eq!(listed(&f), ["error: no such file", "(searched 1 of 2 panes)", "?text"], "the same query can be searched again");
        assert!(matches!(f.key(key(KeyCode::Enter)), Outcome::Act(Target::Line(1, -3, ..), ref n) if n == "Pane claude"));
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
    fn the_window_is_compact_grows_downward_with_its_rows_and_a_click_picks_a_tab_a_chip_or_a_row() {
        let screen = Rect::new(0, 0, 160, 45);
        let mut f = Find::new();
        f.add(vec![pane(1, 0, "claude", AgentState::Working), pane(2, 0, "bun", AgentState::Idle), command("Wrap long lines")]);
        f.add((0..30).map(|i| command(&format!("x{i}"))).collect());
        let r = f.rect(screen);
        assert_eq!(r.width, WIDTH + 2 * crate::chrome::DIALOG_FRAME.horizontal, "not the whole screen");
        typed(&mut f, "wrap");
        let one = f.rect(screen);
        assert!(one.y == r.y && one.height > r.height, "one row: the top stays, the bottom moves");
        f.query = "x".into();
        f.set_tab(Some(Group::Commands));
        let many = f.rect(screen);
        // "wrap" lists a header, a row and the pane text row
        assert!(many.y == r.y && many.height == one.height + ROWS - 3, "at most {ROWS} rows: {many:?} {one:?}");
        f.query.clear();
        f.set_tab(None);
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
        assert_eq!(listed(&f), ["bun"]);
        let d = drawn(&f);
        assert!(matches!(click(&mut f, at(&d, Click::Row(0))), Outcome::Act(Target::Pane(2), _)));
        assert!(matches!(click(&mut f, (0, 0)), Outcome::Close), "a click outside closes");
        let b = crate::chrome::close_rect(f.rect(screen));
        assert!(matches!(click(&mut f, (b.x, b.y)), Outcome::Close));
    }

    fn text_of(buf: &Buffer) -> String {
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect()
    }

    #[test]
    fn the_wheel_stops_at_the_ends_and_a_row_scrolled_to_is_clicked_as_drawn() {
        let screen = Rect::new(0, 0, 120, 20);
        let mut f = Find::new();
        f.add((0..50).map(|i| pane(i, 0, &format!("p{i}"), AgentState::Idle)).collect());
        f.set_tab(Some(Group::Panes));
        typed(&mut f, "p");
        let wheel = |f: &mut Find, kind| {
            let d = f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
            f.mouse(MouseEvent { kind, column: 5, row: 5, modifiers: KeyModifiers::NONE }, screen, &d);
        };
        wheel(&mut f, MouseEventKind::ScrollUp);
        assert_eq!(chosen(&f), "p0", "no wrap to the end");
        for _ in 0..20 {
            wheel(&mut f, MouseEventKind::ScrollDown);
        }
        assert_eq!(chosen(&f), "p49", "no wrap to the top");
        let d = f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        let (r, i) = d.hits.iter().find_map(|(r, c)| match c { Click::Row(i) if *i + 3 == f.selected => Some((*r, *i)), _ => None }).expect("rows above the chosen one are drawn");
        let out = f.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }, screen, &d);
        assert!(matches!(out, Outcome::Act(Target::Pane(id), _) if id == 46), "{}", listed(&f)[i]);
    }

    #[test]
    fn on_a_narrow_window_the_tabs_and_chips_go_on_more_lines_and_each_is_clicked() {
        let screen = Rect::new(0, 0, 60, 30);
        let mut f = Find::new();
        let d = f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        let tabs: Vec<(Rect, Option<Group>)> = d.hits.iter().filter_map(|(r, c)| if let Click::Tab(t) = c { Some((*r, *t)) } else { None }).collect();
        assert_eq!(tabs.len(), GROUPS.len() + 1, "every tab is drawn");
        assert!(tabs.last().unwrap().0.y > tabs[0].0.y, "on a second line");
        let (r, t) = *tabs.last().unwrap();
        f.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }, screen, &d);
        assert_eq!(f.tab, t);

        let screen = Rect::new(0, 0, 36, 30);
        f.set_tab(Some(Group::Panes));
        let d = f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        let chips: Vec<(Rect, Chip)> = d.hits.iter().filter_map(|(r, c)| if let Click::Chip(c) = c { Some((*r, *c)) } else { None }).collect();
        assert_eq!(chips.len(), CHIPS.len(), "the chips too");
        assert!(chips.last().unwrap().0.y > chips[0].0.y);
        let (r, c) = *chips.last().unwrap();
        f.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }, screen, &d);
        assert_eq!(f.chip, c);
    }

    #[test]
    fn the_note_is_drawn_over_the_key_strip_until_the_query_changes_and_a_paste_types_its_line() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        f.note = Some((true, "Pane planner is closed".into()));
        let mut buf = Buffer::empty(screen);
        f.draw(&mut buf, screen, &Theme::dark());
        assert!(text_of(&buf).contains("Pane planner is closed"));
        f.paste("needle");
        assert_eq!((f.query.as_str(), &f.note), ("needle", &None));
        f.help = Some((0, 0));
        f.note = Some((false, "opened".into()));
        let mut buf = Buffer::empty(screen);
        f.draw(&mut buf, screen, &Theme::dark());
        assert!(text_of(&buf).contains("opened"), "in a help page too");
        f.paste("more");
        assert_eq!(f.query, "needle", "a help page's field takes no paste");
    }

    #[test]
    fn a_result_is_one_plain_line_with_one_value_on_the_right() {
        let screen = Rect::new(0, 0, 160, 45);
        let mut f = Find::new();
        let deep = format!("{}/src", "very/deep".repeat(10));
        f.add(vec![
            Hit::new(Group::Commands, "Isolation for new panes…", "Space s", Target::Command(Action::NewTab, Ctx::Global)),
            Hit::new(Group::Commands, "Isolation for new panes…", "Tab 1", Target::Command(Action::NewTab, Ctx::Global)),
            Hit::new(Group::Files, "isolation.rs", &deep, Target::File(PathBuf::from("/x"))),
            Hit::new(Group::Help, "Isolation", "", Target::Help(0)).extra("isolation body text"),
        ]);
        f.paste("isolation");
        let mut buf = Buffer::empty(screen);
        f.draw(&mut buf, screen, &Theme::dark());
        let text = text_of(&buf);
        let row = |name: &str| text.lines().find(|l| l.contains(name)).unwrap_or_else(|| panic!("{name}: {text}")).to_string();
        let shared: Vec<&str> = text.lines().filter(|l| l.contains("Isolation for new panes…")).collect();
        assert!(shared.len() == 2 && shared[0].contains("Isolation for new panes…  Space s") && shared[1].contains("Isolation for new panes…  Tab 1"), "a shared label says where it acts: {shared:?}");
        let file = row("isolation.rs");
        assert!(file.contains("…") && file.trim_end().trim_end_matches('│').trim_end().ends_with("deep/src"), "the folder, cut from the left: {file}");
        assert!(!text.contains('"') || text.matches('"').count() == 2, "no quotes but the pane text row's: {text}");
    }

    #[test]
    fn hovering_a_scrolled_list_keeps_it_where_it_is() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        f.add((0..50).map(|i| pane(i, 0, &format!("p{i}"), AgentState::Idle)).collect());
        f.set_tab(Some(Group::Panes));
        typed(&mut f, "p");
        let ev = |kind, (x, y): (u16, u16)| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        let draw = |f: &Find| f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        for _ in 0..20 {
            let d = draw(&f);
            f.mouse(ev(MouseEventKind::ScrollDown, (5, 5)), screen, &d);
        }
        let d = draw(&f);
        let rows: Vec<(Rect, usize)> = d.hits.iter().filter_map(|(r, c)| if let Click::Row(i) = c { Some((*r, *i)) } else { None }).collect();
        let (top, first) = rows[0];
        assert!(first > 0, "scrolled");
        f.mouse(ev(MouseEventKind::Moved, (top.x + 1, top.y)), screen, &d);
        assert_eq!(f.chosen(), first, "the row under the pointer is chosen");
        let d = draw(&f);
        let again: Vec<(Rect, usize)> = d.hits.iter().filter_map(|(r, c)| if let Click::Row(i) = c { Some((*r, *i)) } else { None }).collect();
        assert_eq!(again, rows, "and the list did not move under it");
        let out = f.mouse(ev(MouseEventKind::Down(MouseButton::Left), (top.x + 1, top.y)), screen, &d);
        assert!(matches!(out, Outcome::Act(Target::Pane(id), _) if id == first), "the click acts on it");
    }

    #[test]
    fn nothing_is_read_or_walked_before_a_letter_and_the_list_height_is_capped() {
        let mut f = Find::new();
        for t in [Group::Plugins, Group::Help, Group::Files] {
            f.set_tab(Some(t));
            assert!(!f.wants_lazy() && !f.wants_files(), "{t:?}");
        }
        typed(&mut f, "a");
        assert!(f.wants_lazy() && f.wants_files(), "one letter on the Files tab walks");
        let screen = Rect::new(0, 0, 160, 45);
        assert_eq!(f.rect_n(screen, 65_536), f.rect_n(screen, ROWS as usize), "no wrap past u16");
    }

    #[test]
    fn a_short_or_narrow_screen_draws_without_a_panic() {
        let mut f = Find::new();
        f.add((0..30).map(|i| pane(i, 0, &format!("p{i}"), AgentState::Idle)).collect());
        f.note = Some((true, "a note long enough to wrap onto more than one line of a narrow window".into()));
        typed(&mut f, "p");
        for tab in [None, Some(Group::Panes)] {
            f.set_tab(tab);
            for (w, h) in [(68, 8), (68, 3), (10, 5), (1, 1), (0, 0)] {
                let screen = Rect::new(0, 0, w, h);
                f.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
                f.cursor(screen);
            }
        }
    }

    #[test]
    fn the_cursor_stays_in_the_field_after_emoji_sequences() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        f.paste(&"\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".repeat(100));
        let inner = f.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let (x, y) = f.cursor(screen).unwrap();
        assert!(x < inner.right() - 2 && y == inner.y + 1, "({x}, {y}) inside {inner:?}");
    }

    #[test]
    fn a_long_wide_query_keeps_its_end_and_the_cursor_in_the_field() {
        let screen = Rect::new(0, 0, 120, 40);
        let mut f = Find::new();
        f.paste(&"界".repeat(40_000));
        let inner = f.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let shown = (inner.width - 5) / 2;
        assert_eq!(f.cursor(screen), Some((inner.x + 2 + 2 * shown, inner.y + 1)), "in the field's box, right after the last one drawn");
        let mut buf = Buffer::empty(screen);
        f.draw(&mut buf, screen, &Theme::dark());
        let field: String = (inner.x..inner.right()).map(|x| buf[(x, inner.y + 1)].symbol()).collect();
        assert_eq!(field.matches('界').count(), shown as usize, "as many as fit before the cursor");
    }
}
