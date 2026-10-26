//! The Messages window: the matrix of who may write to whom, the mail waiting for the owner, today's threads; and the routes map.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{Card, MailAction, MailView};
use crate::chrome::Scroll;
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
pub fn draw<H: Clone>(buf: &mut Buffer, r: Rect, title: &str, theme: &Theme, lines: &[Line<H>], scroll: &Scroll, buttons: &[(String, H)], close: &str) -> Drawn<H> {
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
    let from = scroll.place(room, lines.len());
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
    scroll.draw(buf, r, inner.bottom() - 2, theme);
    let ground = Style::default().bg(theme.ground);
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

/// A wide window for `lines`, and the lines wrapped to its width.
pub fn fit<H: Clone + PartialEq>(screen: Rect, lines: Vec<Line<H>>) -> (Rect, Vec<Line<H>>) {
    let w = crate::chrome::fit(screen, crate::chrome::WIDE_W, 0).width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
    let lines = wrap(lines, w);
    (crate::chrome::fit(screen, crate::chrome::WIDE_W, lines.len() as u16), lines)
}

/// `lines` in at most `width` cells. Text alone wraps between words under its own indent; a line of several parts,
/// such as chips, breaks between them and goes on under its second part, each keeping its click. A part that does not
/// fit there goes on under the text after the line's label ("pi: "), or its indent when it has none; a first part
/// wider than the line wraps under that same place.
pub fn wrap<H: Clone + PartialEq>(lines: Vec<Line<H>>, width: usize) -> Vec<Line<H>> {
    let mut out = Vec::new();
    let pad = |n: usize| (" ".repeat(n), Style::default(), None);
    for line in lines {
        if self::width(&line) <= width {
            out.push(line);
        } else if let [(text, style, hit)] = &line[..] {
            let indent = text.len() - text.trim_start_matches(' ').len();
            for part in crate::menu::wrap(&text[indent..], width.saturating_sub(indent).max(1)) {
                out.push(vec![(format!("{}{part}", " ".repeat(indent)), *style, hit.clone())]);
            }
        } else {
            let indent = line.first().map_or(0, |s| s.0.len() - s.0.trim_start_matches(' ').len());
            // under the text after a short "name: " label, or the indent
            let hang = line.first().and_then(|s| s.0[indent..].find(": ")).filter(|&i| indent + i + 2 < width / 2).map_or(indent, |i| indent + i + 2);
            let mut under = line.first().map_or(0, |s| cells(&s.0));
            let mut segs = line.into_iter();
            let (mut row, mut x, mut start): (Line<H>, usize, usize) = (Vec::new(), 0, 0);
            if under > width
                && let Some((text, style, hit)) = segs.next()
            {
                let (mut at, mut cur) = (indent, String::new());
                for word in text[indent..].split(' ').filter(|w| !w.is_empty()) {
                    if !cur.is_empty() && at + cells(&cur) + 1 + cells(word) > width {
                        out.push(vec![(format!("{}{cur}", " ".repeat(at)), style, hit.clone())]);
                        (at, cur) = (hang, String::new());
                    }
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(word);
                }
                if !cur.is_empty() {
                    out.push(vec![(format!("{}{cur}", " ".repeat(at)), style, hit.clone())]);
                }
                (row, x, start, under) = (vec![pad(hang)], hang, hang, hang);
            }
            // A button's key and word, or a chip and the gap after it, move together.
            let mut units: Vec<Line<H>> = Vec::new();
            for seg in segs {
                let blank = seg.2.is_none() && seg.0.trim().is_empty();
                match units.last_mut() {
                    Some(u) if blank || seg.2.is_some() && u.last().is_some_and(|p| p.2 == seg.2) => u.push(seg),
                    _ => units.push(vec![seg]),
                }
            }
            for unit in units {
                let all = self::width(&unit);
                let w = all - unit.iter().rev().take_while(|s| s.2.is_none() && s.0.trim().is_empty()).map(|s| cells(&s.0)).sum::<usize>();
                if x > start && x + w > width {
                    out.push(std::mem::take(&mut row));
                    start = if under + w <= width { under } else { hang };
                    row.push(pad(start));
                    x = start;
                }
                x += all;
                row.extend(unit);
            }
            if x > start {
                out.push(row);
            }
        }
    }
    out
}

pub const CLOSE: &str = " [Esc] close ";
const BACK: &str = " [Esc] back ";
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
    /// The matrix's first shown column.
    Start(usize),
    /// The spaces, or the panes of a pair of them (None: `you`).
    Zoom(Option<(String, Option<String>)>),
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
    /// The held-message view shows this waiting message, by id.
    pub message: Option<String>,
    /// Its sender and recipient: the next message between them follows it once it is answered.
    pair: Option<(String, String)>,
    /// Opened from outside the window (the menu, the sidebar): leaving the map or the message closes it.
    alone: bool,
    /// The matrix's first shown column when its columns do not all fit.
    start: usize,
    /// The pair of spaces whose panes the matrix shows (None: `you`); None shows the spaces.
    zoom: Option<(String, Option<String>)>,
    /// The routes map's first column and first line.
    pan: Cell<(usize, usize)>,
    scroll: Scroll,
    /// The shown message's text.
    read: Scroll,
    /// The message the last frame drew, None for the list: keys act on what was drawn only.
    drawn: std::cell::RefCell<Option<String>>,
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

    /// Opened on one waiting message from outside the window.
    pub fn at(id: String) -> Self {
        Messages { at: Some(id.clone()), message: Some(id), alone: true, ..Self::default() }
    }

    /// Before a frame: a shown message that no longer waits gives way to the next of its pair; false closes the window.
    pub fn settle(&mut self, v: &MailView) -> bool {
        let named = |s: &str| v.columns.iter().any(|c| c.space.as_deref() == Some(s) && c.cards.iter().any(|c| c.pane.is_some()));
        if self.zoom.as_ref().is_some_and(|(a, b)| !named(a) || b.as_deref().is_some_and(|b| !named(b))) {
            self.zoom = None;
            self.start = 0;
        }
        let Some(id) = &self.message else { return true };
        if let Some(m) = v.waiting.iter().find(|m| m.id == *id) {
            self.pair.get_or_insert_with(|| (m.from.name().into(), m.to.name().into()));
            return true;
        }
        self.read.set(0);
        let next = self.pair.as_ref().and_then(|(f, t)| v.waiting.iter().find(|m| self.mine(m) && m.from.name() == f && m.to.name() == t));
        match next {
            Some(m) => {
                self.message = Some(m.id.clone());
                self.at = self.message.clone();
                true
            }
            None => self.leave() == Out::None,
        }
    }

    /// Out of the held-message view: back to the window, or closed when it was opened from outside.
    fn leave(&mut self) -> Out {
        if self.alone {
            return Out::Close;
        }
        self.message = None;
        self.pair = None;
        Out::None
    }

    fn open(&mut self, id: String) {
        self.at = Some(id.clone());
        self.message = Some(id);
        self.read.set(0);
    }

    fn shown<'a>(&self, v: &'a MailView) -> Option<&'a Message> {
        self.message.as_ref().and_then(|id| v.waiting.iter().find(|m| m.id == *id))
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
        if let Some(m) = self.shown(v) {
            return self.draw_message(buf, screen, theme, m);
        }
        self.drawn.replace(None);
        let r = crate::chrome::fit(screen, crate::chrome::WIDE_W, u16::MAX);
        let w = r.width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let (lines, at) = self.lines(v, verdict, w, theme);
        if let Some(at) = at.filter(|_| !self.shown.replace(true)) {
            self.scroll.set(at.saturating_sub(1));
        }
        draw(buf, r, "Messages", theme, &lines, &self.scroll, &[], CLOSE)
    }

    /// One waiting message: who to whom, why, its buttons on a line that stays, then its text, which scrolls.
    fn draw_message(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, m: &Message) -> Drawn<Click> {
        self.drawn.replace(Some(m.id.clone()));
        let g = Style::default().bg(theme.ground);
        let r = crate::chrome::fit(screen, crate::chrome::WIDE_W, u16::MAX);
        let w = r.width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize;
        let room = r.height.saturating_sub(2 * crate::chrome::DIALOG_FRAME.vertical + 2) as usize;
        let verb = if m.state == State::Held(Hold::RouteRequest) { " asks to write to " } else { " → " };
        let waited = format!(" · waited {}", age(m));
        // The names give way, not the wait: the shorter keeps up to half the room whole.
        let (a, b, both) = (m.from.name(), m.to.name(), w.saturating_sub(cells(verb) + cells(&waited)));
        let keep_a = a.chars().count().min(both.saturating_sub(b.chars().count().min(both / 2)));
        let head = format!("{}{verb}{}", cut(a, keep_a), cut(b, both - keep_a));
        let mut lines: Vec<Line<Click>> = vec![vec![(head, g.fg(theme.text).add_modifier(Modifier::BOLD), None), (waited, g.fg(theme.muted), None)]];
        if let State::Held(h) = &m.state {
            lines.extend(crate::menu::wrap(&h.sentence(m), w).into_iter().map(|l| vec![(l, g.fg(theme.warn), None)]));
        }
        lines.push(Vec::new());
        let mut row: Line<Click> = Vec::new();
        for (key, word, click) in buttons(m) {
            if !row.is_empty() && width(&row) + cells(key) + cells(word) + 3 > w {
                lines.push(std::mem::take(&mut row));
            }
            button(&mut row, key, word, click, theme);
        }
        lines.push(row);
        lines.push(vec![("─".repeat(w), g.fg(theme.border), None)]);
        let text = text_lines(&m.text, w);
        let seen = room.saturating_sub(lines.len());
        let from = self.read.place(seen, text.len());
        lines.extend(text.iter().skip(from).take(seen).map(|l| vec![(l.clone(), g.fg(theme.text), None)]));
        let title = if matches!(m.state, State::Held(_)) { "Held message" } else { "Message" };
        let d = draw(buf, r, title, theme, &lines, &Scroll::default(), &[], if self.alone { CLOSE } else { BACK });
        self.read.draw(buf, d.frame, d.close.y.saturating_sub(1), theme);
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
        draw(buf, screen, "Routes map", theme, &lines, &Scroll::default(), &buttons, CLOSE)
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
        out.push(vec![("quick start".into(), g.fg(theme.muted), None)]);
        for (i, step) in crate::mail::QUICK_START.iter().enumerate() {
            for (j, l) in crate::menu::wrap(step, w.saturating_sub(3)).into_iter().enumerate() {
                out.push(vec![(format!("{} {l}", if j == 0 { format!("{}.", i + 1) } else { "  ".into() }), g.fg(theme.text), None)]);
            }
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
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for col in &v.columns {
            let Some(space) = &col.space else { continue };
            let names: Vec<&str> = col.cards.iter().filter(|c| c.pane.is_some()).map(|c| c.name.as_str()).collect();
            if !names.is_empty() {
                groups.push((space, names));
            }
        }
        // the quick start above already says to name a pane
        if groups.is_empty() {
            return Vec::new();
        }
        let find = |s: &str| groups.iter().find(|(n, _)| *n == s);
        match &self.zoom {
            Some((a, b)) if find(a).is_some() && b.as_deref().is_none_or(|b| find(b).is_some()) => {
                let to = b.as_deref().and_then(find).map(|(s, names)| (*s, names.as_slice()));
                self.pair(v, verdict, find(a).unwrap(), to, w, theme)
            }
            _ => self.spaces(v, verdict, &groups, w, theme),
        }
    }

    /// The columns from `start` that fit in `room`, of `n`.
    fn page(&self, n: usize, room: usize) -> std::ops::Range<usize> {
        if n <= room {
            return 0..n;
        }
        let start = self.start.min(n - 1);
        start..(start + room).min(n)
    }

    /// When not every column fits: which do, and the chips that move by a page.
    fn paging(&self, shown: &std::ops::Range<usize>, n: usize, room: usize, w: usize, theme: &Theme) -> Vec<Line<Click>> {
        if shown.len() == n {
            return Vec::new();
        }
        let g = Style::default().bg(theme.ground);
        let fit = if shown.is_empty() { "no column".to_string() } else { format!("columns {}–{}", shown.start + 1, shown.end) };
        let mut out: Vec<Line<Click>> = crate::menu::wrap(&format!("{fit} of {n} fit this width"), w).into_iter().map(|s| vec![(s, g.fg(theme.warn), None)]).collect();
        let mut l = out.pop().unwrap_or_default();
        text(&mut l, " ", g);
        let chips = [(room > 0 && shown.start > 0, "‹ earlier", shown.start.saturating_sub(room)), (room > 0 && shown.end < n, "later ›", shown.end)];
        for (_, word, at) in chips.into_iter().filter(|c| c.0) {
            if width(&l) + cells(word) + 3 > w {
                out.push(std::mem::take(&mut l));
            }
            chip(&mut l, word, None, Some(Click::Start(at)), theme);
        }
        out.push(l);
        out
    }

    fn too_narrow(need: usize, w: usize, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        vec![vec![(format!("the matrix needs {need} columns, this window has {w}: widen the window"), g.fg(theme.warn), None)]]
    }

    /// The column numbers over the cells; `you` last when `you`.
    fn head(lw: usize, numbers: impl Iterator<Item = usize>, you: bool, theme: &Theme) -> Line<Click> {
        let g = Style::default().bg(theme.ground);
        let mut head: Line<Click> = vec![(" ".repeat(lw), g, None)];
        for n in numbers {
            text(&mut head, format!(" {:<8}", n + 1), g.fg(theme.muted));
        }
        if you {
            text(&mut head, format!("│{:<8}", "you"), g.fg(theme.muted));
        }
        head
    }

    /// A row and a column per space, and `you`: a cell sums up every pair of their panes, and a press opens them.
    fn spaces(&self, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, groups: &[(&str, Vec<&str>)], w: usize, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let label = |i: usize| format!("{} {}", i + 1, groups[i].0);
        let lw = (0..groups.len()).map(|i| cells(&label(i))).max().unwrap_or(0);
        let cap = w.saturating_sub(lw) / CELL;
        if cap == 0 {
            return Self::too_narrow(lw + CELL, w, theme);
        }
        let shown = self.page(groups.len(), cap - 1);
        let mut out: Vec<Line<Click>> = self.paging(&shown, groups.len(), cap - 1, w, theme);
        out.push(Self::head(lw, shown.clone(), true, theme));
        let you = vec!["you"];
        for (i, (space, from)) in groups.iter().enumerate() {
            let mut l: Line<Click> = vec![(format!("{:<lw$}", label(i)), g.fg(theme.text).add_modifier(Modifier::BOLD), None)];
            for j in shown.clone().map(Some).chain([None]) {
                let to = j.map_or(&you, |j| &groups[j].1);
                let pairs: Vec<(&str, &str)> = from.iter().flat_map(|f| to.iter().filter(move |t| *t != f).map(move |t| (*f, *t))).collect();
                text(&mut l, if j.is_none() { "│" } else { " " }, g.fg(theme.muted));
                if pairs.is_empty() {
                    text(&mut l, format!("{:<8}", "—"), g.fg(theme.faint));
                    continue;
                }
                let click = Some(Click::Zoom(Some((space.to_string(), j.map(|j| groups[j].0.to_string())))));
                let held = v.waiting.iter().filter(|m| from.contains(&m.from.name()) && to.contains(&m.to.name())).count();
                if held > 0 {
                    let n = if held > 99 { "99+".to_string() } else { held.to_string() };
                    l.push((format!("{:<8}", format!("held {n}")), g.fg(theme.warn), click));
                    continue;
                }
                let verdicts: Vec<Verdict> = pairs.iter().map(|(f, t)| verdict(f, t)).collect();
                let (word, style) = if verdicts.iter().all(|x| x.action == verdicts[0].action) {
                    let bright = if verdicts.iter().any(|x| x.route.is_some()) { Modifier::BOLD } else { Modifier::DIM };
                    (verdicts[0].action.word(), g.fg(action_color(verdicts[0].action, theme)).add_modifier(bright))
                } else {
                    ("mixed", g.fg(theme.text))
                };
                l.push((format!("{word:<8}"), style, click));
            }
            out.push(l);
        }
        let legend = "a cell opens the panes behind it · mixed: they differ · bright: a route decides · faint: the mode · a reply in a thread passes unless a route says no";
        out.extend(crate::menu::wrap(legend, w).into_iter().map(|l| vec![(l, g.fg(theme.faint), None)]));
        out
    }

    /// The panes of space `a` towards those of `to` (None: `you`), and a route for the whole pair.
    fn pair(&self, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, (a, from): &(&str, Vec<&str>), to: Option<(&str, &[&str])>, w: usize, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let mut out: Vec<Line<Click>> = Vec::new();
        let mut l: Line<Click> = Vec::new();
        chip(&mut l, "‹ every space", None, Some(Click::Zoom(None)), theme);
        text(&mut l, format!(" {a} → {}", to.map_or("you", |(b, _)| b)), g.fg(theme.text).add_modifier(Modifier::BOLD));
        out.push(l);
        let (sf, st) = (Scope::Space(a.to_string()), to.map_or(Scope::You, |(b, _)| Scope::Space(b.into())));
        let own = v.routes.iter().find(|r| r.route.from == sf && r.route.to == st && r.left != Some(0)).map(|r| r.route.action);
        let mut l: Line<Click> = Vec::new();
        text(&mut l, "every pane ", g.fg(theme.muted));
        for a in [Some(Action::Allow), Some(Action::Ask), Some(Action::No), None] {
            let lit = match a {
                Some(a) => (own == Some(a)).then(|| Style::default().bg(action_color(a, theme)).fg(theme.ground)),
                None => own.is_none().then(|| theme.cursor()),
            };
            let act = MailAction::SetRoute { from: sf.clone(), to: st.clone(), action: a, until: None };
            chip(&mut l, a.map_or("mode", Action::word), lit, Some(Click::Act(act)), theme);
        }
        out.push(l);
        out.extend(crate::menu::wrap("more specific routes go first; the cells show what each pair gets", w).into_iter().map(|l| vec![(l, g.fg(theme.faint), None)]));
        let names: &[&str] = to.map_or(&["you"], |(_, n)| n);
        if to.is_some() {
            let mut l: Line<Click> = Vec::new();
            text(&mut l, "to ", g.fg(theme.muted));
            for (k, n) in names.iter().enumerate() {
                let word = format!("{} {n}", k + 1);
                if width(&l) + cells(&word) + 3 > w {
                    out.push(std::mem::take(&mut l));
                    text(&mut l, "   ", g);
                }
                chip(&mut l, &word, None, Some(Click::Select(n.to_string())), theme);
            }
            out.push(l);
        }
        let lw = from.iter().map(|n| cells(n)).max().unwrap_or(0);
        let cap = w.saturating_sub(lw) / CELL;
        if cap == 0 {
            out.extend(Self::too_narrow(lw + CELL, w, theme));
            return out;
        }
        let shown = if to.is_some() { self.page(names.len(), cap) } else { 0..1 };
        out.extend(self.paging(&shown, names.len(), cap, w, theme));
        out.push(if to.is_some() { Self::head(lw, shown.clone(), false, theme) } else { Self::head(lw, std::iter::empty(), true, theme) });
        for name in from {
            let chosen = self.selected.as_deref() == Some(*name);
            let style = if chosen { g.fg(theme.accent) } else { g.fg(theme.text) }.add_modifier(Modifier::BOLD);
            let mut l: Line<Click> = vec![(format!("{name:<lw$}"), style, Some(Click::Select(name.to_string())))];
            for t in &names[shown.clone()] {
                text(&mut l, if to.is_none() { "│" } else { " " }, g.fg(theme.muted));
                pane_cell(&mut l, v, verdict, name, t, theme);
            }
            out.push(l);
        }
        let legend = "a name shows its rows · bright: a route decides · faint: the mode · a reply in a thread passes unless a route says no";
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
        if !self.settle(v) {
            return Out::Close;
        }
        // the view moved on since the last frame: the owner has not seen what an action would hit
        if self.message != *self.drawn.borrow() && matches!(k.code, Char('a' | 'k' | 'n' | 'd' | 'h' | 'x' | 'r')) {
            return Out::None;
        }
        let view = self.message.is_some();
        if view {
            match k.code {
                Esc => return self.leave(),
                Up | Down | PageUp | PageDown | Home | End => {
                    self.read.key(k.code);
                    return Out::None;
                }
                Char('a' | 'k' | 'n' | 'd' | 'h' | 'x' | 'r') => {}
                _ => return Out::None,
            }
        }
        let current = if view { self.shown(v) } else { self.waiting(v).1 };
        let id = current.map(|m| m.id.clone()).unwrap_or_default();
        let request = current.is_some_and(|m| m.state == State::Held(Hold::RouteRequest));
        let held = current.is_some_and(|m| matches!(m.state, State::Held(_)));
        let answer = |action| Out::Act(MailAction::AnswerRequest { id: id.clone(), action });
        match k.code {
            Esc if self.selected.take().is_some() => Out::None,
            Esc if self.zoom.take().is_some() => {
                self.start = 0;
                Out::None
            }
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
            Enter if current.is_some() => {
                self.open(id);
                Out::None
            }
            code => {
                self.scroll.key(code);
                Out::None
            }
        }
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
        if self.message.is_some() {
            match ev.kind {
                Down(MouseButton::Left) if d.close.contains((x, y).into()) || !d.frame.contains((x, y).into()) => return self.leave(),
                Down(MouseButton::Left) => match hit {
                    Some(Click::Act(a)) => return Out::Act(a),
                    Some(Click::Reply(id)) => return Out::Reply(id),
                    _ => {}
                },
                kind => _ = self.read.wheel(kind),
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
                    Some(Click::Zoom(z)) => {
                        self.zoom = z;
                        self.start = 0;
                    }
                    Some(Click::Incoming(b)) => self.incoming = b,
                    Some(Click::Act(a)) => return Out::Act(a),
                    Some(Click::Reply(id)) => return Out::Reply(id),
                    Some(Click::Broadcast) => return Out::Broadcast,
                    Some(Click::Topology) => return Out::Topology,
                    Some(Click::MapView) => self.map_view = true,
                    Some(Click::Waiting(id)) => self.open(id),
                    Some(Click::AllThreads) => self.inbox = None,
                    Some(Click::Card(_) | Click::Pan(..)) | None => {}
                }
                Out::None
            }
            kind => {
                self.scroll.wheel(kind);
                Out::None
            }
        }
    }
}

/// A pane's cell towards `to`: `—` for itself, `held N` for waiting mail, else the verdict, whose press gives the pair's next route.
fn pane_cell(l: &mut Line<Click>, v: &MailView, verdict: &dyn Fn(&str, &str) -> Verdict, name: &str, to: &str, theme: &Theme) {
    let g = Style::default().bg(theme.ground);
    if name == to {
        text(l, format!("{:<8}", "—"), g.fg(theme.faint));
        return;
    }
    let waiting: Vec<&Message> = v.waiting.iter().filter(|m| m.from.name() == name && m.to.name() == to).collect();
    if let Some(first) = waiting.first() {
        let n = if waiting.len() > 99 { "99+".to_string() } else { waiting.len().to_string() };
        l.push((format!("{:<8}", format!("held {n}")), g.fg(theme.warn), Some(Click::Waiting(first.id.clone()))));
        return;
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

/// The message's text wrapped to `w`, each line keeping its indent of spaces.
// ceiling: runs of spaces inside a line collapse to one; a character-exact wrap if code alignment matters
fn text_lines(text: &str, w: usize) -> Vec<String> {
    text.lines()
        .flat_map(|l| {
            // ASCII spaces only: one byte and one cell each, so the slice is on a char boundary
            let n = l.bytes().take_while(|&b| b == b' ').count().min(w / 2);
            crate::menu::wrap(&l[n..], w - n).into_iter().map(move |r| format!("{}{r}", " ".repeat(n)))
        })
        .collect()
}

/// A waiting message's buttons: its key, its word and what it does.
fn buttons(m: &Message) -> Vec<(&'static str, &'static str, Click)> {
    let id = m.id.clone();
    match &m.state {
        State::Held(Hold::RouteRequest) => [("a", "allow", Action::Allow), ("k", "ask", Action::Ask), ("n", "no", Action::No)]
            .into_iter()
            .map(|(k, w, action)| (k, w, Click::Act(MailAction::AnswerRequest { id: id.clone(), action })))
            .collect(),
        State::Held(_) => vec![
            ("d", "deliver", Click::Act(MailAction::Deliver(id.clone()))),
            ("h", "deliver, allow 1 hour", Click::Act(MailAction::DeliverAllow(id.clone()))),
            ("x", "drop", Click::Act(MailAction::Drop(id.clone()))),
            ("r", "reply…", Click::Reply(id)),
        ],
        _ => vec![("r", "reply…", Click::Reply(id.clone())), ("x", "drop", Click::Act(MailAction::Drop(id)))],
    }
}

/// A waiting row: who, to whom, why, how long; then its buttons, with keys on the row the keys act on.
fn waiting_row(m: &Message, keys: bool, theme: &Theme) -> Vec<Line<Click>> {
    let g = Style::default().bg(theme.ground);
    let head = match &m.state {
        State::Held(Hold::RouteRequest) => format!("{} asks to write to {} · {}", m.from.name(), m.to.name(), age(m)),
        State::Held(h) => format!("{} → {} · {} · {}", m.from.name(), m.to.name(), h.words(), age(m)),
        _ => format!("{} → {} · {}", m.from.name(), Party::You.name(), age(m)),
    };
    let head_style = if keys { g.fg(theme.text).add_modifier(Modifier::BOLD) } else { g.fg(theme.text) };
    let mut l: Line<Click> = vec![("  ".into(), g, None)];
    for (key, word, click) in buttons(m) {
        if keys {
            button(&mut l, key, word, click, theme);
        } else {
            chip(&mut l, word, None, Some(click), theme);
        }
    }
    vec![vec![(head, head_style, Some(Click::Waiting(m.id.clone())))], l]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::{MailColumn, RouteView, Traffic};
    use crate::mail::{Step, Via};
    use crate::policy::{Files, Isolation, Network, Route};

    #[test]
    fn text_lines_keep_their_indent() {
        assert_eq!(text_lines("fn a() {\n    b(c, d)\n}", 10), ["fn a() {", "    b(c,", "    d)", "}"]);
        assert_eq!(text_lines(&format!("{}x", "\u{a0}".repeat(17)), 66), ["x"], "other whitespace is not an indent");
        assert_eq!(text_lines(&format!("{}x", " ".repeat(40)), 10), ["     x"], "an indent takes at most half the width");
    }

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

    pub(crate) fn verdict(_: &str, _: &str) -> Verdict {
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

    fn norm(l: &str) -> String {
        l.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The paging line and the chips wrapped under it, as one line of words.
    fn paged(text: &[String]) -> String {
        let at = text.iter().position(|l| l.starts_with("columns") || l.starts_with("no column")).expect("a paging line");
        let more = text[at + 1..].iter().take_while(|l| l.contains("earlier") || l.contains("later"));
        norm(&std::iter::once(&text[at]).chain(more).cloned().collect::<Vec<_>>().join(" "))
    }

    /// The window on the panes of space `a` towards `b` (None: `you`).
    fn zoomed(a: &str, b: Option<&str>) -> Messages {
        Messages { zoom: Some((a.into(), b.map(Into::into))), ..Messages::new() }
    }

    fn zoom(a: &str, b: Option<&str>) -> Click {
        Click::Zoom(Some((a.into(), b.map(Into::into))))
    }

    #[test]
    fn a_space_cell_sums_up_its_pairs_and_opens_them() {
        let theme = Theme::dark();
        let mut v = view();
        v.waiting.clear();
        // vault is another account: open mode asks for it
        let by_pane = |from: &str, to: &str| match (from, to) {
            ("vault", _) => Verdict { action: Action::Ask, why: "open mode, another account".into(), route: None },
            ("planner", "reader") => Verdict { action: Action::Allow, why: "route".into(), route: Some(0) },
            _ => Verdict { action: Action::Allow, why: "open mode".into(), route: None },
        };
        let (lines, _) = Messages::new().lines(&v, &by_pane, 120, &theme);
        let text = texts(&lines);
        assert!(text.iter().any(|l| l.contains(" 1        2       │you")), "spaces numbered, then you:\n{text:#?}");
        let (word, style, hit) = cell(&lines, "1 work", 0);
        assert_eq!((word.trim_end(), *hit == Some(zoom("work", Some("work")))), ("allow", true));
        assert!(style.add_modifier.contains(Modifier::DIM), "only the mode decides: faint");
        let (word, style, _) = cell(&lines, "1 work", 1);
        assert_eq!(word.trim_end(), "allow");
        assert!(style.add_modifier.contains(Modifier::BOLD), "a route decides one of the pairs: bright");
        let (word, _, hit) = cell(&lines, "2 research", 0);
        assert_eq!((word.trim_end(), hit.clone()), ("mixed", Some(zoom("research", Some("work")))), "reader allow, vault ask");
        assert_eq!(cell(&lines, "2 research", 2).2, Some(zoom("research", None)), "the you column");
        // held mail between the spaces counts once per message
        v.waiting.push(msg("m1", "coder", Some("reader"), State::Held(Hold::Route), 120));
        v.waiting.push(msg("m4", "planner", Some("vault"), State::Held(Hold::Route), 120));
        let (lines, _) = Messages::new().lines(&v, &by_pane, 120, &theme);
        let (word, style, hit) = cell(&lines, "1 work", 1);
        assert_eq!((word.trim_end(), style.fg, hit.clone()), ("held 2", Some(theme.warn), Some(zoom("work", Some("research")))));
        // a space of one pane has no pair inside it
        let (lines, _) = Messages::new().lines(&full(), &verdict, 120, &theme);
        assert_eq!(cell(&lines, "6 s5", 5).0.trim_end(), "—");
        assert_eq!(cell(&lines, "6 s5", 5).2, None);
    }

    #[test]
    fn a_press_on_a_cell_opens_the_pair_and_esc_or_every_space_comes_back() {
        let v = view();
        let mut m = Messages::new();
        let (d, _) = drawn(&m, &v, 140, 60);
        assert_eq!(press(&mut m, &d, &zoom("work", Some("research"))), Out::None);
        let (d, shown) = drawn(&m, &v, 140, 60);
        assert!(shown.contains("‹ every space") && shown.contains("work → research") && shown.contains("1 reader") && shown.contains("2 vault"), "{shown}");
        assert_eq!(press(&mut m, &d, &Click::Zoom(None)), Out::None);
        assert_eq!(m.zoom, None);
        let (d, _) = drawn(&m, &v, 140, 60);
        press(&mut m, &d, &zoom("work", Some("research")));
        let (d, _) = drawn(&m, &v, 140, 60);
        press(&mut m, &d, &Click::Select("planner".into()));
        m.start = 1;
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Esc), Out::None, "Esc clears the chosen pane first");
        assert!(m.selected.is_none() && m.zoom.is_some());
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Esc), Out::None, "then the pair");
        assert_eq!((m.zoom.clone(), m.start), (None, 0));
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Esc), Out::Close);
    }

    #[test]
    fn every_pane_of_the_pair_takes_one_space_route_and_the_cells_keep_the_verdicts() {
        let theme = Theme::dark();
        let mut v = view();
        let space = |s: &str| Scope::Space(s.into());
        let set = |a| MailAction::SetRoute { from: space("work"), to: space("research"), action: a, until: None };
        let acts = |m: &Messages, v: &MailView| -> Vec<MailAction> {
            m.lines(v, &verdict, 120, &theme).0.into_iter().flatten().filter_map(|(_, _, h)| if let Some(Click::Act(a)) = h { Some(a) } else { None }).collect()
        };
        let m = zoomed("work", Some("research"));
        for a in [Some(Action::Allow), Some(Action::Ask), Some(Action::No), None] {
            assert!(acts(&m, &v).contains(&set(a)), "{a:?}");
        }
        assert!(acts(&zoomed("work", None), &v).contains(&MailAction::SetRoute { from: space("work"), to: Scope::You, action: Some(Action::No), until: None }));
        // the owner's space route is lit; a more specific route still decides its cell
        v.routes.push(RouteView { index: 5, route: Route { from: space("work"), to: space("research"), action: Action::Allow, until: None }, risk: false, left: None });
        let shadow = |from: &str, _: &str| Verdict { action: if from == "planner" { Action::No } else { Action::Allow }, why: "route".into(), route: Some(if from == "planner" { 6 } else { 5 }) };
        let (lines, _) = m.lines(&v, &shadow, 120, &theme);
        let lit = lines.iter().flatten().find(|(t, _, h)| t.contains("allow") && *h == Some(Click::Act(set(Some(Action::Allow))))).unwrap();
        assert_eq!(lit.1.bg, Some(action_color(Action::Allow, &theme)), "the space route's chip is lit");
        assert_eq!(cell(&lines, "planner", 0).0.trim_end(), "no", "planner → space:research no goes first");
        assert!(texts(&lines).iter().any(|l| l == "more specific routes go first; the cells show what each pair gets"));
    }

    #[test]
    fn the_pair_shows_the_verdict_of_each_pane_bright_for_a_route_and_faint_for_the_mode() {
        let theme = Theme::dark();
        let v = view();
        let by_route = |from: &str, to: &str| match (from, to) {
            ("planner", "reader") => Verdict { action: Action::Allow, why: "route".into(), route: Some(0) },
            ("coder", "reader") => Verdict { action: Action::Ask, why: "route".into(), route: Some(1) },
            _ => Verdict { action: Action::No, why: "open mode".into(), route: None },
        };
        let (lines, _) = zoomed("work", Some("research")).lines(&v, &by_route, 120, &theme);
        // columns: 1 reader, 2 vault
        let (word, style, _) = cell(&lines, "planner", 0);
        assert_eq!(word.trim_end(), "allow");
        assert!(style.add_modifier.contains(Modifier::BOLD) && !style.add_modifier.contains(Modifier::DIM), "a route decides: bright");
        let (word, style, _) = cell(&lines, "planner", 1);
        assert_eq!(word.trim_end(), "no");
        assert!(style.add_modifier.contains(Modifier::DIM), "the mode decides: faint");
        assert_eq!(cell(&lines, "coder", 0).0.trim_end(), "held 1", "coder → reader has one message waiting");
        let (lines, _) = zoomed("work", Some("work")).lines(&v, &by_route, 120, &theme);
        assert_eq!(cell(&lines, "planner", 0).0.trim_end(), "—", "the diagonal");
    }

    #[test]
    fn a_held_cell_counts_and_selects_the_first_waiting_message() {
        let theme = Theme::dark();
        let mut v = view();
        v.waiting.push(msg("m4", "coder", Some("reader"), State::Held(Hold::Route), 10));
        let m = zoomed("work", Some("research"));
        let (lines, _) = m.lines(&v, &verdict, 120, &theme);
        let (word, style, hit) = cell(&lines, "coder", 0);
        assert_eq!(word.trim_end(), "held 2");
        assert_eq!(style.fg, Some(theme.warn));
        assert_eq!(*hit, Some(Click::Waiting("m1".into())), "the first in waiting order");
        for _ in 0..120 {
            v.waiting.push(msg("mx", "coder", Some("reader"), State::Held(Hold::Route), 10));
        }
        let (lines, _) = m.lines(&v, &verdict, 120, &theme);
        assert_eq!(cell(&lines, "coder", 0).0.trim_end(), "held 99+");
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
        let pair = |a, b, v: &MailView| zoomed(a, b).lines(v, &verdict, 120, &theme).0;
        assert_eq!(act(&pair("work", Some("research"), &v), "planner", 0), set("planner", "reader", Some(Action::Ask), Some(1_000)), "a timed allow becomes ask with the same time");
        assert_eq!(act(&pair("work", Some("work"), &v), "planner", 1), set("planner", "coder", Some(Action::Allow), None), "none → allow");
        assert_eq!(act(&pair("work", None, &v), "planner", 0), set("planner", "you", Some(Action::Allow), None));
        v.routes[0].route.action = Action::No;
        assert_eq!(act(&pair("work", Some("research"), &v), "planner", 0), set("planner", "reader", None, None), "no → none");
        let mut m = zoomed("work", Some("research"));
        let (d, _) = drawn(&m, &v, 140, 60);
        assert_eq!(press(&mut m, &d, &Click::Act(set("planner", "reader", None, None))), Out::Act(set("planner", "reader", None, None)));
    }

    #[test]
    fn a_click_on_a_name_gives_its_rows_both_ways() {
        let theme = Theme::dark();
        let v = view();
        let mut m = zoomed("work", Some("research"));
        let (d, _) = drawn(&m, &v, 140, 60);
        assert_eq!(press(&mut m, &d, &Click::Select("planner".into())), Out::None);
        assert_eq!(m.selected.as_deref(), Some("planner"));
        let (d, _) = drawn(&m, &v, 140, 60);
        press(&mut m, &d, &Click::Select("vault".into()));
        assert_eq!(m.selected.as_deref(), Some("vault"), "a recipient on the `to` line too");
        m.selected = Some("planner".into());
        let acts = |m: &Messages| -> Vec<MailAction> {
            m.lines(&v, &verdict, 120, &theme).0.into_iter().flatten().filter_map(|(_, _, h)| if let Some(Click::Act(a)) = h { Some(a) } else { None }).collect()
        };
        let route = |f: &str, t: Scope, a| MailAction::SetRoute { from: scope_of(f), to: t, action: a, until: None };
        let shown = texts(&m.lines(&v, &verdict, 120, &theme).0);
        assert!(shown.iter().any(|l| l.contains("where it can write")), "{shown:#?}");
        assert!(acts(&m).contains(&route("planner", Scope::Space("research".into()), None)), "→ every pane in space research [mode]");
        m.incoming = true;
        assert!(acts(&m).contains(&route("reader", Scope::Pane("planner".into()), Some(Action::No))), "← reader [no]");
        // opened on a pane from outside, the rows show under the spaces
        let on = Messages::on(Some("planner".into()), false);
        assert!(texts(&on.lines(&v, &verdict, 120, &theme).0).iter().any(|l| l.contains("where it can write")));
    }

    #[test]
    fn the_quick_start_follows_the_controls_and_their_explanation() {
        let said = texts(&Messages::new().lines(&view(), &verdict, 140, &Theme::dark()).0);
        let at = said.iter().position(|l| l == "quick start").expect("a quick start");
        assert!(said[..at].iter().any(|l| l.contains("Broadcast…")) && said[..at].iter().any(|l| l.starts_with("no route:")), "under the buttons and the mode's line: {said:?}");
        let steps: Vec<String> = crate::mail::QUICK_START.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect();
        assert_eq!(said[at + 1..at + 4], steps[..]);
    }

    #[test]
    fn the_mode_is_explained_and_strict_mode_lets_a_pane_write_to_you_only() {
        let theme = Theme::dark();
        let mut v = view();
        let said = |v: &MailView| texts(&Messages::new().lines(v, &verdict, 200, &theme).0);
        assert!(said(&v).iter().any(|l| l == "no route: a pane may write to you and to its own account; to another account, you are asked"));
        assert!(said(&v).iter().any(|l| l == "a cell opens the panes behind it · mixed: they differ · bright: a route decides · faint: the mode · a reply in a thread passes unless a route says no"));
        v.mode = Mode::Strict;
        assert!(said(&v).iter().any(|l| l == "no route: a pane may write to you only"));
        let strict = |_: &str, to: &str| Verdict { action: if to == "you" { Action::Allow } else { Action::No }, why: "strict mode".into(), route: None };
        let (lines, _) = zoomed("work", None).lines(&v, &strict, 140, &theme);
        let (you, style, _) = cell(&lines, "planner", 0);
        assert_eq!(you.trim_end(), "allow");
        assert!(style.add_modifier.contains(Modifier::DIM), "faint: the mode decides");
        let (lines, _) = zoomed("work", Some("work")).lines(&v, &strict, 140, &theme);
        assert_eq!(cell(&lines, "planner", 1).0.trim_end(), "no");
    }

    #[test]
    fn spaces_that_do_not_fit_are_paged_and_the_window_says_so() {
        let theme = Theme::dark();
        let mut v = full();
        let mut m = Messages::new();
        // label "1 s0" is 4 wide: 4 + 3 × 9 holds two spaces and you
        let w = 4 + 3 * CELL;
        let line = |m: &Messages| -> String { paged(&texts(&m.lines(&v, &verdict, w, &theme).0)) };
        assert_eq!(line(&m), "columns 1–2 of 6 fit this width later ›");
        m.start = 2;
        assert_eq!(line(&m), "columns 3–4 of 6 fit this width ‹ earlier later ›");
        let (lines, _) = m.lines(&v, &verdict, w, &theme);
        assert!(lines.iter().flatten().any(|(_, _, h)| *h == Some(Click::Start(0))), "earlier goes back a page");
        m.start = 9;
        assert_eq!(line(&m), "columns 6–6 of 6 fit this width ‹ earlier");
        // room for you only, then for nothing
        assert!(norm(&texts(&m.lines(&v, &verdict, 4 + CELL, &theme).0).join(" ")).contains("no column of 6 fit this width"));
        assert!(texts(&m.lines(&v, &verdict, 3 + CELL, &theme).0).iter().any(|l| l == "the matrix needs 13 columns, this window has 12: widen the window"));
        // a route to a name no pane holds is a chip under the matrix, not a cell
        v.routes.push(RouteView { index: 5, route: Route { from: Scope::parse("p1").unwrap(), to: Scope::parse("ghost").unwrap(), action: Action::Allow, until: None }, risk: false, left: None });
        let (lines, _) = Messages::new().lines(&v, &verdict, 140, &theme);
        assert!(lines.iter().flatten().any(|(t, _, h)| *h == Some(Click::Line(5)) && t.contains("ghost")));
        assert!(texts(&lines).iter().any(|l| l.starts_with("routes ")));
    }

    #[test]
    fn the_paging_chips_wrap_in_a_narrow_window_and_a_press_turns_the_page() {
        let theme = Theme::dark();
        let mut v = full();
        let w = 4 + 2 * CELL;
        let (lines, _) = Messages::new().lines(&v, &verdict, w, &theme);
        let paging = lines.iter().filter(|l| l.iter().any(|(t, _, _)| t.contains("columns") || t.contains("width") || t.contains("later") || t.contains("earlier")));
        assert!(paging.clone().count() >= 2 && paging.clone().all(|l| width(l) <= w), "{:#?}", texts(&lines));
        assert!(lines.iter().flatten().any(|(_, _, h)| *h == Some(Click::Start(1))), "later › is there");
        // twelve spaces do not fit the window at 140 cells: a press on later › moves the columns
        v.columns = (0..12).map(|s| MailColumn { space: Some(format!("s{s}")), cards: vec![card(&format!("p{s}"), Some(s), "main", None, Isolation::default())] }).collect();
        let mut m = Messages::new();
        let (d, _) = drawn(&m, &v, 140, 60);
        let later = d.hits.iter().map(|(_, c)| c.clone()).find(|c| matches!(c, Click::Start(i) if *i > 0)).expect("later ›");
        assert_eq!(press(&mut m, &d, &later), Out::None);
        assert!(matches!(later, Click::Start(i) if m.start == i));
    }

    #[test]
    fn an_expired_space_route_is_not_lit_and_a_gone_space_closes_its_pair() {
        let theme = Theme::dark();
        let mut v = view();
        let space = |s: &str| Scope::Space(s.into());
        v.routes.push(RouteView { index: 5, route: Route { from: space("work"), to: space("research"), action: Action::No, until: Some(1) }, risk: false, left: Some(0) });
        let mut m = zoomed("work", Some("research"));
        let (lines, _) = m.lines(&v, &verdict, 120, &theme);
        let no = MailAction::SetRoute { from: space("work"), to: space("research"), action: Some(Action::No), until: None };
        let chip = lines.iter().flatten().find(|(_, _, h)| *h == Some(Click::Act(no.clone()))).unwrap();
        assert_ne!(chip.1.bg, Some(action_color(Action::No, &theme)), "expired: not lit");
        m.start = 3;
        assert!(m.settle(&v));
        assert!(m.zoom.is_some(), "both spaces are there");
        v.columns.retain(|c| c.space.as_deref() != Some("research"));
        assert!(m.settle(&v));
        assert_eq!((m.zoom.clone(), m.start), (None, 0), "research is gone: back to the spaces");
    }

    #[test]
    fn an_expired_route_is_not_cycled_and_the_cell_starts_again_from_allow() {
        let theme = Theme::dark();
        let mut v = view();
        v.routes[0].route.until = Some(1);
        v.routes[0].left = Some(0);
        let (lines, _) = zoomed("work", Some("research")).lines(&v, &verdict, 120, &theme);
        let want = MailAction::SetRoute { from: scope_of("planner"), to: scope_of("reader"), action: Some(Action::Allow), until: None };
        assert_eq!(cell(&lines, "planner", 0).2, Some(Click::Act(want)));
    }

    #[test]
    fn a_pair_wider_than_the_room_is_paged_by_panes() {
        let theme = Theme::dark();
        let mut v = full();
        v.columns = vec![MailColumn { space: Some("big".into()), cards: (1..=11).map(|n| card(&format!("p{n}"), Some(n), "main", None, Isolation::default())).collect() }];
        let mut m = zoomed("big", Some("big"));
        // names 3 wide: 3 + 4 × 9 holds four panes
        let text = texts(&m.lines(&v, &verdict, 3 + 4 * CELL, &theme).0);
        assert_eq!(paged(&text), "columns 1–4 of 11 fit this width later ›");
        m.start = 4;
        let text = texts(&m.lines(&v, &verdict, 3 + 4 * CELL, &theme).0);
        assert_eq!(paged(&text), "columns 5–8 of 11 fit this width ‹ earlier later ›");
        let text = texts(&m.lines(&v, &verdict, 3 + CELL - 1, &theme).0);
        assert!(text.iter().any(|l| l == "the matrix needs 12 columns, this window has 11: widen the window"), "{text:#?}");
        assert!(text.iter().any(|l| l.contains("‹ every space")), "the way back stays");
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
    fn forty_panes_in_eight_spaces_fit_whole_at_120_cells() {
        let theme = Theme::dark();
        let mut v = full();
        let mut n = 0;
        v.columns = (0..8)
            .map(|s| MailColumn {
                space: Some(format!("space-{s}")),
                cards: (0..5)
                    .map(|_| {
                        n += 1;
                        card(&format!("p{n}"), Some(n), "main", None, Isolation::default())
                    })
                    .collect(),
            })
            .collect();
        let (lines, _) = Messages::new().lines(&v, &verdict, 120, &theme);
        let text = texts(&lines);
        assert!(!text.iter().any(|l| l.starts_with("columns")), "no paging: {text:#?}");
        let row = lines.iter().find(|l| l.first().is_some_and(|(t, _, _)| t.trim_end() == "1 space-0")).unwrap();
        assert_eq!(row.len(), 1 + 2 * 9, "the label, then a separator and a cell for each of 8 spaces and you");
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
        drawn(&m, &v, 140, 60);
        assert_eq!(key(&mut m, 'a'), Out::Act(MailAction::AnswerRequest { id: "m2".into(), action: Action::Allow }));
        assert_eq!(key(&mut m, 'd'), Out::None);
        let mut m = Messages::at("m3".into());
        drawn(&m, &v, 140, 60);
        assert_eq!(key(&mut m, 'r'), Out::Reply("m3".into()));
        assert_eq!(key(&mut m, 'm'), Out::None, "the held view has only its message's keys");
        assert_eq!(key(&mut Messages::new(), 'm'), Out::Act(MailAction::SetMode(Mode::Strict)));
    }

    /// The held view on `id`, settled against `v` as a frame would.
    fn shown_on(id: &str, v: &MailView) -> Messages {
        let mut m = Messages::at(id.into());
        assert!(m.settle(v));
        m
    }

    #[test]
    fn the_window_and_a_held_message_scroll_as_every_window() {
        let (screen, theme) = (Rect::new(0, 0, 160, 24), Theme::dark());
        let mut v = view();
        v.waiting[0].text = "word ".repeat(2000);
        for (name, m) in [("Messages", Messages::new()), ("Held message", shown_on("m1", &v))] {
            let m = std::cell::RefCell::new(m);
            crate::chrome::tests::scrolls(name, screen, false, &|b| _ = m.borrow().draw(b, screen, &theme, &v, &verdict), &|k| _ = m.borrow_mut().key(crossterm::event::KeyEvent::from(k), &v), &|kind| {
                let d = m.borrow().draw(&mut Buffer::empty(screen), screen, &theme, &v, &verdict);
                m.borrow_mut().mouse(crossterm::event::MouseEvent { kind, column: 80, row: 12, modifiers: crossterm::event::KeyModifiers::NONE }, &d);
            });
        }
    }

    #[test]
    fn the_held_view_fits_80x24_with_every_button_over_a_64_kib_text() {
        let mut v = view();
        v.waiting[0].state = State::Held(Hold::Limit("hops".into()));
        v.waiting[0].hops = ["coder", "reader"].repeat(5).into_iter().map(String::from).collect();
        v.waiting[0].text = "word ".repeat(crate::mail::MESSAGE_BYTES / 5);
        let mut m = shown_on("m1", &v);
        let (d, shown) = drawn(&m, &v, 80, 24);
        assert_eq!((d.frame.width, d.frame.height), (70, 20));
        assert!(shown.contains("coder → reader · waited 2m") && shown.contains("coder and reader have written to each other 8 times"), "{shown}");
        assert!(shown.contains("[d] deliver  [h] deliver, allow 1 hour  [x] drop  [r] reply…") && shown.contains("[Esc] close"), "{shown}");
        assert!(shown.contains("word word") && shown.contains("lines 1–"), "{shown}");
        for (h, out) in [
            (Click::Act(MailAction::Deliver("m1".into())), Out::Act(MailAction::Deliver("m1".into()))),
            (Click::Act(MailAction::DeliverAllow("m1".into())), Out::Act(MailAction::DeliverAllow("m1".into()))),
            (Click::Act(MailAction::Drop("m1".into())), Out::Act(MailAction::Drop("m1".into()))),
            (Click::Reply("m1".into()), Out::Reply("m1".into())),
        ] {
            assert_eq!(press(&mut m, &d, &h), out);
        }
        let wheel = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::ScrollDown, column: 40, row: 15, modifiers: crossterm::event::KeyModifiers::NONE };
        m.mouse(wheel, &d);
        let (_, after) = drawn(&m, &v, 80, 24);
        assert!(after.contains("lines 4–") && after.contains("[d] deliver"), "the text scrolls, the buttons stay: {after}");
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('h')), Out::Act(MailAction::DeliverAllow("m1".into())), "keys repeat the buttons");
        let close = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: d.close.x, row: d.close.y, modifiers: crossterm::event::KeyModifiers::NONE };
        assert_eq!(m.mouse(close, &d), Out::Close, "opened from outside the window: close");
    }

    #[test]
    fn long_names_are_cut_and_the_wait_stays_in_the_head() {
        let mut v = view();
        let long = "a-very-long-pane-name-".repeat(3);
        v.waiting[0] = msg("m1", &long, Some(&long), State::Held(Hold::Route), 120);
        v.waiting[1] = msg("m2", &long, Some("planner"), State::Held(Hold::RouteRequest), 60);
        let (_, shown) = drawn(&shown_on("m1", &v), &v, 80, 24);
        assert!(shown.contains("a-very-long-pane-name-a-v… → a-very-long-pane-name-a-… · waited 2m │"), "{shown}");
        let (_, shown) = drawn(&shown_on("m2", &v), &v, 80, 24);
        assert!(shown.contains("a-very-long-pane-name-a-very… asks to write to planner · waited 1m │"), "the short name stays whole: {shown}");
    }

    #[test]
    fn the_view_of_a_request_and_of_a_message_to_you() {
        let v = view();
        let mut m = shown_on("m2", &v);
        let (d, shown) = drawn(&m, &v, 80, 24);
        assert!(shown.contains("vault asks to write to planner") && shown.contains("allow lets it write") && shown.contains("[a] allow  [k] ask  [n] no"), "{shown}");
        assert_eq!(press(&mut m, &d, &Click::Act(MailAction::AnswerRequest { id: "m2".into(), action: Action::No })), Out::Act(MailAction::AnswerRequest { id: "m2".into(), action: Action::No }));
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('d')), Out::None);
        let m = shown_on("m3", &v);
        let (_, shown) = drawn(&m, &v, 80, 24);
        assert!(shown.contains("reader → you · waited") && shown.contains("[r] reply…  [x] drop") && !shown.contains("[d] deliver"), "{shown}");
        assert!(!shown.contains("waits for you") && shown.contains("hi"), "no reason, the text: {shown}");
    }

    #[test]
    fn every_way_in_from_the_window_opens_the_view_and_escape_comes_back() {
        let mut v = view();
        v.waiting.push(msg("m4", "coder", Some("reader"), State::Held(Hold::Route), 10));
        let mut m = Messages::new();
        let (d, _) = drawn(&m, &v, 140, 60);
        assert_eq!(press(&mut m, &d, &Click::Waiting("m1".into())), Out::None);
        assert!(m.settle(&v));
        let (d, shown) = drawn(&m, &v, 140, 60);
        assert!(shown.contains("Held message") && shown.contains("[Esc] back"), "{shown}");
        let back = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: d.close.x, row: d.close.y, modifiers: crossterm::event::KeyModifiers::NONE };
        assert_eq!(m.mouse(back, &d), Out::None);
        assert!(drawn(&m, &v, 140, 60).1.contains("waiting for you"), "back in the window");
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Enter), Out::None);
        assert!(m.settle(&v) && drawn(&m, &v, 140, 60).1.contains("Held message"), "Enter opens the current row");
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Esc), Out::None);
        assert!(m.message.is_none(), "Escape: back");
        // after an action the next held message of the pair, then back where it was opened from
        m.message = Some("m1".into());
        assert!(m.settle(&v));
        v.waiting.retain(|x| x.id != "m1");
        assert!(m.settle(&v) && m.message.as_deref() == Some("m4"), "the next of coder → reader");
        v.waiting.push(msg("m5", "coder", Some("reader"), State::Held(Hold::Route), 10));
        v.waiting.retain(|x| x.id != "m4");
        for _ in 0..2 {
            assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('d')), Out::None, "m5 was never drawn: d does nothing");
        }
        drawn(&m, &v, 140, 60);
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('d')), Out::Act(MailAction::Deliver("m5".into())), "drawn, it acts");
        v.waiting.retain(|x| x.id != "m5");
        assert!(m.settle(&v) && m.message.is_none(), "none left: the window");
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('a')), Out::None, "the list was not drawn yet");
        drawn(&m, &v, 140, 60);
        assert_eq!(key(&mut m, &v, crossterm::event::KeyCode::Char('a')), Out::Act(MailAction::AnswerRequest { id: "m2".into(), action: Action::Allow }), "drawn, its keys act");
        let mut alone = shown_on("m2", &v);
        v.waiting.retain(|x| x.id != "m2");
        assert!(!alone.settle(&v), "opened from the sidebar: nothing left closes the window");
    }
}
