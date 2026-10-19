//! The Messages window: the matrix of who may write to whom, the mail waiting for the owner, today's threads; and the routes map.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{Card, MailAction, MailView};
use crate::layout::PaneId;
use crate::mail::{Hold, Message, Party, State};
use crate::policy::{Action, Mode, Scope, Verdict};
use crate::render::Theme;

/// One line of a window: runs of text, each with its style and what a click on it does.
pub type Line<H> = Vec<(String, Style, Option<H>)>;

/// What the last frame drew: the clickable runs, the window, its close button, and the lines against the room.
pub struct Drawn<H> {
    pub hits: Vec<(Rect, H)>,
    pub frame: Rect,
    pub close: Rect,
    pub rows: usize,
    pub room: usize,
}

impl<H> Default for Drawn<H> {
    fn default() -> Self {
        Drawn { hits: Vec::new(), frame: Rect::default(), close: Rect::default(), rows: 0, room: 0 }
    }
}

impl<H: Clone> Drawn<H> {
    pub fn at(&self, x: u16, y: u16) -> Option<H> {
        self.hits.iter().find(|(r, _)| r.contains((x, y).into())).map(|(_, h)| h.clone())
    }
}

fn cells(s: &str) -> usize {
    crate::slots::cells(s)
}

/// `s` in at most `max` characters, the last one `…` when cut.
pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(['…']).collect()
}

pub fn width<H>(l: &Line<H>) -> usize {
    l.iter().map(|(t, _, _)| cells(t)).sum()
}

/// The window `r` with `lines` from `scroll`, and `buttons` then `close` centred on its last row.
#[allow(clippy::too_many_arguments)]
pub fn draw<H: Clone>(buf: &mut Buffer, r: Rect, title: &str, theme: &Theme, lines: &[Line<H>], scroll: usize, buttons: &[(String, H)], close: &str) -> Drawn<H> {
    let mut d = Drawn::default();
    let r = r.intersection(buf.area);
    if r.is_empty() {
        return d;
    }
    let inner = crate::chrome::dialog(buf, r, Some(title), theme.border_focus, theme, crate::chrome::Moat::All);
    d.frame = r;
    if inner.height < 2 || inner.width == 0 {
        return d;
    }
    let room = inner.height.saturating_sub(2) as usize;
    let from = scroll.min(lines.len().saturating_sub(room));
    for (row, line) in lines.iter().skip(from).take(room).enumerate() {
        let y = inner.y + row as u16;
        let mut x = inner.x;
        for (text, style, hit) in line {
            if x >= inner.right() {
                break;
            }
            let end = buf.set_stringn(x, y, text, (inner.right() - x) as usize, *style).0;
            if let Some(h) = hit {
                d.hits.push((Rect::new(x, y, end - x, 1), h.clone()));
            }
            x = end;
        }
    }
    let ground = Style::default().bg(theme.ground);
    if lines.len() > room && room > 0 {
        let at = format!("lines {}–{} of {}", from + 1, from + room, lines.len());
        buf.set_stringn(inner.x, inner.bottom() - 2, at, inner.width as usize, ground.fg(theme.faint));
    }
    let bottom = inner.bottom() - 1;
    let total = buttons.iter().map(|(b, _)| cells(b) + 1).sum::<usize>() + cells(close);
    let mut x = inner.x + (inner.width as usize).saturating_sub(total) as u16 / 2;
    for (label, h) in buttons {
        let len = cells(label) as u16;
        if x + len > inner.right() {
            break;
        }
        buf.set_string(x, bottom, label, ground.fg(theme.text));
        d.hits.push((Rect::new(x, bottom, len, 1), h.clone()));
        x += len + 1;
    }
    let len = (cells(close) as u16).min(inner.right().saturating_sub(x));
    buf.set_stringn(x, bottom, close, len as usize, ground.fg(theme.text));
    d.close = Rect::new(x, bottom, len, 1);
    d.rows = lines.len();
    d.room = room;
    d
}

/// A window sized to its lines and its bottom row.
pub fn fit<H>(screen: Rect, lines: &[Line<H>], bottom: usize) -> Rect {
    let w = lines.iter().map(width).chain([bottom]).max().unwrap_or(0);
    crate::chrome::fit(screen, w as u16, lines.len() as u16)
}

pub const CLOSE: &str = " [Esc] close ";
/// Delivered into Claude Code's inbox is not yet seen by the session: its own setting decides.
const INBOX_NOTE: &str = "Claude Code's own crossSessionInbound setting may still hold it";

/// `[k] word`: the key in the accent, the word plainly, both clicking the same.
pub fn button<H: Clone>(l: &mut Line<H>, key: &str, word: &str, click: H, theme: &Theme) {
    let g = Style::default().bg(theme.ground);
    l.push((format!("[{key}]"), g.fg(theme.accent), Some(click.clone())));
    l.push((format!(" {word}"), g.fg(theme.text), Some(click)));
    l.push(("  ".into(), g, None));
}

/// A chip: the word on the surface, lit when chosen.
pub fn chip<H>(l: &mut Line<H>, word: &str, lit: Option<Style>, click: Option<H>, theme: &Theme) {
    let style = lit.unwrap_or(Style::default().bg(theme.surface).fg(theme.muted));
    l.push((format!(" {word} "), style, click));
    l.push((" ".into(), Style::default().bg(theme.ground), None));
}

pub fn text<H>(l: &mut Line<H>, s: impl Into<String>, style: Style) {
    l.push((s.into(), style, None));
}

#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// A card of the routes map: a press chooses it.
    Card(String),
    /// A route, by index: allow, ask, no, then removed.
    Line(usize),
    /// A row label of the matrix.
    Select(String),
    /// The matrix starts at this space group.
    Start(usize),
    /// The routes map moves by columns and lines.
    Pan(isize, isize),
    Topology,
    MapView,
    /// true: who can write to the chosen card.
    Incoming(bool),
    Act(MailAction),
    Reply(String),
    Broadcast,
    /// A waiting row: the keys act on it.
    Waiting(String),
    /// Every thread again, not one pane's.
    AllThreads,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
    Act(MailAction),
    Reply(String),
    Broadcast,
    Topology,
}

#[derive(Clone, Debug, Default)]
pub struct Messages {
    pub selected: Option<String>,
    /// The rows show who may write to the chosen card, not where it may write.
    pub incoming: bool,
    /// Only this pane's threads and waiting mail.
    pub inbox: Option<PaneId>,
    /// The waiting row the keys act on, by id; the first when none.
    pub at: Option<String>,
    /// The routes map fills the screen.
    pub map_view: bool,
    /// Opened from the menu, not from the window: leaving the map closes it.
    alone: bool,
    /// The first space group of the matrix when its columns do not all fit.
    start: usize,
    /// The routes map's first column and first line.
    pan: Cell<(usize, usize)>,
    scroll: Cell<usize>,
    /// `at` was scrolled into view once.
    shown: Cell<bool>,
}

const BOX_DASH: [[char; 3]; 2] = [['│', '┆', '┊'], ['─', '┄', '┈']];
const N: u8 = 1;
const E: u8 = 2;
const S: u8 = 4;
const W: u8 = 8;
/// A matrix column: a separator and eight cells, the widest being `held 99+`.
const CELL: usize = 9;
/// A card's height: the border, the name, the agent, the isolation, the border.
const CH: usize = 5;
/// The widest a card's isolation line grows, so one pane's long list does not widen every card.
const CARD_WORDS: usize = 44;
/// Between columns: the exit cell, six lanes, and the next column's exit cell.
const GX: usize = 8;
/// Between card rows: two lanes and the arrow row.
const GY: usize = 3;
/// Above the first card row: the headers, their arrow row and lane, two lanes and the arrow row.
const TOP: usize = 6;

#[derive(Clone)]
struct Spot {
    sym: char,
    bits: u8,
    dash: usize,
    style: Style,
    hit: Option<Click>,
}

/// The map as cells: text, and line pieces that join into box drawing.
struct Grid {
    w: usize,
    cells: Vec<Spot>,
}

impl Grid {
    fn new(w: usize, h: usize, theme: &Theme) -> Self {
        Grid { w, cells: vec![Spot { sym: ' ', bits: 0, dash: 0, style: Style::default().bg(theme.ground), hit: None }; w * h] }
    }

    fn cell(&mut self, x: usize, y: usize) -> Option<&mut Spot> {
        if x >= self.w {
            return None;
        }
        self.cells.get_mut(y * self.w + x)
    }

    fn put(&mut self, x: usize, y: usize, s: &str, style: Style, hit: Option<Click>) {
        for (i, ch) in s.chars().enumerate() {
            if let Some(c) = self.cell(x + i, y) {
                *c = Spot { sym: ch, bits: 0, dash: 0, style, hit: hit.clone() };
            }
        }
    }

    /// A line piece; never over text, which the cards and the labels are.
    fn seg(&mut self, x: usize, y: usize, bit: u8, dash: usize, style: Style, hit: &Click) {
        if let Some(c) = self.cell(x, y)
            && (c.sym == ' ' || c.bits != 0)
        {
            c.bits |= bit;
            c.dash = dash;
            c.style = style;
            c.hit = Some(hit.clone());
        }
    }

    /// Straight runs between the points, joined at the corners.
    fn path(&mut self, pts: &[(usize, usize)], dash: usize, style: Style, hit: &Click) {
        for w in pts.windows(2) {
            let ((mut x, mut y), (x1, y1)) = (w[0], w[1]);
            while (x, y) != (x1, y1) {
                let (nx, ny, out, back) = if x < x1 {
                    (x + 1, y, E, W)
                } else if x > x1 {
                    (x - 1, y, W, E)
                } else if y < y1 {
                    (x, y + 1, S, N)
                } else {
                    (x, y - 1, N, S)
                };
                self.seg(x, y, out, dash, style, hit);
                self.seg(nx, ny, back, dash, style, hit);
                (x, y) = (nx, ny);
            }
        }
    }

    fn lines(self) -> Vec<Line<Click>> {
        self.cells
            .chunks(self.w)
            .map(|row| {
                let mut l: Line<Click> = Vec::new();
                for c in row {
                    let sym = if c.bits == 0 { c.sym } else { boxed(c.bits, c.dash) };
                    match l.last_mut() {
                        Some((t, s, h)) if *s == c.style && *h == c.hit => t.push(sym),
                        _ => l.push((sym.to_string(), c.style, c.hit.clone())),
                    }
                }
                l
            })
            .collect()
    }
}

fn boxed(bits: u8, dash: usize) -> char {
    match bits {
        b if b & (E | W) == 0 => BOX_DASH[0][dash],
        b if b & (N | S) == 0 => BOX_DASH[1][dash],
        b if b == E | S => '┌',
        b if b == W | S => '┐',
        b if b == N | E => '└',
        b if b == N | W => '┘',
        b if b == N | E | S => '├',
        b if b == N | W | S => '┤',
        b if b == E | W | S => '┬',
        b if b == N | E | W => '┴',
        _ => '┼',
    }
}

/// The routes map: its lines, the counts with no room on their line, and a column's width.
struct Map {
    lines: Vec<Line<Click>>,
    legend: Vec<(String, Style, usize)>,
    colw: usize,
}

/// The line cut to the cells `from .. from + width`, each run keeping its style and click.
fn slice(line: &Line<Click>, from: usize, width: usize) -> Line<Click> {
    let mut x = 0;
    let mut out = Vec::new();
    for (t, style, hit) in line {
        let mut part = String::new();
        for ch in t.chars() {
            let n = cells(ch.encode_utf8(&mut [0; 4]));
            if x >= from && x + n <= from + width {
                part.push(ch);
            }
            x += n;
        }
        if !part.is_empty() {
            out.push((part, *style, hit.clone()));
        }
    }
    out
}

/// Where a route's end sits on the map: a card by column and row, or a space's header by column.
#[derive(Clone, Copy, PartialEq)]
enum End {
    Card(usize, usize),
    Head(usize),
}

fn scope_of(name: &str) -> Scope {
    if name == "you" { Scope::You } else { Scope::Pane(name.into()) }
}

fn age(m: &Message) -> String {
    crate::sidebar::ago(std::time::Duration::from_secs(crate::hook::now().saturating_sub(m.created)))
}

fn action_color(a: Action, theme: &Theme) -> ratatui::style::Color {
    match a {
        Action::Allow => theme.ok,
        Action::Ask => theme.warn,
        Action::No => theme.bad,
    }
}

impl Messages {
    pub fn new() -> Self {
        Self::default()
    }

    /// The routes map, full screen.
    pub fn routes_map() -> Self {
        Messages { map_view: true, alone: true, ..Self::default() }
    }

    /// Opened on a card, its rows in one direction.
    pub fn on(name: Option<String>, incoming: bool) -> Self {
        Messages { selected: name, incoming, ..Self::default() }
    }

    /// Opened on one waiting row.
    pub fn at(id: String) -> Self {
        Messages { at: Some(id), ..Self::default() }
    }

    /// One pane's threads and waiting mail.
    pub fn inbox(pane: PaneId, name: Option<String>) -> Self {
        Messages { inbox: Some(pane), selected: name, ..Self::default() }
    }

    fn mine(&self, m: &Message) -> bool {
        self.inbox.is_none_or(|p| m.to.pane() == Some(p) || m.from.pane() == Some(p))
    }

    /// The waiting rows shown, and the one the keys act on.
    fn waiting<'a>(&self, v: &'a MailView) -> (Vec<&'a Message>, Option<&'a Message>) {
        let rows: Vec<&Message> = v.waiting.iter().filter(|m| self.mine(m)).collect();
        let current = self.at.as_ref().and_then(|id| rows.iter().find(|m| m.id == *id).copied()).or(rows.first().copied());
        (rows, current)
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict) -> Drawn<Click> {
        if self.map_view {
            return self.draw_map(buf, screen, theme, v);
        }
        let r = crate::chrome::fit(screen, u16::MAX, u16::MAX);
        let w = r.width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let (lines, at) = self.lines(v, verdict, w, theme);
        if let Some(at) = at.filter(|_| !self.shown.replace(true)) {
            self.scroll.set(at.saturating_sub(1));
        }
        let d = draw(buf, r, "Messages", theme, &lines, self.scroll.get(), &[], CLOSE);
        self.scroll.set(self.scroll.get().min(d.rows.saturating_sub(d.room)));
        d
    }

    /// The routes map in the whole screen: the part of the grid from the pan, its position and the buttons that move it.
    fn draw_map(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, v: &MailView) -> Drawn<Click> {
        let g = Style::default().bg(theme.ground);
        // the top row is left to the logo, an image the terminal draws over any text
        let screen = Rect { y: screen.y + 1, height: screen.height.saturating_sub(1), ..screen };
        let w = screen.width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let room = screen.height.saturating_sub(2 * crate::chrome::DIALOG_FRAME.vertical + 2) as usize;
        let map = self.map(v, theme);
        let mut content = map.lines;
        // counts whose line has no straight run long enough to carry them
        if !map.legend.is_empty() {
            let mut l: Line<Click> = Vec::new();
            text(&mut l, "on the lines ", g.fg(theme.muted));
            for (label, style, i) in map.legend {
                chip(&mut l, &label, Some(style), Some(Click::Line(i)), theme);
            }
            content.push(l);
        }
        let cols = v.columns.len();
        let seen = (w / map.colw).clamp(1, cols.max(1));
        let rows = room.saturating_sub(1);
        let (px, py) = self.pan.get();
        let (px, py) = (px.min(cols.saturating_sub(seen)), py.min(content.len().saturating_sub(rows)));
        self.pan.set((px, py));
        let mut lines: Vec<Line<Click>> = content.iter().skip(py).take(rows).map(|l| slice(l, px * map.colw, w)).collect();
        lines.resize(rows, Vec::new());
        let last = (py + rows).min(content.len());
        lines.push(vec![(format!("columns {}–{} of {cols} · lines {}–{last} of {}", px + 1, px + seen, py + 1, content.len()), g.fg(theme.faint), None)]);
        let buttons: Vec<(String, Click)> = [
            (px > 0, "[←] left", Click::Pan(-1, 0)),
            (px + seen < cols, "[→] right", Click::Pan(1, 0)),
            (py > 0, "[↑] up", Click::Pan(0, -3)),
            (py + rows < content.len(), "[↓] down", Click::Pan(0, 3)),
        ]
        .into_iter()
        .filter(|b| b.0)
        .map(|b| (b.1.to_string(), b.2))
        .collect();
        draw(buf, screen, "Routes map", theme, &lines, 0, &buttons, CLOSE)
    }

    /// Every line of the window for an inside `w` cells wide, and the line of the row it opened on.
    pub fn lines(&self, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, w: usize, theme: &Theme) -> (Vec<Line<Click>>, Option<usize>) {
        let g = Style::default().bg(theme.ground);
        let rule = || -> Line<Click> { vec![("─".repeat(w), g.fg(theme.border), None)] };
        let mut out: Vec<Line<Click>> = Vec::new();

        let mut top: Line<Click> = Vec::new();
        text(&mut top, "mode ", g.fg(theme.muted));
        for (m, word) in [(Mode::Open, "open"), (Mode::Strict, "strict")] {
            let lit = (v.mode == m).then(|| theme.cursor());
            chip(&mut top, word, lit, (v.mode != m).then_some(Click::Act(MailAction::SetMode(m))), theme);
        }
        if !v.on {
            text(&mut top, "  messaging off", g.fg(theme.warn));
        }
        out.push(top);
        let mut buttons: Line<Click> = Vec::new();
        button(&mut buttons, "b", "Broadcast…", Click::Broadcast, theme);
        button(&mut buttons, "t", "Set up a topology…", Click::Topology, theme);
        button(&mut buttons, "v", "Routes map…", Click::MapView, theme);
        out.push(buttons);
        let unrouted = match v.mode {
            Mode::Open => "no route: a pane may write to you and to its own account; to another account, you are asked",
            Mode::Strict => "no route: a pane may write to you only",
        };
        out.extend(crate::menu::wrap(unrouted, w).into_iter().map(|l| vec![(l, g.fg(theme.faint), None)]));
        if v.broken {
            out.push(vec![("policy.json does not parse: messaging off until a change here".into(), g.fg(theme.bad), None)]);
        }

        out.push(Vec::new());
        out.extend(self.matrix(v, verdict, w, theme));
        let pane_named = |n: &str| v.columns.iter().flat_map(|c| &c.cards).any(|c| c.pane.is_some() && c.name == n);
        let cell = |s: &Scope, to: bool| match s {
            Scope::Pane(n) => pane_named(n),
            Scope::You => to,
            _ => false,
        };
        let other: Vec<_> = v.routes.iter().filter(|r| !(cell(&r.route.from, false) && cell(&r.route.to, true) && r.route.from != r.route.to)).collect();
        if !other.is_empty() {
            let mut l: Line<Click> = Vec::new();
            text(&mut l, "routes ", g.fg(theme.muted));
            for r in other {
                let label = r.route.said();
                if width(&l) + cells(&label) + 3 > w {
                    out.push(std::mem::take(&mut l));
                    text(&mut l, "  ", g);
                }
                chip(&mut l, &label, Some(g.fg(action_color(r.route.action, theme))), Some(Click::Line(r.index)), theme);
            }
            out.push(l);
        }
        let risky: Vec<String> = v.routes.iter().filter(|r| r.risk).map(|r| format!("{} → {}", r.route.from.text(), r.route.to.text())).collect();
        if !risky.is_empty() {
            let risk = format!("risk: {}: an allow between a pane with network and one without", risky.join(", "));
            out.extend(crate::menu::wrap(&risk, w).into_iter().map(|l| vec![(l, g.fg(theme.bad), None)]));
        }
        if let Some(sel) = &self.selected {
            out.push(rule());
            out.extend(self.rows(sel, v, verdict, theme));
        }

        out.push(rule());
        out.push(vec![("waiting for you".into(), g.fg(theme.muted), None)]);
        let (rows, current) = self.waiting(v);
        let mut at = None;
        if rows.is_empty() {
            out.push(vec![("nothing waits".into(), g.fg(theme.faint), None)]);
        }
        for m in rows {
            let keys = current.is_some_and(|c| c.id == m.id);
            if keys && self.at.is_some() {
                at = Some(out.len());
            }
            out.extend(waiting_row(m, keys, theme));
        }

        out.push(rule());
        let mut head: Line<Click> = Vec::new();
        text(&mut head, "today", g.fg(theme.muted));
        if self.inbox.is_some() {
            text(&mut head, "  ", g);
            chip(&mut head, "every thread", None, Some(Click::AllThreads), theme);
        }
        out.push(head);
        let today: Vec<&Message> = v.today.iter().filter(|m| self.mine(m)).collect();
        if today.is_empty() {
            out.push(vec![("none yet".into(), g.fg(theme.faint), None)]);
        }
        for m in today {
            let via = m.via.map_or("-".to_string(), |v| format!("{v:?}").to_lowercase());
            let mut l: Line<Click> = vec![
                (format!("{} → {}", m.from.name(), m.to.name()), g.fg(theme.text), None),
                (format!("  {via}  {}  {}", m.state.words(), age(m)), g.fg(theme.muted), None),
            ];
            if m.via == Some(crate::mail::Via::Inbox) {
                text(&mut l, format!("  {INBOX_NOTE}"), g.fg(theme.faint));
            }
            out.push(l);
        }
        (out, at)
    }

    /// Who may write to whom: a row per sender, a column per recipient and `you`, each cell the verdict a message would get now.
    fn matrix(&self, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, w: usize, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let mut panes: Vec<&Card> = Vec::new();
        let mut groups: Vec<(&str, std::ops::Range<usize>)> = Vec::new();
        for col in &v.columns {
            let Some(space) = &col.space else { continue };
            let from = panes.len();
            panes.extend(col.cards.iter().filter(|c| c.pane.is_some()));
            if panes.len() > from {
                groups.push((space, from..panes.len()));
            }
        }
        if panes.is_empty() {
            return vec![vec![("no pane is named yet: name panes with Rename pane…".into(), g.fg(theme.faint), None)]];
        }
        let label = |i: usize| format!("{} {}", i + 1, panes[i].name);
        let lw = (0..panes.len()).map(|i| cells(&label(i))).max().unwrap_or(0);
        let cap = w.saturating_sub(lw) / CELL;
        if cap == 0 {
            return vec![vec![(format!("the matrix needs {} columns, this window has {w}: widen the window", lw + CELL), g.fg(theme.warn), None)]];
        }
        let start = if panes.len() < cap { 0 } else { self.start.min(groups.len() - 1) };
        let shown = if panes.len() < cap {
            0..panes.len()
        } else {
            let first = groups[start].1.start;
            let end = groups[start..].iter().map(|(_, r)| r.end).take_while(|end| end - first < cap).last();
            first..end.unwrap_or_else(|| (first + cap.saturating_sub(1)).min(groups[start].1.end))
        };
        let mut out: Vec<Line<Click>> = Vec::new();
        if shown.len() < panes.len() {
            let mut l: Line<Click> = Vec::new();
            let fit = if shown.is_empty() { "no recipient".to_string() } else { format!("recipients {}–{}", shown.start + 1, shown.end) };
            text(&mut l, format!("{fit} of {} fit this width; start at space: ", panes.len()), g.fg(theme.warn));
            for (i, (space, _)) in groups.iter().enumerate() {
                if width(&l) + cells(space) + 3 > w {
                    out.push(std::mem::take(&mut l));
                }
                let lit = (i == start).then(|| theme.cursor());
                chip(&mut l, space, lit, Some(Click::Start(i)), theme);
            }
            out.push(l);
        }
        // None is the `you` column
        let columns: Vec<Option<usize>> = shown.clone().map(Some).chain([None]).collect();
        let sep = |j: Option<usize>| match j {
            Some(j) => j != shown.start && groups.iter().any(|(_, r)| r.start == j),
            None => true,
        };
        let mut head: Line<Click> = vec![(" ".repeat(lw), g, None)];
        for j in &columns {
            text(&mut head, if sep(*j) { "│" } else { " " }, g.fg(theme.muted));
            text(&mut head, format!("{:<8}", j.map_or("you".to_string(), |j| (j + 1).to_string())), g.fg(theme.muted));
        }
        out.push(head);
        for (space, range) in &groups {
            out.push(vec![(format!("space {space}"), g.fg(theme.muted), None)]);
            for i in range.clone() {
                let name = &panes[i].name;
                let chosen = self.selected.as_deref() == Some(name.as_str());
                let style = if chosen { g.fg(theme.accent) } else { g.fg(theme.text) }.add_modifier(Modifier::BOLD);
                let mut l: Line<Click> = vec![(format!("{:<lw$}", label(i)), style, Some(Click::Select(name.clone())))];
                for j in &columns {
                    text(&mut l, if sep(*j) { "│" } else { " " }, g.fg(theme.muted));
                    let to = j.map_or("you", |j| panes[j].name.as_str());
                    if *j == Some(i) {
                        text(&mut l, format!("{:<8}", "—"), g.fg(theme.faint));
                        continue;
                    }
                    let waiting: Vec<&Message> = v.waiting.iter().filter(|m| m.from.name() == name && m.to.name() == to).collect();
                    if let Some(first) = waiting.first() {
                        let n = if waiting.len() > 99 { "99+".to_string() } else { waiting.len().to_string() };
                        l.push((format!("{:<8}", format!("held {n}")), g.fg(theme.warn), Some(Click::Waiting(first.id.clone()))));
                        continue;
                    }
                    let verdict = verdict(name, to);
                    let style = g.fg(action_color(verdict.action, theme)).add_modifier(if verdict.route.is_some() { Modifier::BOLD } else { Modifier::DIM });
                    let (from, to) = (scope_of(name), scope_of(to));
                    let own = v.routes.iter().find(|r| r.route.from == from && r.route.to == to && r.left != Some(0)).map(|r| &r.route);
                    let (action, until) = match own {
                        None => (Some(Action::Allow), None),
                        Some(r) => match r.action {
                            Action::Allow => (Some(Action::Ask), r.until),
                            Action::Ask => (Some(Action::No), r.until),
                            Action::No => (None, None),
                        },
                    };
                    l.push((format!("{:<8}", verdict.action.word()), style, Some(Click::Act(MailAction::SetRoute { from, to, action, until }))));
                }
                out.push(l);
            }
        }
        let legend = "bright: a route decides · faint: the mode · a reply in a thread passes unless a route says no";
        out.extend(crate::menu::wrap(legend, w).into_iter().map(|l| vec![(l, g.fg(theme.faint), None)]));
        out
    }

    /// The routes map: one column per space and the last, cards in them, routes in the gaps, and the counts that found no
    /// room on their line.
    fn map(&self, v: &MailView, theme: &Theme) -> Map {
        let g = Style::default().bg(theme.ground);
        let cw = v.columns.iter().flat_map(|c| &c.cards).map(card_width).max().unwrap_or(0).max(22);
        let cols = v.columns.len();
        let rows = v.columns.iter().map(|c| c.cards.len()).max().unwrap_or(1).max(1);
        let col_x = |c: usize| c * (cw + GX);
        let row_y = |r: usize| TOP + r * (CH + GY);
        let mut grid = Grid::new(cols * (cw + GX), TOP + rows * CH + (rows - 1) * GY, theme);

        let mut ends: Vec<(String, End)> = Vec::new();
        let mut sel_space = None;
        for (c, col) in v.columns.iter().enumerate() {
            match &col.space {
                Some(s) => {
                    let head = format!("space {s} ");
                    grid.put(col_x(c), 0, "space ", g.fg(theme.muted), None);
                    grid.put(col_x(c) + 6, 0, &format!("{s} "), g.fg(theme.text), None);
                    grid.put(col_x(c) + cells(&head), 0, &"─".repeat(cw.saturating_sub(cells(&head))), g.fg(theme.muted), None);
                    ends.push((format!("space:{s}"), End::Head(c)));
                }
                None => grid.put(col_x(c), 0, "outside spaces", g.fg(theme.muted), None),
            }
            for (r, card) in col.cards.iter().enumerate() {
                let chosen = self.selected.as_deref() == Some(card.name.as_str());
                if chosen {
                    sel_space = col.space.clone();
                }
                draw_card(&mut grid, col_x(c), row_y(r), cw, card, chosen, theme);
                ends.push((card.name.clone(), End::Card(c, r)));
            }
        }
        let end = |s: &Scope| -> Option<End> {
            let key = match s {
                Scope::Pane(n) => n.clone(),
                Scope::You => "you".into(),
                Scope::Space(sp) => format!("space:{sp}"),
                _ => return None,
            };
            ends.iter().find(|(k, _)| *k == key).map(|(_, e)| *e)
        };
        let related = |s: &Scope| match (s, &self.selected) {
            (Scope::Pane(n), Some(sel)) => n == sel,
            (Scope::You, Some(sel)) => sel == "you",
            (Scope::Space(sp), Some(_)) => sel_space.as_ref() == Some(sp),
            _ => false,
        };
        let mut placed: Vec<(usize, End, End, bool)> = v
            .routes
            .iter()
            .filter_map(|r| Some((r.index, end(&r.route.from)?, end(&r.route.to)?, related(&r.route.from) || related(&r.route.to))))
            .filter(|(_, a, b, _)| a != b)
            .collect();
        // the chosen card's routes last, so they lie on top
        placed.sort_by_key(|p| p.3);
        let mut labels = Vec::new();
        // Each card's exits and entries, each gap's lanes and each row gap's lanes are handed out in turn, so lines spread.
        let mut turns: std::collections::HashMap<(u8, usize, usize), usize> = std::collections::HashMap::new();
        let mut turn = |kind: u8, a: usize, b: usize| {
            let n = turns.entry((kind, a, b)).or_default();
            *n += 1;
            *n - 1
        };
        for (i, a, b, rel) in &placed {
            let Some(rv) = v.routes.iter().find(|r| r.index == *i) else { continue };
            let color = if self.selected.is_some() && !rel {
                theme.faint
            } else if rv.risk {
                theme.bad
            } else {
                action_color(rv.route.action, theme)
            };
            let style = g.fg(color);
            let dash = match rv.route.action {
                Action::Allow => 0,
                Action::Ask => 1,
                Action::No => 2,
            };
            let (ca, ya) = match *a {
                End::Card(c, r) => (c, row_y(r) + 1 + turn(0, c, r) % 3),
                End::Head(c) => (c, 0),
            };
            let (cb, lane_y, arrow_y, arrow, slot) = match *b {
                End::Card(c, r) => (c, row_y(r) - 2 - turn(3, r, 0) % 2, row_y(r) - 1, "▼", turn(1, c, r)),
                End::Head(c) => (c, 2, 1, "▲", turn(1, c, usize::MAX)),
            };
            let entry_x = col_x(cb) + 2 + (slot * 4) % (cw - 4);
            let right = cb >= ca;
            let (exit_x, gap) = if right { (col_x(ca) + cw, ca) } else { (col_x(ca) - 1, ca - 1) };
            let lane_x = col_x(gap) + cw + 1 + turn(2, gap, 0) % (GX - 2);
            let hit = Click::Line(*i);
            grid.path(&[(exit_x, ya), (lane_x, ya), (lane_x, lane_y), (entry_x, lane_y), (entry_x, arrow_y)], dash, style, &hit);
            grid.seg(exit_x, ya, if right { W } else { E }, dash, style, &hit);
            if let End::Card(..) = a {
                let border = if right { col_x(ca) + cw - 1 } else { col_x(ca) };
                grid.put(border, ya, if right { "├" } else { "┤" }, g.fg(theme.border), None);
            }
            grid.put(entry_x, arrow_y, arrow, style, Some(hit.clone()));
            let (from, to) = (rv.route.from.text(), rv.route.to.text());
            let mut words: Vec<String> = Vec::new();
            if rv.risk {
                words.push("risk".into());
            }
            if let Some(t) = v.traffic.iter().find(|t| t.from == from && t.to == to) {
                if t.held > 0 {
                    words.push(format!("{} held", t.held));
                }
                if t.today > 0 {
                    words.push(format!("{} today", t.today));
                }
            }
            if let Some(left) = rv.left {
                words.push(format!("{}m left", left.div_ceil(60)));
            }
            if !words.is_empty() && (self.selected.is_none() || *rel) {
                labels.push((lane_x.min(entry_x), lane_x.max(entry_x), lane_y, words.join(" · "), style, *i, format!("{from} → {to}")));
            }
        }
        let mut legend = Vec::new();
        for (x0, x1, y, words, style, i, ends) in labels {
            let label = format!(" {words} ");
            let len = cells(&label);
            if x1 > x0 + len + 1 {
                grid.put(x0 + (x1 - x0 - len) / 2 + 1, y, &label, style, Some(Click::Line(i)));
            } else {
                legend.push((format!("{ends} {words}"), style, i));
            }
        }
        Map { lines: grid.lines(), legend, colw: cw + GX }
    }

    /// The chosen card's rows: every other named pane, its tabs and its spaces, with the buttons that set a route.
    fn rows(&self, sel: &str, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let mut out: Vec<Line<Click>> = Vec::new();
        let mut head: Line<Click> = Vec::new();
        text(&mut head, format!("{sel}  "), g.fg(theme.accent).add_modifier(Modifier::BOLD));
        if sel == "you" {
            text(&mut head, "you write to every named pane; every named pane may write to you unless a route says no", g.fg(theme.muted));
            out.push(head);
            return out;
        }
        for (incoming, word) in [(false, "where it can write"), (true, "who can write here")] {
            let lit = (self.incoming == incoming).then(|| theme.cursor());
            chip(&mut head, word, lit, Some(Click::Incoming(incoming)), theme);
        }
        out.push(head);
        let me = scope_of(sel);
        let arrow = if self.incoming { "← " } else { "→ " };
        let names = v.columns.iter().flat_map(|c| &c.cards).filter(|c| c.pane.is_some()).map(|c| cells(&c.name)).max().unwrap_or(0);
        let row = |out: &mut Vec<Line<Click>>, label: String, other: Scope, why: String| {
            let (from, to) = if self.incoming { (other, me.clone()) } else { (me.clone(), other) };
            let own = v.routes.iter().find(|r| r.route.from == from && r.route.to == to).map(|r| r.route.action);
            let mut l: Line<Click> = Vec::new();
            text(&mut l, format!("{arrow}{label}  "), g.fg(theme.text));
            for a in [Action::Allow, Action::Ask, Action::No] {
                let lit = (own == Some(a)).then(|| Style::default().bg(action_color(a, theme)).fg(theme.ground));
                let act = MailAction::SetRoute { from: from.clone(), to: to.clone(), action: Some(a), until: None };
                chip(&mut l, a.word(), lit, Some(Click::Act(act)), theme);
            }
            let act = MailAction::SetRoute { from: from.clone(), to: to.clone(), action: None, until: None };
            chip(&mut l, "mode", own.is_none().then(|| theme.cursor()), Some(Click::Act(act)), theme);
            text(&mut l, format!(" {why}"), g.fg(theme.faint));
            out.push(l);
        };
        for col in v.columns.iter().filter(|c| c.space.is_some()) {
            let space = col.space.clone().unwrap_or_default();
            let others: Vec<&Card> = col.cards.iter().filter(|c| c.name != sel && c.pane.is_some()).collect();
            if others.is_empty() {
                continue;
            }
            out.push(vec![(format!("space {space}"), g.fg(theme.muted), None)]);
            let mut tabs: Vec<&str> = others.iter().map(|c| c.tab.as_str()).collect();
            tabs.dedup();
            for c in &others {
                let why = if self.incoming { verdict(&c.name, sel) } else { verdict(sel, &c.name) }.why;
                row(&mut out, format!("{:<names$}", c.name), Scope::Pane(c.name.clone()), why);
            }
            if tabs.len() > 1 {
                for t in tabs {
                    row(&mut out, format!("every pane in tab {space}/{t}"), Scope::Tab(space.clone(), t.into()), String::new());
                }
            }
            row(&mut out, format!("every pane in space {space}"), Scope::Space(space.clone()), String::new());
        }
        if out.len() == 1 {
            out.push(vec![("no other pane is named".into(), g.fg(theme.faint), None)]);
        }
        out
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent, v: &MailView) -> Out {
        use crossterm::event::KeyCode::*;
        let k = crate::keys::normalize(k);
        if self.map_view {
            match k.code {
                Esc if self.alone => return Out::Close,
                Esc => self.map_view = false,
                Left => self.pan_by(-1, 0),
                Right => self.pan_by(1, 0),
                Up => self.pan_by(0, -1),
                Down => self.pan_by(0, 1),
                PageUp => self.pan_by(0, -10),
                PageDown => self.pan_by(0, 10),
                _ => {}
            }
            return Out::None;
        }
        let (_, current) = self.waiting(v);
        let id = current.map(|m| m.id.clone()).unwrap_or_default();
        let request = current.is_some_and(|m| m.state == State::Held(Hold::RouteRequest));
        let held = current.is_some_and(|m| matches!(m.state, State::Held(_)));
        let answer = |action| Out::Act(MailAction::AnswerRequest { id: id.clone(), action });
        match k.code {
            Esc if self.selected.take().is_some() => Out::None,
            Esc => Out::Close,
            Char('m') => Out::Act(MailAction::SetMode(if v.mode == Mode::Open { Mode::Strict } else { Mode::Open })),
            Char('b') => Out::Broadcast,
            Char('t') => Out::Topology,
            Char('v') => {
                self.map_view = true;
                Out::None
            }
            Char('a') if request => answer(Action::Allow),
            Char('k') if request => answer(Action::Ask),
            Char('n') if request => answer(Action::No),
            Char('d') if held && !request => Out::Act(MailAction::Deliver(id)),
            Char('h') if held && !request => Out::Act(MailAction::DeliverAllow(id)),
            Char('x') if current.is_some() && !request => Out::Act(MailAction::Drop(id)),
            Char('r') if current.is_some() && !request => Out::Reply(id),
            Up => self.scroll_by(-1),
            Down => self.scroll_by(1),
            PageUp => self.scroll_by(-10),
            PageDown => self.scroll_by(10),
            _ => Out::None,
        }
    }

    fn scroll_by(&mut self, by: isize) -> Out {
        self.scroll.set(self.scroll.get().saturating_add_signed(by));
        Out::None
    }

    fn toggle(&mut self, name: String) {
        self.selected = if self.selected.as_ref() == Some(&name) { None } else { Some(name) };
    }

    fn pan_by(&self, columns: isize, lines: isize) {
        let (x, y) = self.pan.get();
        self.pan.set((x.saturating_add_signed(columns), y.saturating_add_signed(lines)));
    }

    /// `d`: where the last frame drew everything.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind::*};
        let (x, y) = (ev.column, ev.row);
        let hit = d.at(x, y);
        if self.map_view {
            match ev.kind {
                Down(MouseButton::Left) if d.close.contains((x, y).into()) && self.alone => return Out::Close,
                Down(MouseButton::Left) if d.close.contains((x, y).into()) => self.map_view = false,
                Down(MouseButton::Left) => match hit {
                    Some(Click::Card(n)) => self.toggle(n),
                    Some(Click::Pan(dx, dy)) => self.pan_by(dx, dy),
                    _ => {}
                },
                ScrollUp | ScrollDown | ScrollLeft | ScrollRight => {
                    let back = matches!(ev.kind, ScrollUp | ScrollLeft);
                    let by = if back { -1 } else { 1 };
                    // shift turns a mouse wheel sideways; a trackpad sends the sideways scroll itself
                    if matches!(ev.kind, ScrollLeft | ScrollRight) || ev.modifiers.contains(KeyModifiers::SHIFT) {
                        self.pan_by(by, 0);
                    } else {
                        self.pan_by(0, 3 * by);
                    }
                }
                _ => {}
            }
            return Out::None;
        }
        match ev.kind {
            Down(MouseButton::Left) => {
                if d.close.contains((x, y).into()) || !d.frame.contains((x, y).into()) {
                    return Out::Close;
                }
                match hit {
                    Some(Click::Line(i)) => return Out::Act(MailAction::CycleRoute(i)),
                    Some(Click::Select(n)) => self.toggle(n),
                    Some(Click::Start(i)) => self.start = i,
                    Some(Click::Incoming(b)) => self.incoming = b,
                    Some(Click::Act(a)) => return Out::Act(a),
                    Some(Click::Reply(id)) => return Out::Reply(id),
                    Some(Click::Broadcast) => return Out::Broadcast,
                    Some(Click::Topology) => return Out::Topology,
                    Some(Click::MapView) => self.map_view = true,
                    Some(Click::Waiting(id)) => self.at = Some(id),
                    Some(Click::AllThreads) => self.inbox = None,
                    Some(Click::Card(_) | Click::Pan(..)) | None => {}
                }
                Out::None
            }
            ScrollUp => self.scroll_by(-3),
            ScrollDown => self.scroll_by(3),
            _ => Out::None,
        }
    }
}

/// The widest of a card's lines, with the border and padding.
fn card_width(c: &Card) -> usize {
    let ((name, tag), l2, l3) = card_text(c);
    4 + (cells(&name) + 1 + cells(&tag)).max(cells(&l2)).max(cells(&l3))
}

/// A card's lines: (its name, its tab or foreign uid), its agent, its isolation.
fn card_text(c: &Card) -> ((String, String), String, String) {
    let tag = if c.uid != crate::policy::owner_uid() {
        format!("uid {}", c.uid)
    } else if c.tab.is_empty() {
        String::new()
    } else {
        format!("tab {}", c.tab)
    };
    let (l2, l3) = match (c.name.as_str(), c.pane) {
        ("you", _) => ("the owner".into(), String::new()),
        (_, None) => ("no pane holds".into(), "this name now".into()),
        _ => (c.agent.clone().unwrap_or_else(|| "shell".into()), cut(&c.isolation.words(), CARD_WORDS)),
    };
    ((c.name.clone(), tag), l2, l3)
}

fn draw_card(grid: &mut Grid, x: usize, y: usize, cw: usize, c: &Card, on: bool, theme: &Theme) {
    let g = Style::default().bg(theme.ground);
    let loose = c.pane.is_none() && c.name != "you";
    let border = g.fg(if on { theme.accent } else if loose { theme.muted } else { theme.border });
    let hit = Some(Click::Card(c.name.clone()));
    let (h, v) = if loose { ('┄', '┆') } else { ('─', '│') };
    let line = |l: char, r: char| format!("{l}{}{r}", h.to_string().repeat(cw - 2));
    grid.put(x, y, &line('┌', '┐'), border, hit.clone());
    grid.put(x, y + CH - 1, &line('└', '┘'), border, hit.clone());
    for j in 1..CH - 1 {
        grid.put(x, y + j, &format!("{v}{}{v}", " ".repeat(cw - 2)), border, hit.clone());
    }
    let ((name, tag), l2, l3) = card_text(c);
    let name_style = if on { g.fg(theme.accent) } else { g.fg(theme.text) }.add_modifier(Modifier::BOLD);
    grid.put(x + 2, y + 1, &name, name_style, hit.clone());
    grid.put(x + cw - 2 - cells(&tag), y + 1, &tag, g.fg(theme.muted), hit.clone());
    grid.put(x + 2, y + 2, &l2, g.fg(if loose { theme.muted } else { theme.text }), hit.clone());
    grid.put(x + 2, y + 3, &l3, g.fg(theme.muted), hit);
}

/// A waiting row: who, to whom, why, how long; then its buttons, with keys on the row the keys act on.
fn waiting_row(m: &Message, keys: bool, theme: &Theme) -> Vec<Line<Click>> {
    let g = Style::default().bg(theme.ground);
    let id = m.id.clone();
    let (head, buttons): (String, Vec<(&str, &str, Click)>) = match &m.state {
        State::Held(Hold::RouteRequest) => (
            format!("{} asks to write to {} · {}", m.from.name(), m.to.name(), age(m)),
            [("a", "allow", Action::Allow), ("k", "ask", Action::Ask), ("n", "no", Action::No)]
                .into_iter()
                .map(|(k, w, action)| (k, w, Click::Act(MailAction::AnswerRequest { id: id.clone(), action })))
                .collect(),
        ),
        State::Held(h) => (
            format!("{} → {} · {} · {}", m.from.name(), m.to.name(), h.words(), age(m)),
            vec![
                ("d", "deliver", Click::Act(MailAction::Deliver(id.clone()))),
                ("h", "deliver, allow 1 hour", Click::Act(MailAction::DeliverAllow(id.clone()))),
                ("x", "drop", Click::Act(MailAction::Drop(id.clone()))),
                ("r", "reply…", Click::Reply(id.clone())),
            ],
        ),
        _ => (
            format!("{} → {} · {}", m.from.name(), Party::You.name(), age(m)),
            vec![("r", "reply…", Click::Reply(id.clone())), ("x", "drop", Click::Act(MailAction::Drop(id.clone())))],
        ),
    };
    let head_style = if keys { g.fg(theme.text).add_modifier(Modifier::BOLD) } else { g.fg(theme.text) };
    let mut l: Line<Click> = vec![("  ".into(), g, None)];
    for (key, word, click) in buttons {
        if keys {
            button(&mut l, key, word, click, theme);
        } else {
            chip(&mut l, word, None, Some(click), theme);
        }
    }
    vec![vec![(head, head_style, Some(Click::Waiting(id)))], l]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MailColumn, RouteView, Traffic};
    use crate::mail::{Step, Via};
    use crate::policy::{Files, Isolation, Network, Route};

    fn card(name: &str, pane: Option<PaneId>, tab: &str, agent: Option<&str>, isolation: Isolation) -> Card {
        Card { name: name.into(), pane, tab: tab.into(), agent: agent.map(Into::into), uid: crate::policy::owner_uid(), isolation }
    }

    fn msg(id: &str, from: &str, to: Option<&str>, state: State, ago: u64) -> Message {
        let party = |n: &str, p| Party::Pane { pane: Some(p), name: n.into(), agent: Some("claude".into()), uid: crate::policy::owner_uid() };
        let at = crate::hook::now() - ago;
        Message {
            id: id.into(),
            thread: id.into(),
            reply_to: None,
            from: party(from, 1),
            to: to.map_or(Party::You, |t| party(t, 3)),
            text: "hi".into(),
            hops: vec![],
            auto: false,
            created: at,
            via: (state == State::Delivered).then_some(Via::Hook),
            events: vec![Step { state: state.clone(), at, note: None }],
            state,
            passed: false,
        }
    }

    /// Two spaces, four named panes, one route of each kind and a risky one, and three waiting rows.
    pub(crate) fn view() -> MailView {
        let open = Isolation::default();
        let offline = Isolation { files: Files::Everything, network: Network::None };
        let columns = vec![
            MailColumn { space: Some("work".into()), cards: vec![card("planner", Some(1), "main", Some("claude"), open.clone()), card("coder", Some(2), "main", Some("codex"), open.clone())] },
            MailColumn { space: Some("research".into()), cards: vec![card("reader", Some(3), "1", Some("pi"), open.clone()), card("vault", Some(4), "1", Some("pi"), offline)] },
            MailColumn { space: None, cards: vec![card("you", None, "", None, open)] },
        ];
        let route = |i, f: &str, t: &str, a, risk| RouteView { index: i, route: Route { from: Scope::parse(f).unwrap(), to: Scope::parse(t).unwrap(), action: a, until: None }, risk, left: None };
        let routes = vec![
            route(0, "planner", "reader", Action::Allow, false),
            route(1, "coder", "reader", Action::Ask, false),
            route(2, "reader", "space:work", Action::No, false),
            route(3, "planner", "vault", Action::Allow, true),
            route(4, "*", "*", Action::Allow, false),
        ];
        let waiting = vec![
            msg("m1", "coder", Some("reader"), State::Held(Hold::Route), 120),
            msg("m2", "vault", Some("planner"), State::Held(Hold::RouteRequest), 60),
            msg("m3", "reader", None, State::Delivered, 30),
        ];
        let traffic = vec![Traffic { from: "coder".into(), to: "reader".into(), held: 1, today: 3 }];
        MailView { on: true, mode: Mode::Open, broken: false, columns, routes, traffic, today: waiting.clone(), waiting }
    }

    fn verdict(_: &str, _: &str) -> Verdict {
        Verdict { action: Action::Allow, why: "open mode".into(), route: None }
    }

    #[test]
    fn golden_messages_window_narrow_and_wide() {
        let theme = Theme::dark();
        let v = view();
        let mut out = String::new();
        for (w, h) in [(80, 24), (120, 40)] {
            let screen = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(screen);
            Messages::new().draw(&mut buf, screen, &theme, &v, &verdict);
            out += &format!("== {w}x{h}\n{}", crate::app::snapshots::dump(&buf, screen));
        }
        crate::hook::tests::golden("ui/messages.txt", &out);
    }

    /// The text of every line.
    fn texts(lines: &[Line<Click>]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|(t, _, _)| t.as_str()).collect()).collect()
    }

    /// What `m` draws on a `w` × `h` screen.
    fn drawn(m: &Messages, v: &MailView, w: u16, h: u16) -> (Drawn<Click>, String) {
        let screen = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(screen);
        let d = m.draw(&mut buf, screen, &Theme::dark(), v, &verdict);
        (d, crate::app::snapshots::dump(&buf, screen))
    }

    fn press(m: &mut Messages, d: &Drawn<Click>, h: &Click) -> Out {
        let r = d.hits.iter().find(|(_, c)| c == h).unwrap_or_else(|| panic!("no {h:?} drawn")).0;
        let ev = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: r.x, row: r.y, modifiers: crossterm::event::KeyModifiers::NONE };
        m.mouse(ev, d)
    }

    fn key(m: &mut Messages, v: &MailView, code: crossterm::event::KeyCode) -> Out {
        m.key(crossterm::event::KeyEvent::from(code), v)
    }

    /// A full matrix: eleven panes in six spaces, and `you`.
    fn full() -> MailView {
        let mut v = view();
        let open = Isolation::default();
        let sizes = [2, 2, 2, 2, 2, 1];
        let mut n = 0;
        v.columns = sizes
            .iter()
            .enumerate()
            .map(|(s, k)| MailColumn {
                space: Some(format!("s{s}")),
                cards: (0..*k)
                    .map(|_| {
                        n += 1;
                        card(&format!("p{n}"), Some(n), "main", Some("claude"), open.clone())
                    })
                    .collect(),
            })
            .chain([MailColumn { space: None, cards: vec![card("you", None, "", None, Isolation::default())] }])
            .collect();
        v.routes.clear();
        v.waiting.clear();
        v.traffic.clear();
        v
    }

    fn cell<'a>(lines: &'a [Line<Click>], row: &str, at: usize) -> &'a (String, Style, Option<Click>) {
        let l = lines.iter().find(|l| l.first().is_some_and(|(t, _, _)| t.trim_end() == row)).unwrap_or_else(|| panic!("no row {row}"));
        &l[1 + 2 * at + 1]
    }

    #[test]
    fn the_matrix_shows_the_verdict_of_each_pair_bright_for_a_route_and_faint_for_the_mode() {
        let theme = Theme::dark();
        let v = view();
        let by_route = |from: &str, to: &str| match (from, to) {
            ("planner", "reader") => Verdict { action: Action::Allow, why: "route".into(), route: Some(0) },
            ("coder", "reader") => Verdict { action: Action::Ask, why: "route".into(), route: Some(1) },
            _ => Verdict { action: Action::No, why: "open mode".into(), route: None },
        };
        let (lines, _) = Messages::new().lines(&v, &by_route, 120, &theme);
        let text = texts(&lines);
        assert!(text.iter().any(|l| l.starts_with(" ") && l.contains("│you")), "numbers, then you:\n{text:#?}");
        // columns: 1 planner, 2 coder, 3 reader, 4 vault, then you
        let (word, style, _) = cell(&lines, "1 planner", 2);
        assert_eq!(word.trim_end(), "allow");
        assert!(style.add_modifier.contains(Modifier::BOLD) && !style.add_modifier.contains(Modifier::DIM), "a route decides: bright");
        let (word, style, _) = cell(&lines, "1 planner", 3);
        assert_eq!(word.trim_end(), "no");
        assert!(style.add_modifier.contains(Modifier::DIM), "the mode decides: faint");
        assert_eq!(cell(&lines, "1 planner", 0).0.trim_end(), "—", "the diagonal");
        // coder → reader has one message waiting in view(); two make "held 2"
        assert_eq!(cell(&lines, "2 coder", 2).0.trim_end(), "held 1");
    }

    #[test]
    fn a_held_cell_counts_and_selects_the_first_waiting_message() {
        let theme = Theme::dark();
        let mut v = view();
        v.waiting.push(msg("m4", "coder", Some("reader"), State::Held(Hold::Route), 10));
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        let (word, style, hit) = cell(&lines, "2 coder", 2);
        assert_eq!(word.trim_end(), "held 2");
        assert_eq!(style.fg, Some(theme.warn));
        assert_eq!(*hit, Some(Click::Waiting("m1".into())), "the first in waiting order");
        for _ in 0..120 {
            v.waiting.push(msg("mx", "coder", Some("reader"), State::Held(Hold::Route), 10));
        }
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        assert_eq!(cell(&lines, "2 coder", 2).0.trim_end(), "held 99+");
    }

    #[test]
    fn a_click_on_a_cell_gives_the_next_route_and_keeps_the_time() {
        let theme = Theme::dark();
        let mut v = view();
        v.routes[0].route.until = Some(1_000);
        let act = |lines: &[Line<Click>], row: &str, at: usize| match &cell(lines, row, at).2 {
            Some(Click::Act(a)) => a.clone(),
            other => panic!("{other:?}"),
        };
        let set = |f: &str, t: &str, a, until| MailAction::SetRoute { from: scope_of(f), to: scope_of(t), action: a, until };
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        assert_eq!(act(&lines, "1 planner", 2), set("planner", "reader", Some(Action::Ask), Some(1_000)), "a timed allow becomes ask with the same time");
        assert_eq!(act(&lines, "1 planner", 1), set("planner", "coder", Some(Action::Allow), None), "none → allow");
        assert_eq!(act(&lines, "1 planner", 4), set("planner", "you", Some(Action::Allow), None));
        v.routes[0].route.action = Action::No;
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        assert_eq!(act(&lines, "1 planner", 2), set("planner", "reader", None, None), "no → none");
        let (d, _) = drawn(&Messages::new(), &v, 140, 60);
        let mut m = Messages::new();
        assert_eq!(press(&mut m, &d, &Click::Act(set("planner", "reader", None, None))), Out::Act(set("planner", "reader", None, None)));
    }

    #[test]
    fn a_click_on_a_label_gives_its_rows_both_ways() {
        let theme = Theme::dark();
        let v = view();
        let (d, _) = drawn(&Messages::new(), &v, 140, 60);
        let mut m = Messages::new();
        assert_eq!(press(&mut m, &d, &Click::Select("planner".into())), Out::None);
        assert_eq!(m.selected.as_deref(), Some("planner"));
        let acts = |m: &Messages| -> Vec<MailAction> {
            m.lines(&v, &verdict, 120, &theme).0.into_iter().flatten().filter_map(|(_, _, h)| if let Some(Click::Act(a)) = h { Some(a) } else { None }).collect()
        };
        let route = |f: &str, t: Scope, a| MailAction::SetRoute { from: scope_of(f), to: t, action: a, until: None };
        let shown = texts(&m.lines(&v, &verdict, 120, &theme).0);
        assert!(shown.iter().any(|l| l.contains("where it can write")), "{shown:#?}");
        assert!(acts(&m).contains(&route("planner", Scope::Space("research".into()), None)), "→ every pane in space research [mode]");
        m.incoming = true;
        assert!(acts(&m).contains(&route("reader", Scope::Pane("planner".into()), Some(Action::No))), "← reader [no]");
        let esc = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Esc);
        assert_eq!(m.key(esc, &v), Out::None, "Esc clears the choice");
        assert_eq!(m.key(esc, &v), Out::Close, "a second Esc closes");
    }

    #[test]
    fn the_mode_is_explained_and_strict_mode_lets_a_pane_write_to_you_only() {
        let theme = Theme::dark();
        let mut v = view();
        let said = |v: &MailView| texts(&Messages::new().lines(v, &verdict, 140, &theme).0);
        assert!(said(&v).iter().any(|l| l == "no route: a pane may write to you and to its own account; to another account, you are asked"));
        assert!(said(&v).iter().any(|l| l == "bright: a route decides · faint: the mode · a reply in a thread passes unless a route says no"));
        v.mode = Mode::Strict;
        assert!(said(&v).iter().any(|l| l == "no route: a pane may write to you only"));
        let strict = |_: &str, to: &str| Verdict { action: if to == "you" { Action::Allow } else { Action::No }, why: "strict mode".into(), route: None };
        let (lines, _) = Messages::new().lines(&v, &strict, 140, &theme);
        let (you, style, _) = cell(&lines, "1 planner", 4);
        assert_eq!(you.trim_end(), "allow");
        assert!(style.add_modifier.contains(Modifier::DIM), "faint: the mode decides");
        assert_eq!(cell(&lines, "1 planner", 1).0.trim_end(), "no");
    }

    #[test]
    fn columns_that_do_not_fit_are_whole_spaces_from_a_chosen_one_and_the_window_says_so() {
        let theme = Theme::dark();
        let mut v = view();
        v.columns.insert(2, MailColumn { space: Some("ops".into()), cards: vec![card("runner", Some(5), "main", None, Isolation::default())] });
        let mut m = Messages::new();
        let header = |m: &Messages| -> String { texts(&m.lines(&v, &verdict, 60, &theme).0).into_iter().find(|l| l.contains("│you")).unwrap() };
        let limit = |m: &Messages| -> String { texts(&m.lines(&v, &verdict, 60, &theme).0).into_iter().find(|l| l.starts_with("recipients")).unwrap() };
        assert!(limit(&m).starts_with("recipients 1–4 of 5 fit this width; start at space:"), "{}", limit(&m));
        let (h, _) = (header(&m), ());
        assert!(h.contains('1') && h.contains('4') && !h.contains('5'), "work and research, whole spaces: {h}");
        let (lines, _) = m.lines(&v, &verdict, 60, &theme);
        assert_eq!(lines.iter().flatten().filter(|(_, _, h)| matches!(h, Some(Click::Start(_)))).count(), 3, "a chip per space");
        let (d, _) = drawn(&m, &v, 66, 60);
        assert_eq!(press(&mut m, &d, &Click::Start(1)), Out::None);
        assert!(limit(&m).starts_with("recipients 3–5 of 5"), "research is the start now: {}", limit(&m));
        let h = header(&m);
        assert!(h.contains('5') && !h.contains('2'), "{h}");
        // a route to a name no pane holds is a chip under the matrix, not a cell
        v.routes.push(RouteView { index: 5, route: Route { from: Scope::parse("planner").unwrap(), to: Scope::parse("ghost").unwrap(), action: Action::Allow, until: None }, risk: false, left: None });
        let (lines, _) = Messages::new().lines(&v, &verdict, 140, &theme);
        assert!(lines.iter().flatten().any(|(t, _, h)| *h == Some(Click::Line(5)) && t.contains("ghost")));
        assert!(texts(&lines).iter().any(|l| l.starts_with("routes ")));
    }

    #[test]
    fn an_expired_route_is_not_cycled_and_the_cell_starts_again_from_allow() {
        let theme = Theme::dark();
        let mut v = view();
        v.routes[0].route.until = Some(1);
        v.routes[0].left = Some(0);
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        let want = MailAction::SetRoute { from: scope_of("planner"), to: scope_of("reader"), action: Some(Action::Allow), until: None };
        assert_eq!(cell(&lines, "1 planner", 2).2, Some(Click::Act(want)));
    }

    #[test]
    fn a_group_wider_than_the_room_is_cut_and_the_line_gives_the_numbers() {
        let theme = Theme::dark();
        let v = full();
        let mut m = Messages::new();
        // label 6 wide: (50 - 6) / 9 = 4 columns, so s0 fits whole (two panes and you) and s1 does not
        let text = texts(&m.lines(&v, &verdict, 50, &theme).0);
        let limit = text.iter().find(|l| l.starts_with("recipients")).unwrap();
        assert!(limit.starts_with("recipients 1–2 of 11 fit this width"), "{limit}");
        let header = text.iter().find(|l| l.contains("│you")).unwrap();
        assert!(header.contains('2') && !header.contains('3'), "whole groups only: {header}");
        // room for one column and you: the group is cut to what fits, and the line says so
        let text = texts(&m.lines(&v, &verdict, 6 + 2 * CELL, &theme).0);
        let limit = text.iter().find(|l| l.starts_with("recipients")).unwrap();
        assert!(limit.starts_with("recipients 1–1 of 11 fit this width"), "{limit}");
        // no room for any recipient: the line says so
        let text = texts(&m.lines(&v, &verdict, 6 + CELL, &theme).0);
        assert!(text.iter().any(|l| l.starts_with("no recipient of 11 fit this width")), "{text:#?}");
        // no room even for you: no matrix, one line
        let (lines, _) = m.lines(&v, &verdict, 5 + CELL, &theme);
        let text = texts(&lines);
        assert!(text.iter().any(|l| l == "the matrix needs 15 columns, this window has 14: widen the window"), "{text:#?}");
        assert!(!text.iter().any(|l| l.contains("│you")) && !lines.iter().flatten().any(|(_, _, h)| matches!(h, Some(Click::Act(MailAction::SetRoute { .. })))), "no cells");
        m.start = 5;
        let text = texts(&m.lines(&v, &verdict, 50, &theme).0);
        assert!(text.iter().any(|l| l.starts_with("recipients 11–11 of 11")), "{text:#?}");
    }

    #[test]
    fn a_route_from_a_pane_to_itself_is_a_chip() {
        let theme = Theme::dark();
        let mut v = view();
        let me = Scope::parse("planner").unwrap();
        v.routes.push(RouteView { index: 5, route: Route { from: me.clone(), to: me, action: Action::No, until: None }, risk: false, left: None });
        let (lines, _) = Messages::new().lines(&v, &verdict, 140, &theme);
        assert!(lines.iter().flatten().any(|(_, _, h)| *h == Some(Click::Line(5))));
    }

    #[test]
    fn a_long_risk_line_wraps_inside_the_window() {
        let theme = Theme::dark();
        let (lines, _) = Messages::new().lines(&view(), &verdict, 60, &theme);
        let text = texts(&lines);
        let at = text.iter().position(|l| l.starts_with("risk:")).unwrap();
        assert!(text[at..=at + 1].iter().all(|l| l.chars().count() <= 60) && text[at + 1].contains("without"), "{text:#?}");
    }

    #[test]
    fn the_owners_shape_fits_whole_at_182_cells() {
        let theme = Theme::dark();
        let v = full();
        let (lines, _) = Messages::new().lines(&v, &verdict, 182, &theme);
        let text = texts(&lines);
        assert!(!text.iter().any(|l| l.starts_with("recipients")), "no limit line: {text:#?}");
        assert!(text.iter().any(|l| l.contains("11 ") && l.contains("│you")), "every number up to 11 and you");
        let row = lines.iter().find(|l| l.first().is_some_and(|(t, _, _)| t.trim_end() == "1 p1")).unwrap();
        assert_eq!(row.len(), 1 + 2 * 12, "the label, then a separator and a cell for each of 11 panes and you");
    }

    #[test]
    fn the_routes_map_pans_by_columns_and_clamps() {
        let v = full();
        let mut m = Messages::routes_map();
        let (_, shown) = drawn(&m, &v, 100, 30);
        assert!(shown.contains("columns 1–3 of 7") && shown.contains("Routes map"), "{shown}");
        assert!(shown.contains("[→] right") && !shown.contains("[←] left"), "only the ways there is more of: {shown}");
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Right), Out::None);
        assert!(drawn(&m, &v, 100, 30).1.contains("columns 2–4 of 7"));
        for _ in 0..20 {
            key(&mut m, &v, crossterm::event::KeyCode::Right);
        }
        let (d, shown) = drawn(&m, &v, 100, 30);
        assert!(shown.contains("columns 5–7 of 7") && !shown.contains("[→] right") && shown.contains("[←] left"), "{shown}");
        press(&mut m, &d, &Click::Pan(-1, 0));
        assert!(drawn(&m, &v, 100, 30).1.contains("columns 4–6 of 7"), "a button pans too");
    }

    #[test]
    fn the_routes_map_selects_a_card_and_leaves_with_escape_and_never_writes() {
        let v = full();
        let mut m = Messages { map_view: true, ..Messages::new() };
        let (d, _) = drawn(&m, &v, 100, 30);
        let read_only = |m: &mut Messages, d: &Drawn<Click>| {
            use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind::*};
            for c in "abdhkmnrstvx".chars().map(KeyCode::Char).chain([KeyCode::Enter, KeyCode::Up, KeyCode::Down, KeyCode::PageUp, KeyCode::PageDown, KeyCode::Left, KeyCode::Right]) {
                assert!(!matches!(m.key(crossterm::event::KeyEvent::from(c), &v), Out::Act(_) | Out::Reply(_) | Out::Broadcast | Out::Topology), "{c:?}");
            }
            for kind in [Down(MouseButton::Left), Up(MouseButton::Left), Drag(MouseButton::Left), ScrollUp, ScrollDown, ScrollLeft, ScrollRight] {
                for (column, row) in (0..100).flat_map(|x| (0..30).map(move |y| (x, y))) {
                    let out = m.mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }, d);
                    assert!(matches!(out, Out::None), "{kind:?} at {column},{row}");
                    if !m.map_view {
                        m.map_view = true;
                    }
                }
            }
        };
        read_only(&mut m, &d);
        m.map_view = true;
        m.selected = None;
        assert_eq!(press(&mut m, &d, &Click::Card("p1".into())), Out::None);
        assert_eq!(m.selected.as_deref(), Some("p1"));
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Esc), Out::None);
        assert!(!m.map_view && m.selected.as_deref() == Some("p1"), "Escape leaves the map and keeps the choice");
        let mut alone = Messages::routes_map();
        let (d, _) = drawn(&alone, &v, 100, 30);
        assert_eq!(key(&mut alone, &v, crossterm::event::KeyCode::Esc), Out::Close, "opened from the menu, Escape closes it");
        let close = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: d.close.x, row: d.close.y, modifiers: crossterm::event::KeyModifiers::NONE };
        assert_eq!(alone.mouse(close, &d), Out::Close);
    }

    #[test]
    fn the_wheel_pans_the_routes_map_down_and_with_shift_across() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let v = full();
        let mut m = Messages::routes_map();
        let (d, _) = drawn(&m, &v, 100, 20);
        let wheel = |m: &mut Messages, kind, modifiers| m.mouse(MouseEvent { kind, column: 50, row: 5, modifiers }, &d);
        wheel(&mut m, MouseEventKind::ScrollDown, KeyModifiers::NONE);
        assert!(drawn(&m, &v, 100, 20).1.contains("lines 4–"), "three lines down");
        wheel(&mut m, MouseEventKind::ScrollDown, KeyModifiers::SHIFT);
        assert!(drawn(&m, &v, 100, 20).1.contains("columns 2–4 of 7"), "shift: a column across");
        wheel(&mut m, MouseEventKind::ScrollLeft, KeyModifiers::NONE);
        assert!(drawn(&m, &v, 100, 20).1.contains("columns 1–3 of 7"));
    }

    #[test]
    fn a_cards_isolation_line_is_cut_at_its_width() {
        let iso = |e: &[&str]| Isolation { files: Files::Folder, network: Network::Providers(e.iter().map(|s| s.to_string()).collect()) };
        let short = card("reader", Some(3), "main", Some("pi"), iso(&["pi/deepseek"]));
        assert_eq!(card_text(&short).2, "this folder only, only pi (deepseek)");
        let long = card("reader", Some(3), "main", Some("pi"), iso(&["claude", "pi/deepseek", "pi/openai", "pi/xai", "pi/zai"]));
        let l3 = card_text(&long).2;
        assert_eq!((l3.chars().count(), l3.as_str()), (CARD_WORDS, "this folder only, only claude, pi (deepseek…"));
        let two = card("reader", Some(3), "main", Some("gemini"), iso(&["claude", "gemini"]));
        assert_eq!(card_text(&two).2, "this folder only, only claude, gemini", "a list that fit before still fits");
        assert_eq!(card_width(&long), 4 + CARD_WORDS);
    }

    #[test]
    fn the_map_keeps_its_lines_out_of_the_cards() {
        let theme = Theme::dark();
        let v = view();
        let map = Messages::new().map(&v, &theme);
        let (lines, legend) = (map.lines, map.legend);
        let mut drawn: Vec<usize> = lines.iter().flatten().filter_map(|(_, _, h)| if let Some(Click::Line(i)) = h { Some(*i) } else { None }).collect();
        drawn.sort();
        drawn.dedup();
        assert_eq!(drawn, vec![0, 1, 2, 3], "every route between cards and headers; * → * is not on the map");
        let text: String = lines.iter().map(|l| l.iter().map(|(t, _, _)| t.as_str()).collect::<String>() + "\n").collect();
        let said = |words: &str, ends: &str, i: usize| text.contains(words) || legend.iter().any(|(l, _, n)| *l == format!("{ends} {words}") && *n == i);
        assert!(said("1 held · 3 today", "coder → reader", 1) && said("risk", "planner → vault", 3), "traffic and risk on their lines, or beside the map:\n{text}");
        // a card is 22 wide and 5 high; columns 30 apart, rows 8 apart from row 6
        for (y, l) in lines.iter().enumerate() {
            let mut x = 0;
            for (t, _, hit) in l {
                for _ in t.chars() {
                    let in_card = x % (22 + GX) < 22 && y >= TOP && (y - TOP) % (CH + GY) < CH && !(x / (22 + GX) == 2 && y >= TOP + CH);
                    assert!(!(in_card && matches!(hit, Some(Click::Line(_)))), "a line crossed a card at {x},{y}:\n{text}");
                    x += 1;
                }
            }
        }
        assert_eq!(map.colw, 22 + GX);
    }

    #[test]
    fn a_message_delivered_into_an_inbox_says_the_session_may_still_hold_it() {
        let theme = Theme::dark();
        let mut v = view();
        v.today[2].via = Some(Via::Inbox);
        let shown: Vec<String> = Messages::new().lines(&v, &verdict, 102, &theme).0.iter().map(|l| l.iter().map(|(t, _, _)| t.as_str()).collect()).collect();
        let notes: Vec<&String> = shown.iter().filter(|l| l.contains(INBOX_NOTE)).collect();
        assert_eq!(notes.len(), 1, "{shown:#?}");
        assert!(notes[0].starts_with("reader → you  inbox  delivered"), "{shown:#?}");
    }

    #[test]
    fn the_keys_act_on_the_row_it_opened_on() {
        let v = view();
        let key = |m: &mut Messages, c| m.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c)), &v);
        let mut m = Messages::new();
        assert_eq!(key(&mut m, 'd'), Out::Act(MailAction::Deliver("m1".into())), "the first row");
        assert_eq!(key(&mut m, 'a'), Out::None, "a held message is not a request");
        let mut m = Messages::at("m2".into());
        assert_eq!(key(&mut m, 'a'), Out::Act(MailAction::AnswerRequest { id: "m2".into(), action: Action::Allow }));
        assert_eq!(key(&mut m, 'd'), Out::None);
        let mut m = Messages::at("m3".into());
        assert_eq!(key(&mut m, 'r'), Out::Reply("m3".into()));
        assert_eq!(key(&mut m, 'm'), Out::Act(MailAction::SetMode(Mode::Strict)));
    }
}
