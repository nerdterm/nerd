//! A subagent's run: its transcript once there is one, else its tool calls as the hook logged
//! them, two lines each as Claude Code prints them. A section of its agent's pane, split off on
//! the right; read-only.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Clear, Widget};

use crate::hook::{Outcome, Sub, SubState};
use crate::layout::PaneId;
use crate::render::Theme;

/// The section's narrowest, and the least of its pane it leaves in sight.
pub const MIN_W: u16 = 12;
pub const PANE_MIN_W: u16 = 20;

pub struct Run {
    pub pane: PaneId,
    pub id: String,
    /// Lines scrolled up from the newest, and columns scrolled right.
    scroll: usize,
    hscroll: usize,
    /// A press on the seam, dragging the width until release.
    resizing: bool,
    /// A selection in screen cells, as the file preview keeps one.
    sel: Option<crate::select::Span>,
    selecting: bool,
    /// Read so far; None until the file is found, and the hook's log shows meanwhile.
    pub transcript: Option<crate::transcript::Transcript>,
}

/// What the section's last frame put where, and what it still owes: its draw writes it, its mouse reads it.
#[derive(Default)]
pub struct RunView {
    /// The list's length, its longest line and the room it had, for the wheel.
    len: usize,
    widest: usize,
    visible: usize,
    room: usize,
    /// Where it stood, its pane's frame, and its buttons.
    area: Rect,
    parent: Rect,
    zoom_at: Rect,
    close_at: Rect,
    /// A finished selection: the next frame takes its text off the cells.
    copy: bool,
}

impl RunView {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.area.contains((x, y).into())
    }

    /// The seam takes a cell of slack on each side, as a pane seam does; there is none while zoomed.
    fn on_seam(&self, x: u16, y: u16) -> bool {
        self.parent != Rect::default() && y >= self.area.y && y < self.area.bottom() && x + 1 >= self.area.x && x <= self.area.x + 1 && self.area.width > 0
    }

    /// The pointer over (x, y): the seam drags, the buttons click, the rest is text to select.
    pub fn pointer(&self, x: u16, y: u16) -> Option<&'static str> {
        use crate::pointer::*;
        let at = (x, y).into();
        if self.on_seam(x, y) {
            Some(EW)
        } else if self.zoom_at.contains(at) || self.close_at.contains(at) {
            Some(POINTER)
        } else {
            self.area.contains(at).then_some(TEXT)
        }
    }

    pub fn hide(&mut self) {
        self.area = Rect::default();
        self.parent = Rect::default();
        self.zoom_at = Rect::default();
        self.close_at = Rect::default();
    }
}

/// What a mouse event in the section asks of the app.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    Nothing,
    Close,
    Zoom,
    /// The seam moved: the section's new width.
    Width(u16),
}

/// A row of the list: a call's head (mark, tool, target), its result under `⎿`, or how the run ended.
enum Line<'a> {
    Call(&'static str, Color, &'a str, &'a str),
    Result(String),
    End(&'static str, Color),
}

impl Line<'_> {
    /// The row as coloured pieces, from its first column.
    fn pieces(&self, theme: &Theme) -> Vec<(String, Style)> {
        let ground = Style::default().bg(theme.ground);
        match self {
            Line::Call(mark, tone, tool, target) => {
                let mut v = vec![(format!("{mark} "), ground.fg(*tone)), (tool.to_string(), ground.fg(theme.text).add_modifier(Modifier::BOLD))];
                if !target.is_empty() {
                    v.push((format!("({target})"), ground.fg(theme.text)));
                }
                v
            }
            Line::Result(text) => vec![(format!("  \u{23bf} {text}"), ground.fg(theme.muted))],
            Line::End(text, tone) => vec![(text.to_string(), ground.fg(*tone))],
        }
    }
}

/// How the run ended, under its last call; nothing while it runs.
fn end_of(s: &Sub, now: u64, theme: &Theme) -> Option<Line<'static>> {
    let failed = s.calls.last().is_some_and(|c| c.outcome == Outcome::Failed);
    match s.state(now) {
        SubState::Done if failed => Some(Line::End("failed", theme.bad)),
        SubState::Done => Some(Line::End("done", theme.ok)),
        SubState::Failed => Some(Line::End("failed", theme.bad)),
        SubState::Unknown => Some(Line::End("no report", theme.faint)),
        SubState::Running => None,
    }
}

impl Run {
    pub fn new(pane: PaneId, id: String) -> Self {
        Self {
            pane,
            id,
            scroll: 0,
            hscroll: 0,
            resizing: false,
            sel: None,
            selecting: false,
            transcript: None,
        }
    }

    /// A read of the transcript; true when it showed something new. Scrolled up, the view keeps its place.
    pub fn take(&mut self, f: crate::transcript::Found) -> bool {
        let t = self.transcript.get_or_insert_with(Default::default);
        let before = t.rows.len();
        if !t.take(f) {
            return false;
        }
        if self.scroll > 0 {
            self.scroll += t.rows.len().saturating_sub(before);
        }
        true
    }

    /// Whether a mouse event is the section's: inside it, on its seam, or the rest of a drag it started.
    pub fn claims(&self, v: &RunView, ev: &MouseEvent) -> bool {
        let (x, y) = (ev.column, ev.row);
        let drag = !matches!(ev.kind, MouseEventKind::Down(_)) && (self.resizing || self.selecting);
        drag || v.contains(x, y) || v.on_seam(x, y)
    }

    /// A press anywhere else ends the selection, as it does in a pane.
    pub fn clear_selection(&mut self) {
        self.sel = None;
        self.selecting = false;
    }

    /// Draws into `r`: the right-hand part of its pane `parent`, split off by a seam in the pane's
    /// border colour, or the whole screen while zoomed (`parent` None: no seam to drag). `framed`: the
    /// pane draws a frame whose outer edges the section shares; otherwise it draws a box of its own.
    #[allow(clippy::too_many_arguments)]
    /// Returns the text of a selection finished since the last frame, once.
    pub fn draw(&self, v: &mut RunView, buf: &mut Buffer, r: Rect, parent: Option<Rect>, framed: bool, border: Color, focused: bool, pane: &str, theme: &Theme, sub: Option<&Sub>, cut: bool, now: u64) -> Option<String> {
        v.hide();
        let r = r.intersection(buf.area);
        if r.width < MIN_W || r.height < 5 {
            return None;
        }
        v.area = r;
        v.parent = parent.unwrap_or_default();
        let ground = Style::default().bg(theme.ground);
        let line = ground.fg(border);
        let (right, bottom) = (r.right() - 1, r.bottom() - 1);
        let inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
        Clear.render(inner, buf);
        buf.set_style(inner, ground);
        // the seam, and the rest of the box only where no pane frame is under it
        buf.set_string(r.x, r.y, if framed { "\u{252c}" } else { "\u{250c}" }, line);
        buf.set_string(r.x, bottom, if framed { "\u{2534}" } else { "\u{2514}" }, line);
        for y in r.y + 1..bottom {
            buf.set_string(r.x, y, "\u{2502}", line);
            if !framed {
                buf.set_string(right, y, "\u{2502}", line);
            }
        }
        buf.set_string(r.x + 1, r.y, "\u{2500}".repeat(r.width as usize - 2), line);
        if !framed {
            buf.set_string(r.x + 1, bottom, "\u{2500}".repeat(r.width as usize - 2), line);
            buf.set_string(right, r.y, "\u{2510}", line);
            buf.set_string(right, bottom, "\u{2518}", line);
        }
        // the title as the pane's own, and ⊡ · × where the file tree has them
        let room = (r.width as usize).saturating_sub(12);
        let title = match sub {
            None => format!(" {pane} \u{b7} subagent "),
            Some(s) => {
                let end = s.stopped.unwrap_or(if s.state(now) == SubState::Unknown { s.last } else { now });
                let took = crate::sidebar::ago(std::time::Duration::from_secs(end.saturating_sub(s.started)));
                let bare = format!(" {pane} \u{b7} {} \u{b7} {took} ", s.kind);
                // the task gives way first, so the time stays in sight
                let left = room.saturating_sub(bare.chars().count() + 3);
                let task: String = if s.task.chars().count() <= left { s.task.clone() } else { s.task.chars().take(left.saturating_sub(1)).chain(['\u{2026}']).collect() };
                if task.is_empty() || left < 2 { bare } else { format!(" {pane} \u{b7} {} \u{b7} {task} \u{b7} {took} ", s.kind) }
            }
        };
        let name = ground.patch(theme.title(focused));
        buf.set_stringn(r.x + 2, r.y, &title, room, name);
        buf.set_string(right - 8, r.y, " \u{22a1} \u{b7} \u{d7} ", ground.fg(theme.muted));
        v.zoom_at = Rect::new(right - 8, r.y, 3, 1);
        v.close_at = Rect::new(right - 4, r.y, 3, 1);

        let content = Rect::new(r.x + 2, r.y + 1, r.width.saturating_sub(4), r.height.saturating_sub(2));
        let w = content.width as usize;
        let top = content.y + 2;
        v.visible = content.bottom().saturating_sub(top) as usize;
        v.room = w;
        if let Some(t) = &self.transcript {
            let head = match (t.full, t.calls) {
                (true, _) => format!("Past {} MiB, the rest not read", crate::transcript::CAP >> 20),
                (false, 1) => "1 tool call".to_string(),
                (false, n) => format!("{n} tool calls"),
            };
            buf.set_stringn(content.x, content.y, &head, w, ground.fg(theme.muted));
            // only the rows in sight become cells; a transcript runs to many thousands
            let end = sub.and_then(|s| end_of(s, now, theme)).map(|l| l.pieces(theme));
            v.len = t.rows.len() + usize::from(end.is_some());
            v.widest = t.widest.max(end.as_ref().map_or(0, |p| p.iter().map(|(t, _)| t.chars().count()).sum()));
            // scrolled up, the end line arriving below does not move what is read
            let held = self.scroll + usize::from(self.scroll > 0 && end.is_some());
            let start = v.len.saturating_sub(v.visible + held.min(v.len.saturating_sub(v.visible)));
            let tone = |t: crate::transcript::Tone| {
                use crate::transcript::Tone::*;
                match t {
                    Text => ground.fg(theme.text),
                    Bold => ground.fg(theme.text).add_modifier(Modifier::BOLD),
                    Muted => ground.fg(theme.muted),
                    Ok => ground.fg(theme.ok),
                    Bad => ground.fg(theme.bad),
                }
            };
            let rows: Vec<Vec<(String, Style)>> = (start..v.len.min(start + v.visible))
                .map(|i| t.rows.get(i).map_or_else(|| end.clone().unwrap_or_default(), |r| r.iter().map(|(s, k)| (s.clone(), tone(*k))).collect()))
                .collect();
            self.paint(v, buf, content, top, &rows);
            return self.paint_selection(v, buf, content, theme);
        }
        let (head, lines) = match sub {
            None => ("This run is no longer recorded.".to_string(), Vec::new()),
            Some(s) => {
                let head = match (cut, s.calls.len()) {
                    (true, _) => "Many tool calls, the oldest not kept".to_string(),
                    (false, 1) => "1 tool call".to_string(),
                    (false, n) => format!("{n} tool calls"),
                };
                let mut lines = Vec::new();
                for c in &s.calls {
                    let (mark, tone, fallback) = match c.outcome {
                        Outcome::Running => ("\u{25cf}", theme.teal, "Running\u{2026}"),
                        Outcome::Ok => ("\u{2713}", theme.ok, "Done"),
                        Outcome::Failed => ("\u{2715}", theme.bad, "Failed"),
                        Outcome::Dropped => ("\u{b7}", theme.faint, "No result"),
                    };
                    lines.push(Line::Call(mark, tone, &c.tool, &c.target));
                    let said = if c.outcome == Outcome::Running || c.summary.is_empty() { fallback } else { &c.summary };
                    lines.push(Line::Result(said.to_string()));
                }
                lines.extend(end_of(s, now, theme));
                (head, lines)
            }
        };
        buf.set_stringn(content.x, content.y, &head, w, ground.fg(theme.muted));

        // the list starts under the headline's blank row and runs to the frame
        let rows: Vec<Vec<(String, Style)>> = lines.iter().map(|l| l.pieces(theme)).collect();
        v.len = rows.len();
        v.widest = rows.iter().map(|p| p.iter().map(|(t, _)| t.chars().count()).sum::<usize>()).max().unwrap_or(0);
        let scroll = self.scroll.min(v.len.saturating_sub(v.visible));
        let start = v.len.saturating_sub(v.visible + scroll);
        let shown: Vec<_> = rows.into_iter().skip(start).take(v.visible).collect();
        self.paint(v, buf, content, top, &shown);
        self.paint_selection(v, buf, content, theme)
    }

    /// The rows in sight from `top`; a call and its result move sideways together, as one block of text.
    fn paint(&self, v: &RunView, buf: &mut Buffer, content: Rect, top: u16, rows: &[Vec<(String, Style)>]) {
        let hscroll = self.hscroll.min(v.widest.saturating_sub(v.room));
        for (i, pieces) in rows.iter().enumerate() {
            let y = top + i as u16;
            let (mut x, mut skip) = (content.x, hscroll);
            for (text, style) in pieces {
                let shown: String = text.chars().skip(skip).collect();
                skip = skip.saturating_sub(text.chars().count());
                if x < content.right() && !shown.is_empty() {
                    x = buf.set_stringn(x, y, &shown, (content.right() - x) as usize, *style).0;
                }
            }
        }
    }

    /// Paints the selection over what was just drawn, and takes its text while the cells are there to read.
    fn paint_selection(&self, v: &mut RunView, buf: &mut Buffer, content: Rect, theme: &Theme) -> Option<String> {
        let span = self.sel?;
        let take = std::mem::take(&mut v.copy);
        let mut text = String::new();
        for y in content.y..content.bottom() {
            let mut line = String::new();
            let mut any = false;
            for x in content.x..content.right() {
                if !crate::select::in_span(span, y, x) {
                    continue;
                }
                any = true;
                if let Some(cell) = buf.cell_mut((x, y)) {
                    line.push_str(cell.symbol());
                    cell.set_bg(theme.fill).set_fg(theme.on_fill);
                }
            }
            if take && any {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(line.trim_end());
            }
        }
        (take && !text.is_empty()).then_some(text)
    }

    /// A press on a button or the seam, a drag, the wheel.
    pub fn mouse(&mut self, v: &mut RunView, ev: MouseEvent) -> Asked {
        let (x, y) = (ev.column, ev.row);
        let sideways = ev.modifiers.contains(KeyModifiers::SHIFT);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.clear_selection();
                if v.close_at.contains((x, y).into()) {
                    return Asked::Close;
                }
                if v.zoom_at.contains((x, y).into()) {
                    return Asked::Zoom;
                }
                if v.on_seam(x, y) {
                    self.resizing = true;
                } else if v.contains(x, y) {
                    self.sel = Some(((y, x), (y, x)));
                    self.selecting = true;
                }
                Asked::Nothing
            }
            MouseEventKind::Drag(_) if self.resizing => {
                let p = v.parent;
                let w = p.right().saturating_sub(x).clamp(MIN_W, p.width.saturating_sub(PANE_MIN_W).max(MIN_W));
                Asked::Width(w)
            }
            MouseEventKind::Drag(_) if self.selecting => {
                let inside = Rect::new(v.area.x + 2, v.area.y + 1, v.area.width.saturating_sub(4), v.area.height.saturating_sub(2));
                let at = (y.clamp(inside.y, inside.bottom().saturating_sub(1)), x.clamp(inside.x, inside.right().saturating_sub(1)));
                if let Some(s) = self.sel.as_mut() {
                    s.1 = at;
                }
                Asked::Nothing
            }
            MouseEventKind::Up(_) => {
                self.resizing = false;
                if std::mem::take(&mut self.selecting) {
                    if self.sel.is_some_and(|s| s.0 != s.1) {
                        v.copy = true;
                    } else {
                        self.sel = None;
                    }
                }
                Asked::Nothing
            }
            MouseEventKind::ScrollLeft => self.sideways(v, -1),
            MouseEventKind::ScrollRight => self.sideways(v, 1),
            MouseEventKind::ScrollUp if sideways => self.sideways(v, -1),
            MouseEventKind::ScrollDown if sideways => self.sideways(v, 1),
            MouseEventKind::ScrollUp => {
                self.scroll = (self.scroll + 1).min(v.len.saturating_sub(v.visible));
                Asked::Nothing
            }
            MouseEventKind::ScrollDown => {
                // A list that got shorter since leaves the scroll past its end; the frame shows it clamped.
                self.scroll = self.scroll.min(v.len.saturating_sub(v.visible)).saturating_sub(1);
                Asked::Nothing
            }
            _ => Asked::Nothing,
        }
    }

    /// Columns per step of the horizontal gesture, clamped to the longest line.
    fn sideways(&mut self, v: &RunView, dir: isize) -> Asked {
        const STEP: usize = 4;
        let most = v.widest.saturating_sub(v.room);
        self.hscroll = if dir < 0 { self.hscroll.min(most).saturating_sub(STEP) } else { (self.hscroll + STEP).min(most) };
        Asked::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::Call;

    fn call(tool: &str, target: &str, outcome: Outcome, summary: &str) -> Call {
        Call { id: String::new(), tool: tool.into(), target: target.into(), at: 100, outcome, summary: summary.into() }
    }

    fn text(buf: &Buffer) -> Vec<String> {
        let a = buf.area;
        (a.y..a.bottom()).map(|y| (a.x..a.right()).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
    }

    fn sub() -> Sub {
        Sub {
            id: "a1".into(),
            kind: "Explore".into(),
            task: "find the callers".into(),
            started: 100,
            last: 160,
            stopped: Some(160),
            calls: vec![
                call("Read", "src/client.rs", Outcome::Ok, "fn main() {"),
                call("Edit", "foo.rs", Outcome::Failed, "String to replace not found"),
                call("Bash", "cargo test", Outcome::Ok, ""),
            ],
            alone: false,
        }
    }

    fn ev(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    const PARENT: Rect = Rect::new(0, 2, 80, 22);
    const SECTION: Rect = Rect::new(40, 2, 40, 22);

    /// A run with the view the app keeps beside it, and what its frames copied.
    struct R {
        run: Run,
        v: RunView,
        copied: Option<String>,
    }

    impl R {
        fn new(pane: PaneId, id: String) -> Self {
            Self { run: Run::new(pane, id), v: RunView::default(), copied: None }
        }
        fn mouse(&mut self, ev: MouseEvent) -> Asked {
            self.run.mouse(&mut self.v, ev)
        }
        fn claims(&self, ev: &MouseEvent) -> bool {
            self.run.claims(&self.v, ev)
        }
        fn contains(&self, x: u16, y: u16) -> bool {
            self.v.contains(x, y)
        }
        fn hide(&mut self) {
            self.v.hide()
        }
        fn take_copy(&mut self) -> Option<String> {
            self.copied.take()
        }
        #[allow(clippy::too_many_arguments)]
        fn draw(&mut self, buf: &mut Buffer, r: Rect, parent: Option<Rect>, framed: bool, border: Color, focused: bool, pane: &str, theme: &Theme, sub: Option<&Sub>, cut: bool, now: u64) {
            if let Some(t) = self.run.draw(&mut self.v, buf, r, parent, framed, border, focused, pane, theme, sub, cut, now) {
                self.copied = Some(t);
            }
        }
    }

    fn drawn(run: &mut R, sub: Option<&Sub>) -> (Buffer, Vec<String>) {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let t = Theme::dark();
        run.draw(&mut buf, SECTION, Some(PARENT), true, t.border_focus, true, "claude", &t, sub, false, 200);
        let lines = text(&buf);
        (buf, lines)
    }

    #[test]
    fn each_call_reads_as_two_lines_and_the_run_says_how_it_ended() {
        let sub = sub();
        let mut run = R::new(1, "a1".into());
        let (buf, lines) = drawn(&mut run, Some(&sub));
        let t = Theme::dark();
        let find = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("{s:?} not in {lines:#?}"));
        assert!(lines[2][40..].starts_with("┬─ claude · Explore") && lines[2].contains(" · 1m "), "the task gives way, the time stays: {:?}", lines[2]);
        assert!(lines[2].ends_with(" ⊡ · × ─ "), "the pane's own corner is the pane's to draw: {:?}", lines[2]);
        assert!(lines[23][40..].starts_with('┴'));
        assert_eq!(buf[(40, 10)].fg, t.border_focus, "the seam in the pane's border colour");
        let read = find("✓ Read(src/client.rs)");
        assert_eq!(read + 1, find("⎿ fn main() {"), "the result sits under its call");
        let edit = find("✕ Edit(foo.rs)");
        assert_eq!(edit + 1, find("⎿ String to replace not fou"));
        assert_eq!(find("✓ Bash(cargo test)") + 1, find("⎿ Done"), "no summary: the outcome in words");
        assert!(read < edit && edit < find("done"));
        assert!(buf[(44, read as u16)].modifier.contains(Modifier::BOLD), "the tool name is bold");
        assert!(lines.iter().all(|l| l[..40].trim().is_empty()), "nothing left of the section");

        // zoomed: no pane frame under it, a whole box of its own
        let live = Sub { stopped: None, calls: vec![call("Bash", "cargo test", Outcome::Running, "")], ..sub };
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        run.draw(&mut buf, SECTION, None, false, t.border, false, "claude", &t, Some(&live), true, 110);
        let lines = text(&buf);
        assert!(lines.iter().any(|l| l.contains("● Bash(cargo test)")) && lines.iter().any(|l| l.contains("⎿ Running…")), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("done")));
        assert!(lines.iter().any(|l| l.contains("the oldest not kept")));
        assert!(lines[2][40..].starts_with('┌') && lines[2].ends_with('┐') && lines[23].ends_with('┘'));
        assert!(!run.claims(&ev(MouseEventKind::Moved, 39, 10)), "no seam to drag while zoomed");

        // silent since 60 with nothing in flight, after its agent ended the turn: its stop was lost
        let lost = Sub { alone: true, started: 20, last: 50, calls: vec![call("Bash", "cargo test", Outcome::Ok, "")], ..live };
        let (_, lines) = drawn(&mut run, Some(&lost));
        assert!(lines.iter().any(|l| l.contains("no report")) && !lines.iter().any(|l| l.contains("Running")), "{lines:#?}");
        assert!(lines[2].contains(" · 30s "), "its time stops at its last word: {:?}", lines[2]);
    }

    #[test]
    fn golden_run_from_the_hook_log_as_master_drew_it() {
        let (buf, _) = drawn(&mut R::new(1, "a1".into()), Some(&sub()));
        crate::hook::tests::golden("ui/run-hook-log.txt", &crate::app::snapshots::dump(&buf, buf.area));
    }

    fn read(rows: Vec<crate::transcript::Row>, calls: usize, from: u64) -> crate::transcript::Found {
        crate::transcript::Found { pane: 1, id: "a1".into(), from, to: from + 1, rows, calls, full: false }
    }

    fn fixture() -> crate::transcript::Found {
        let (rows, calls) = crate::transcript::parse(include_bytes!("../tests/fixtures/transcript/agent.jsonl"), true);
        read(rows, calls, 0)
    }

    #[test]
    fn golden_run_from_its_transcript() {
        let mut run = R::new(1, "a1".into());
        assert!(run.run.take(fixture()));
        let (buf, lines) = drawn(&mut run, Some(&sub()));
        assert!(lines.iter().any(|l| l.contains("5 tool calls")) && !lines.iter().any(|l| l.contains("Read(src/client.rs)")), "{lines:#?}");
        assert!(lines[22].contains("done"), "the run's end under its transcript");
        crate::hook::tests::golden("ui/run-transcript.txt", &crate::app::snapshots::dump(&buf, buf.area));
    }

    #[test]
    fn a_transcript_draws_only_the_rows_in_sight_and_holds_its_place_when_scrolled_up() {
        use crate::transcript::Tone;
        let many: Vec<_> = (0..1_000_000).map(|n| vec![(format!("row {n}"), Tone::Text)]).collect();
        let mut run = R::new(1, "a1".into());
        assert!(run.run.take(read(many, 0, 0)));
        let t = std::time::Instant::now();
        for _ in 0..20 {
            drawn(&mut run, None);
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(300), "a frame costs its rows in sight: {:?}", t.elapsed());
        let (_, lines) = drawn(&mut run, None);
        assert!(lines[22].contains("row 999999") && run.v.len == 1_000_000, "the newest at the bottom");
        for _ in 0..3 {
            run.mouse(ev(MouseEventKind::ScrollUp, 60, 10));
        }
        let (_, up) = drawn(&mut run, None);
        assert!(up[22].contains("row 999996"));
        let offset = run.run.transcript.as_ref().unwrap().offset;
        assert!(run.run.take(read(vec![vec![("new".into(), Tone::Text)]; 2], 0, offset)));
        assert_eq!(drawn(&mut run, None).1, up, "scrolled up, new rows do not move what is read");
        assert_eq!(drawn(&mut run, Some(&sub())).1[4..], up[4..], "nor does the run's end line");
        run.run.transcript.as_mut().unwrap().full = true;
        assert!(drawn(&mut run, None).1[3].contains("Past 32 MiB, the rest not read"));
    }

    #[test]
    fn the_buttons_close_and_zoom_and_the_wheel_scrolls() {
        let mut run = R::new(1, "a1".into());
        let (buf, lines) = drawn(&mut run, None);
        assert!(lines.iter().any(|l| l.contains("no longer recorded")));
        let down = |x, y| ev(MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(run.contains(60, 12) && !run.contains(10, 12));
        assert_eq!(run.mouse(down(60, 12)), Asked::Nothing, "inside stays open");
        let (close, zoom) = (run.v.close_at, run.v.zoom_at);
        assert_eq!((buf[(close.x + 1, close.y)].symbol(), buf[(zoom.x + 1, zoom.y)].symbol()), ("×", "⊡"));
        use crate::pointer::*;
        assert_eq!((run.v.pointer(close.x + 1, close.y), run.v.pointer(zoom.x, zoom.y), run.v.pointer(60, 12)), (Some(POINTER), Some(POINTER), Some(TEXT)));
        assert_eq!((run.v.pointer(40, 12), run.v.pointer(10, 12)), (Some(EW), None), "the seam drags; outside is not the section's");
        assert_eq!(run.mouse(down(close.x + 1, close.y)), Asked::Close);
        assert_eq!(run.mouse(down(zoom.x + 1, zoom.y)), Asked::Zoom);
        run.hide();
        assert!(!run.claims(&ev(MouseEventKind::Moved, 60, 12)), "a pane not on screen has no section to click");
    }

    #[test]
    fn the_seam_drags_the_width_within_its_bounds() {
        let mut run = R::new(1, "a1".into());
        drawn(&mut run, Some(&sub()));
        let (down, drag, up) = (MouseEventKind::Down(MouseButton::Left), MouseEventKind::Drag(MouseButton::Left), MouseEventKind::Up(MouseButton::Left));
        assert!(run.claims(&ev(MouseEventKind::Moved, 39, 10)) && run.claims(&ev(MouseEventKind::Moved, 41, 10)), "a cell of slack either side, as a pane seam has");
        assert_eq!(run.mouse(ev(down, 39, 10)), Asked::Nothing);
        assert_eq!(run.mouse(ev(drag, 30, 10)), Asked::Width(50));
        assert!(run.claims(&ev(drag, 5, 10)), "the drag keeps the events wherever the pointer goes");
        assert_eq!(run.mouse(ev(drag, 2, 10)), Asked::Width(60), "the pane keeps 20 columns");
        assert_eq!(run.mouse(ev(drag, 79, 10)), Asked::Width(MIN_W), "the section keeps 12");
        run.mouse(ev(up, 79, 10));
        assert!(!run.claims(&ev(drag, 5, 10)));
        assert!(run.take_copy().is_none(), "a resize is not a selection");
    }

    #[test]
    fn a_sideways_gesture_moves_the_text_and_stops_at_the_longest_line() {
        let long = Sub { calls: vec![call("Bash", &"x".repeat(60), Outcome::Ok, "a result")], ..sub() };
        let mut run = R::new(1, "a1".into());
        let (_, lines) = drawn(&mut run, Some(&long));
        let row = lines.iter().position(|l| l.contains("✓ Bash(xxx")).unwrap();
        run.mouse(ev(MouseEventKind::ScrollRight, 60, 10));
        let (_, moved) = drawn(&mut run, Some(&long));
        let at = |l: &str| l.chars().skip(42).collect::<String>();
        assert!(at(&moved[row]).starts_with("sh(xxx"), "four columns further: {:?}", moved[row]);
        assert!(at(&moved[row + 1]).starts_with("a result"), "the result moves with its call: {:?}", moved[row + 1]);
        for _ in 0..40 {
            run.mouse(MouseEvent { modifiers: KeyModifiers::SHIFT, ..ev(MouseEventKind::ScrollDown, 60, 10) });
        }
        let (_, end) = drawn(&mut run, Some(&long));
        assert!(end[row].trim_end().ends_with("xx)"), "the longest line's end, and no further: {:?}", end[row]);
        run.mouse(ev(MouseEventKind::ScrollUp, 60, 10));
        let (_, same) = drawn(&mut run, Some(&long));
        assert_eq!(same[row], end[row], "a plain wheel is vertical");
        for _ in 0..40 {
            run.mouse(ev(MouseEventKind::ScrollLeft, 60, 10));
        }
        assert_eq!(drawn(&mut run, Some(&long)).1[row], lines[row], "back to the start");
    }

    #[test]
    fn a_drag_over_two_lines_copies_their_text() {
        let mut run = R::new(1, "a1".into());
        let (_, lines) = drawn(&mut run, Some(&sub()));
        let read = lines.iter().position(|l| l.contains("✓ Read")).unwrap() as u16;
        run.mouse(ev(MouseEventKind::Down(MouseButton::Left), 42, read));
        run.mouse(ev(MouseEventKind::Drag(MouseButton::Left), 77, read + 1));
        run.mouse(ev(MouseEventKind::Up(MouseButton::Left), 77, read + 1));
        let (buf, _) = drawn(&mut run, Some(&sub()));
        assert_eq!(run.take_copy().as_deref(), Some("✓ Read(src/client.rs)\n  ⎿ fn main() {"));
        assert_eq!(buf[(44, read)].bg, Theme::dark().accent, "the selection wears the pane's highlight");
        assert!(run.take_copy().is_none(), "copied once");
    }
}
