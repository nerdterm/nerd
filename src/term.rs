//! State of a single terminal on top of alacritty_terminal, which reflows on resize: the title set via OSC 0/2,
//! and the mouse modes that decide who gets a click.

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{MIN_COLUMNS, TermMode};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{self, NamedColor};

/// `Done` is `Idle` for a pane nobody has looked at since it finished; the mark
/// clears itself once a human has looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Blocked,
    Done,
    Working,
    Idle,
    /// The first seconds after launch: the program hasn't shown anything yet.
    Unknown,
    /// The turn ended on an error (the hook's `StopFailure`), until the next prompt.
    Failed,
}

impl AgentState {
    pub fn mark(self) -> char {
        match self {
            Self::Blocked => '●',
            Self::Done => '✓',
            Self::Working => '●',
            Self::Idle => '○',
            Self::Unknown => '·',
            Self::Failed => '✕',
        }
    }

    /// Order for collapsing a group: the loudest state wins.
    pub fn priority(self) -> u8 {
        match self {
            Self::Failed => 5,
            Self::Blocked => 4,
            Self::Done => 3,
            Self::Working => 2,
            Self::Idle => 1,
            Self::Unknown => 0,
        }
    }

}

/// Where a wheel event goes; the rule from xterm and tmux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRouting {
    ToChild,
    /// Alternate screen without tracking: arrow keys, same as `less`/`man`.
    Arrows { app_cursor: bool },
    Scrollback,
}

/// Keeps the title a program sets; nerd answers queries itself, so the emulator's replies are dropped.
#[derive(Clone, Default)]
struct Listener(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl alacritty_terminal::event::EventListener for Listener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        if let alacritty_terminal::event::Event::Title(t) = event
            && let Ok(mut slot) = self.0.lock()
        {
            *slot = Some(t);
        }
    }
}

/// The last `CSI ? … h|l` in `bytes` that names `mode`: Some(true) set, Some(false) reset.
fn private_mode(bytes: &[u8], mode: &[u8]) -> Option<bool> {
    let mut found = None;
    let mut i = 0;
    while let Some(at) = bytes[i..].windows(3).position(|w| w == b"\x1b[?") {
        let start = i + at + 3;
        let len = bytes[start..].iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
        let end = start + len;
        if let Some(&f @ (b'h' | b'l')) = bytes.get(end)
            && bytes[start..end].split(|b| *b == b';').any(|p| p == mode)
        {
            found = Some(f == b'h');
        }
        i = end.max(start);
    }
    found
}

/// Whether a ground counts as dark: its luma under the middle.
pub fn dark((r, g, b): (u8, u8, u8)) -> bool {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 < 128
}

/// The report a program that set mode 2031 gets: `CSI ? 997 ; 1 n` dark, `2` light.
pub fn theme_report(dark: bool) -> Vec<u8> {
    format!("\x1b[?997;{}n", if dark { 1 } else { 2 }).into_bytes()
}

struct Size(u16, u16);

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1 as usize
    }
    fn screen_lines(&self) -> usize {
        self.1 as usize
    }
    fn columns(&self) -> usize {
        self.0 as usize
    }
}

pub struct Term {
    term: alacritty_terminal::Term<Listener>,
    parser: ansi::Processor,
    events: Listener,
    title: Option<String>,
    last_output: std::time::Instant,
    started: std::time::Instant,
    /// How many lines are scrolled up from the bottom.
    pub scroll: usize,
    /// Whether a human has looked at the pane since work last finished.
    seen: bool,
    /// The previous base state, to catch the "working -> finished" transition.
    last_base: AgentState,
    /// The word from the agent's hook. If present, we trust it and don't parse the screen.
    hook: Option<AgentState>,
    blocked_text: Vec<String>,
    /// The pointer shape the program asked for with OSC 22, shown while the mouse is over its pane.
    pub pointer: Option<String>,
    /// Last change of the base state, for "most recent first"; None until the first one after launch.
    changed_at: Option<std::time::Instant>,
    /// Bumped by output and resize, which move text between rows; a search reruns only after it moves.
    edits: u64,
    /// The program asked (mode 2031) to be told when the theme turns dark or light.
    pub theme_notify: bool,
}

// How many times this thread built a screen's text for the state, SGR sequences and the whole text, so a test can count them.
#[cfg(test)]
thread_local! {
    pub static CONTENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub static SGR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub static LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How long a silence makes a pane count as idle.
const IDLE_AFTER: std::time::Duration = std::time::Duration::from_millis(600);
/// How long after launch we don't try to determine state.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

impl Term {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let events = Listener::default();
        let config = alacritty_terminal::term::Config { scrolling_history: scrollback, ..Default::default() };
        Self {
            term: alacritty_terminal::Term::new(config, &Size(cols.max(MIN_COLUMNS as u16), rows.max(1)), events.clone()),
            parser: ansi::Processor::new(),
            events,
            title: None,
            // Nothing printed yet: a fresh pane is not "working", or its first tick would pass for activity.
            last_output: std::time::Instant::now().checked_sub(IDLE_AFTER).unwrap_or_else(std::time::Instant::now),
            started: std::time::Instant::now(),
            scroll: 0,
            seen: true,
            last_base: AgentState::Unknown,
            hook: None,
            blocked_text: Vec::new(),
            changed_at: None,
            pointer: None,
            edits: 0,
            theme_notify: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if let Some(p) = crate::pointer::requested(bytes) {
            self.pointer = p;
        }
        if let Some(on) = private_mode(bytes, b"2031") {
            self.theme_notify = on;
        }
        self.advance(bytes);
        self.last_output = std::time::Instant::now();
    }

    /// Old text put back after a restart: drawn, but not output, so it is neither activity nor "done".
    pub fn replay(&mut self, bytes: &[u8]) {
        self.advance(bytes);
    }

    fn advance(&mut self, bytes: &[u8]) {
        self.edits += 1;
        self.parser.advance(&mut self.term, bytes);
        if let Ok(mut slot) = self.events.0.lock()
            && let Some(t) = slot.take()
        {
            self.title = Some(t);
        }
        // New output keeps a scrolled view where it was, which moves the offset.
        self.scroll = self.term.grid().display_offset();
    }

    /// Rewraps the text to the new width, the way a terminal window does.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(Size(cols.max(MIN_COLUMNS as u16), rows.max(1)));
        self.scroll = self.term.grid().display_offset();
        self.edits += 1;
    }

    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// Lines scrolled back, lines still above the view, and whether the alternate screen is up.
    pub fn history(&mut self) -> (usize, usize, bool) {
        let g = self.term.grid();
        let offset = g.display_offset();
        (offset, g.history_size() - offset, self.term.mode().contains(TermMode::ALT_SCREEN))
    }

    pub fn screen(&self) -> Screen<'_> {
        Screen(&self.term)
    }

    /// Every row, history first, as drawn: a wrapped line is several rows.
    pub fn all_lines(&mut self) -> Vec<String> {
        #[cfg(test)]
        LINES.with(|c| c.set(c.get() + 1));
        let g = self.term.grid();
        (-(g.history_size() as i32)..g.screen_lines() as i32)
            .map(|l| {
                let mut s = row_text(&self.term, Line(l)).0;
                s.truncate(s.trim_end().len());
                s
            })
            .collect()
    }

    /// Lines as written: wrapped rows joined, so a narrower or wider terminal wraps them again.
    fn logical_lines(&self, styled: bool) -> Vec<Vec<u8>> {
        let g = self.term.grid();
        let mut out = Vec::new();
        let mut line = Vec::new();
        for l in -(g.history_size() as i32)..g.screen_lines() as i32 {
            let (text, wrapped) = if styled { row_styled(&self.term, Line(l)) } else { row_text(&self.term, Line(l)) };
            line.extend_from_slice(if wrapped { text.as_bytes() } else { text.trim_end().as_bytes() });
            if !wrapped {
                out.push(std::mem::take(&mut line));
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
        out
    }

    /// Every line with its colors, then the cursor and input modes, as escape sequences any build can replay.
    /// On the alternate screen, only that screen crosses over.
    pub fn dump(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        if alt {
            out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
            for l in 0..self.term.grid().screen_lines() {
                out.extend(format!("\x1b[{};1H", l + 1).into_bytes());
                out.extend(row_styled(&self.term, Line(l as i32)).0.into_bytes());
            }
        } else {
            let lines = self.logical_lines(true);
            let n = lines.len();
            for (i, l) in lines.into_iter().enumerate() {
                out.extend(l);
                if i + 1 < n {
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
        let c = self.term.grid().cursor.point;
        out.extend(format!("\x1b[m\x1b[{};{}H", c.line.0 + 1, c.column.0 + 1).into_bytes());
        let pen = sgr(&self.term.grid().cursor.template);
        if pen != "\x1b[0m" {
            out.extend(pen.into_bytes());
        }
        let mode = *self.term.mode();
        for (m, seq) in [
            (TermMode::APP_CURSOR, "\x1b[?1h"),
            (TermMode::APP_KEYPAD, "\x1b="),
            (TermMode::BRACKETED_PASTE, "\x1b[?2004h"),
            (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h"),
            (TermMode::MOUSE_DRAG, "\x1b[?1002h"),
            (TermMode::MOUSE_MOTION, "\x1b[?1003h"),
            (TermMode::UTF8_MOUSE, "\x1b[?1005h"),
            (TermMode::SGR_MOUSE, "\x1b[?1006h"),
        ] {
            if mode.contains(m) {
                out.extend_from_slice(seq.as_bytes());
            }
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }

    /// Plain lines only (no modes, cursor or colors), oldest history dropped until
    /// it fits `cap`; the process is gone, so the visible screen is history too.
    pub fn text(&mut self, cap: usize) -> Vec<u8> {
        let mut lines: Vec<String> = self.logical_lines(false).into_iter().map(|l| String::from_utf8_lossy(&l).into_owned()).collect();
        while lines.last().is_some_and(|l| l.trim_end().is_empty()) {
            lines.pop();
        }
        // Count from the end so the last lines always survive the cap.
        let mut start = lines.len();
        let mut total = 0;
        while start > 0 {
            let n = lines[start - 1].trim_end().len() + 2;
            if total + n > cap && start < lines.len() {
                break;
            }
            total += n;
            start -= 1;
        }
        let mut out = Vec::with_capacity(total);
        for l in &lines[start..] {
            out.extend_from_slice(l.trim_end().as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    /// The counterpart of `dump`: a fresh terminal takes the old one's place.
    pub fn restore(&mut self, dump: &[u8], scroll: usize) {
        self.replay(dump);
        self.term.scroll_display(Scroll::Delta(scroll.min(i32::MAX as usize) as i32));
        self.scroll = self.term.grid().display_offset();
    }

    /// The title the process set on itself.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The name without a leading state marker icon.
    pub fn clean_title(&self) -> Option<&str> {
        // Strips exactly one leading character, or "(draft) x" and "~/project" would lose theirs.
        self.title().map(|t| {
            let t = t.trim_start();
            match t.chars().next() {
                Some(c) if !c.is_alphanumeric() && !matches!(c, '[' | '/' | '~' | '(' | '.' | '_') => t[c.len_utf8()..].trim(),
                _ => t,
            }
        })
    }

    /// Screen text that means the pane's agent asks the human; none for a pane without an agent.
    pub fn set_blocked_text(&mut self, text: Vec<String>) {
        self.blocked_text = text;
    }

    pub fn set_hook_state(&mut self, s: Option<AgentState>) {
        self.hook = s;
    }

    /// Base state disregarding "seen": what's visible on screen and by timing.
    fn base_state(&self) -> AgentState {
        if let Some(s) = self.hook {
            return s;
        }
        // Decided from the visible prompt text, cheaper than tracking the protocol; built only when a rule reads it.
        let visible = || {
            #[cfg(test)]
            CONTENTS.with(|c| c.set(c.get() + 1));
            self.screen().contents()
        };
        if !self.blocked_text.is_empty() {
            let visible = visible();
            if self.blocked_text.iter().any(|t| visible.contains(t.as_str())) {
                return AgentState::Blocked;
            }
        }
        if self.last_output.elapsed() < IDLE_AFTER {
            return AgentState::Working;
        }
        if self.started.elapsed() < STARTUP_GRACE && visible().trim().is_empty() {
            return AgentState::Unknown;
        }
        AgentState::Idle
    }

    /// State to display: `Idle` that hasn't been looked at yet is `Done`.
    pub fn state(&self) -> AgentState {
        match self.base_state() {
            AgentState::Idle if !self.seen => AgentState::Done,
            other => other,
        }
    }

    /// `state` as of the last `tick_state`, without reading the screen again.
    pub fn shown(&self) -> AgentState {
        match self.last_base {
            AgentState::Idle if !self.seen => AgentState::Done,
            other => other,
        }
    }

    /// Catches the moment work finished and clears the "seen" mark.
    /// Returns whether the state changed: idle comes from the clock, so the caller cannot tell by comparing.
    pub fn tick_state(&mut self) -> bool {
        let base = self.base_state();
        if self.last_base == AgentState::Working && base == AgentState::Idle {
            self.seen = false;
        }
        if base != AgentState::Idle {
            self.seen = true;
        }
        // Leaving Unknown is launch, not activity: a restart would otherwise reshuffle every list.
        if base != self.last_base && self.last_base != AgentState::Unknown {
            self.changed_at = Some(std::time::Instant::now());
        }
        let changed = base != self.last_base;
        self.last_base = base;
        changed
    }

    pub fn changed_at(&self) -> Option<std::time::Instant> {
        self.changed_at
    }

    /// What an update in place carries over: time since the last change, and unseen finished work.
    pub fn activity(&self) -> (Option<std::time::Duration>, bool) {
        (self.changed_at.map(|t| t.elapsed()), !self.seen)
    }

    /// The counterpart of `activity`, in the new process.
    pub fn carry(&mut self, since: Option<std::time::Duration>, unseen: bool) {
        self.changed_at = since.and_then(|d| std::time::Instant::now().checked_sub(d));
        self.seen = !unseen;
    }

    /// A human looked at the pane, "done" no longer needs to be highlighted.
    pub fn mark_seen(&mut self) {
        self.seen = true;
    }

    /// Answers DA/CPR queries, and OSC 10/11 with the theme's text and ground: without them programs
    /// wait out timeouts (Claude Code sends `ESC[?6n` every frame) or guess the background dark.
    /// ceiling: a query split across a chunk boundary goes unseen, rare with 64K reads.
    pub fn answers(&self, bytes: &[u8], fg: (u8, u8, u8), bg: (u8, u8, u8)) -> Vec<u8> {
        let (row, col) = self.screen().cursor_position();
        let mut out = Vec::new();
        // xterm's form, ended the way the question was: BEL or ST
        for (code, (r, g, b)) in [(b'0', fg), (b'1', bg)] {
            let q = [0x1b, b']', b'1', code, b';', b'?'];
            let mut from = 0;
            while let Some(at) = bytes[from..].windows(q.len()).position(|w| w == q) {
                let end = from + at + q.len();
                let st = bytes.get(end) == Some(&0x1b);
                out.extend(format!("\x1b]1{};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}", code as char).into_bytes());
                out.extend_from_slice(if st { b"\x1b\\" } else { b"\x07" });
                from = end;
            }
        }
        let mut i = 0;
        while let Some(at) = bytes[i..].windows(2).position(|w| w == b"\x1b[") {
            let rest = &bytes[i + at + 2..];
            let n = if rest.starts_with(b"c") || rest.starts_with(b"0c") {
                out.extend_from_slice(b"\x1b[?1;2c"); // VT100 with color, like Terminal.app
                1
            } else if rest.starts_with(b"6n") {
                out.extend(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
                2
            } else if rest.starts_with(b"?6n") {
                out.extend(format!("\x1b[?{};{}R", row + 1, col + 1).into_bytes());
                3
            } else if rest.starts_with(b"?996n") {
                out.extend(theme_report(dark(bg)));
                5
            } else if rest.starts_with(b"?2031$p") {
                out.extend(format!("\x1b[?2031;{}$y", if self.theme_notify { 1 } else { 2 }).into_bytes());
                7
            } else {
                0
            };
            i += at + 2 + n;
        }
        out
    }

    /// Mouse reporting off, as `reset` would turn it: for a program that left it on.
    pub fn mouse_off(&mut self) {
        self.advance(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l");
    }

    /// Whether the program inside the pane is requesting mouse events.
    pub fn wants_mouse(&self) -> bool {
        self.screen().mouse_protocol_mode() != MouseProtocolMode::None
    }

    pub fn wheel_routing(&self) -> WheelRouting {
        if self.wants_mouse() {
            WheelRouting::ToChild
        } else if self.screen().alternate_screen() {
            WheelRouting::Arrows { app_cursor: self.screen().app_cursor() }
        } else {
            WheelRouting::Scrollback
        }
    }

    /// Scrolls our own scrollback. Returns whether anything changed.
    pub fn scroll_by(&mut self, lines: isize) -> bool {
        let before = self.scroll;
        let offset = self.term.grid().display_offset() as isize;
        let next = (self.scroll as isize - lines).max(0);
        self.term.scroll_display(Scroll::Delta((next - offset) as i32));
        // The emulator clamps to history length; our counter must match or an overshoot
        // makes scrolling back down stay silent until it catches up.
        self.scroll = self.term.grid().display_offset();
        self.scroll != before
    }
}

/// A row's text without wide characters' second halves, and whether it wraps into the next.
fn row_text(term: &alacritty_terminal::Term<Listener>, line: Line) -> (String, bool) {
    let row = &term.grid()[line];
    let mut s = String::with_capacity(row.len());
    for cell in &row[..] {
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        s.push(ch(cell));
        s.extend(cell.zerowidth().into_iter().flatten());
    }
    (s, row.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE)))
}

/// The same with SGR sequences, trailing plain blanks dropped unless the row wraps.
fn row_styled(term: &alacritty_terminal::Term<Listener>, line: Line) -> (String, bool) {
    let row = &term.grid()[line];
    let wrapped = row.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
    let plain = |c: &alacritty_terminal::term::cell::Cell| ch(c) == ' ' && c.bg == ansi::Color::Named(NamedColor::Background) && !c.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
    let end = if wrapped { row.len() } else { row[..].iter().rposition(|c| !plain(c)).map_or(0, |i| i + 1) };
    let mut s = String::new();
    let mut pen = String::new();
    let mut last = None;
    for cell in &row[..Column(end)] {
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let style = Some((cell.fg, cell.bg, cell.flags & STYLE));
        if style != last {
            last = style;
            let sgr = sgr(cell);
            if sgr != pen {
                s.push_str(&sgr);
                pen = sgr;
            }
        }
        s.push(ch(cell));
        s.extend(cell.zerowidth().into_iter().flatten());
    }
    if !pen.is_empty() && pen != "\x1b[0m" {
        s.push_str("\x1b[0m");
    }
    (s, wrapped)
}

/// The flags `sgr` writes.
const STYLE: Flags = Flags::BOLD.union(Flags::DIM).union(Flags::ITALIC).union(Flags::ALL_UNDERLINES).union(Flags::INVERSE).union(Flags::HIDDEN).union(Flags::STRIKEOUT);

/// A tab leaves '\t' in its cell; it reads as the blank it is.
fn ch(cell: &alacritty_terminal::term::cell::Cell) -> char {
    if cell.c == '\t' { ' ' } else { cell.c }
}

fn sgr(cell: &alacritty_terminal::term::cell::Cell) -> String {
    #[cfg(test)]
    SGR.with(|c| c.set(c.get() + 1));
    let mut p = vec!["0".to_string()];
    for (f, n) in [(Flags::BOLD, "1"), (Flags::DIM, "2"), (Flags::ITALIC, "3"), (Flags::ALL_UNDERLINES, "4"), (Flags::INVERSE, "7"), (Flags::HIDDEN, "8"), (Flags::STRIKEOUT, "9")] {
            if cell.flags.intersects(f) {
                p.push(n.into());
            }
    }
    for (color, base) in [(cell.fg, 30), (cell.bg, 40)] {
        match Color::from(color) {
            Color::Default => {}
            Color::Idx(i) if i < 8 => p.push((base + i as u16).to_string()),
            Color::Idx(i) if i < 16 => p.push((base + 60 + i as u16 - 8).to_string()),
            Color::Idx(i) => p.push(format!("{};5;{i}", base + 8)),
            Color::Rgb(r, g, b) => p.push(format!("{};2;{r};{g};{b}", base + 8)),
        }
    }
    format!("\x1b[{}m", p.join(";"))
}

/// A color as nerd's theme sees it: the terminal's default, one of 256, or exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl From<ansi::Color> for Color {
    fn from(c: ansi::Color) -> Self {
        match c {
            ansi::Color::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
            ansi::Color::Indexed(i) => Color::Idx(i),
            ansi::Color::Named(n) => match n as usize {
                i @ 0..=15 => Color::Idx(i as u8),
                _ => match n {
                    NamedColor::DimBlack => Color::Idx(0),
                    NamedColor::DimRed => Color::Idx(1),
                    NamedColor::DimGreen => Color::Idx(2),
                    NamedColor::DimYellow => Color::Idx(3),
                    NamedColor::DimBlue => Color::Idx(4),
                    NamedColor::DimMagenta => Color::Idx(5),
                    NamedColor::DimCyan => Color::Idx(6),
                    NamedColor::DimWhite => Color::Idx(7),
                    _ => Color::Default,
                },
            },
        }
    }
}

/// Which mouse events the program asked for: 1000, 1002 or 1003.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseProtocolMode {
    None,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// How mouse events are written to the program: X10 bytes, UTF-8 (1005) or SGR (1006).
// X10 and UTF-8 are encoded but never chosen: see mouse::CHILD_ENCODING.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    Default,
    Utf8,
    Sgr,
}

/// The visible rows, following the scrolled view.
#[derive(Clone, Copy)]
pub struct Screen<'a>(&'a alacritty_terminal::Term<Listener>);

impl Screen<'_> {
    /// Rows and columns.
    pub fn size(&self) -> (u16, u16) {
        let g = self.0.grid();
        (g.screen_lines() as u16, g.columns() as u16)
    }

    /// The cell `row` rows down the view: with the view scrolled back, a history row.
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell> {
        let g = self.0.grid();
        if row as usize >= g.screen_lines() || col as usize >= g.columns() {
            return None;
        }
        let c = &g[Line(row as i32 - g.display_offset() as i32)][Column(col as usize)];
        let mut text = if ch(c) == ' ' { String::new() } else { c.c.to_string() };
        text.extend(c.zerowidth().into_iter().flatten());
        Some(Cell { text, fg: c.fg.into(), bg: c.bg.into(), flags: c.flags })
    }

    /// The visible text: rows joined by newlines, a wrapped row joined to the next, trailing blanks dropped.
    pub fn contents(&self) -> String {
        let g = self.0.grid();
        let top = -(g.display_offset() as i32);
        let mut out = String::new();
        for l in top..top + g.screen_lines() as i32 {
            let (text, wrapped) = row_text(self.0, Line(l));
            if wrapped {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        out.trim_end().to_string()
    }

    /// Row and column of the cursor on the live screen.
    pub fn cursor_position(&self) -> (u16, u16) {
        let p = self.0.grid().cursor.point;
        (p.line.0.max(0) as u16, p.column.0 as u16)
    }

    pub fn hide_cursor(&self) -> bool {
        !self.0.mode().contains(TermMode::SHOW_CURSOR)
    }

    pub fn alternate_screen(&self) -> bool {
        self.0.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn app_cursor(&self) -> bool {
        self.0.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.0.mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        let m = self.0.mode();
        if m.contains(TermMode::MOUSE_MOTION) {
            MouseProtocolMode::AnyMotion
        } else if m.contains(TermMode::MOUSE_DRAG) {
            MouseProtocolMode::ButtonMotion
        } else if m.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseProtocolMode::PressRelease
        } else {
            MouseProtocolMode::None
        }
    }
}

/// One grid cell, copied out: its text (empty when blank), colors and attributes.
#[derive(Clone)]
pub struct Cell {
    text: String,
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Cell {
    pub fn contents(&self) -> &str {
        &self.text
    }
    pub fn has_contents(&self) -> bool {
        !self.text.is_empty()
    }
    pub fn fgcolor(&self) -> Color {
        self.fg
    }
    pub fn bgcolor(&self) -> Color {
        self.bg
    }
    pub fn bold(&self) -> bool {
        self.flags.contains(Flags::BOLD)
    }
    pub fn dim(&self) -> bool {
        self.flags.contains(Flags::DIM)
    }
    pub fn italic(&self) -> bool {
        self.flags.contains(Flags::ITALIC)
    }
    pub fn underline(&self) -> bool {
        self.flags.intersects(Flags::ALL_UNDERLINES)
    }
    pub fn inverse(&self) -> bool {
        self.flags.contains(Flags::INVERSE)
    }
    pub fn is_wide(&self) -> bool {
        self.flags.contains(Flags::WIDE_CHAR)
    }
    /// The second half of a wide character, or the gap left before one that wrapped.
    pub fn is_wide_continuation(&self) -> bool {
        self.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_and_restore_keep_history_screen_and_modes() {
        let mut t = super::Term::new(10, 3, 100);
        t.feed(b"one\r\ntwo\r\nthree\r\nfour\r\n\x1b[31mred\x1b[?1000h");
        let dump = t.dump();
        let mut u = super::Term::new(10, 3, 100);
        u.restore(&dump, 0);
        assert_eq!(u.screen().contents(), t.screen().contents(), "visible screen");
        assert!(u.wants_mouse(), "input modes cross over");
        assert_eq!(u.all_lines(), t.all_lines(), "history crosses over");
        assert_eq!(u.screen().cell(2, 0).map(|c| c.fgcolor()), t.screen().cell(2, 0).map(|c| c.fgcolor()), "colors on screen");

        let mut a = super::Term::new(10, 3, 0);
        a.feed(b"\x1b[?1049hALT");
        let mut b = super::Term::new(10, 3, 0);
        b.restore(&a.dump(), 0);
        assert!(b.screen().alternate_screen(), "alternate screen crosses over");
        assert!(b.screen().contents().contains("ALT"));
    }

    #[test]
    fn the_pen_a_program_set_survives_a_dump() {
        let mut t = super::Term::new(10, 3, 100);
        t.feed(b"\x1b[1;31mred");
        let mut u = super::Term::new(10, 3, 100);
        u.restore(&t.dump(), 0);
        u.feed(b"more");
        let c = u.screen().cell(0, 3).unwrap();
        assert_eq!((c.contents(), c.fgcolor(), c.bold()), ("m", super::Color::Idx(1), true), "text after the update is still red and bold");
    }

    #[test]
    fn long_combining_sequences_read_back_whole() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let accented = "a\u{301}\u{302}\u{303}\u{304}\u{306}\u{307}\u{308}\u{30A}\u{30B}\u{30C}";
        let mut t = super::Term::new(20, 3, 100);
        t.feed(format!("{family}\r\n{accented}").as_bytes());
        let mut u = super::Term::new(20, 3, 100);
        u.restore(&t.dump(), 0);
        for (name, term) in [("original", &t), ("restored", &u)] {
            let s = term.screen();
            assert_eq!(s.contents(), format!("{family}\n{accented}"), "{name} contents");
            let row: String = (0..20).filter_map(|x| s.cell(0, x)).filter(|c| !c.is_wide_continuation()).map(|c| c.contents().to_string()).collect();
            assert_eq!(row, family, "{name} cells of the emoji");
            assert_eq!(s.cell(1, 0).unwrap().contents(), accented, "{name} cell of the accented letter");
        }
    }

    #[test]
    fn a_tab_reads_as_spaces_and_survives_a_dump() {
        let mut t = super::Term::new(40, 4, 100);
        t.feed(b"a\tb\r\n");
        assert_eq!(t.all_lines()[0], "a       b");
        assert!(!t.screen().cell(0, 1).unwrap().has_contents());
        let mut u = super::Term::new(40, 4, 100);
        u.restore(&t.dump(), 0);
        assert_eq!(u.all_lines()[0], "a       b", "b stays at column 8 after an update in place");
    }

    #[test]
    fn a_pane_resized_to_one_column_with_wide_text_returns() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut t = super::Term::new(10, 3, 10);
            t.feed("ab中文\r\n".as_bytes());
            t.resize(1, 3);
            tx.send(t.screen().size()).unwrap();
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)), Ok((3, 2)));
    }

    #[test]
    fn a_dump_writes_colours_only_when_they_change() {
        let mut t = super::Term::new(120, 10, 100);
        for _ in 0..20 {
            t.feed(&[b'x'; 120]);
            t.feed(b"\r\n");
        }
        super::SGR.with(|c| c.set(0));
        t.dump();
        assert!(super::SGR.with(|c| c.get()) <= 30, "one SGR per row, not per cell");
    }

    #[test]
    fn a_narrowed_then_widened_pane_shows_its_lines_whole() {
        let mut t = super::Term::new(80, 6, 100);
        t.feed(b"~/dev/projects/my-project main\r\n.rw-r--r--@ 21k user 20260930-123448-README.md\r\n$ ");
        t.resize(22, 6);
        let narrow = t.all_lines();
        assert!(narrow.contains(&"~/dev/projects/my-proj".to_string()) && narrow.concat().contains("~/dev/projects/my-project main"), "wrapped, not cut: {narrow:?}");
        t.feed(b"echo more");
        t.resize(80, 6);
        let rows: Vec<String> = t.all_lines().into_iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(rows, ["~/dev/projects/my-project main", ".rw-r--r--@ 21k user 20260930-123448-README.md", "$ echo more"]);
    }

    #[test]
    fn a_screen_dumped_by_the_vt100_build_restores() {
        let mut t = super::Term::new(20, 5, 100);
        t.restore(include_bytes!("../tests/fixtures/dump-vt100.bin"), 0);
        let lines: Vec<String> = t.all_lines().into_iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, ["one", "two", "three", "four", "five", "six", "red bold", "wide 中文 ok"]);
        let s = t.screen();
        assert_eq!(s.cursor_position(), (2, 4));
        assert_eq!(s.mouse_protocol_mode(), super::MouseProtocolMode::PressRelease);
        assert!(s.bracketed_paste());
        let red = (0..5).find_map(|r| s.cell(r, 0).filter(|c| c.contents() == "r")).expect("the red row");
        assert_eq!(red.fgcolor(), super::Color::Idx(1));
        assert!(s.cell(4, 5).is_some_and(|c| c.is_wide() && c.contents() == "中") && s.cell(4, 6).is_some_and(|c| c.is_wide_continuation()));

        let mut a = super::Term::new(20, 5, 0);
        a.restore(include_bytes!("../tests/fixtures/dump-vt100-alt.bin"), 0);
        assert!(a.screen().alternate_screen());
        assert_eq!(a.screen().cell(1, 2).map(|c| c.fgcolor()), Some(super::Color::Idx(2)));
    }

    #[test]
    fn a_dump_keeps_colors_in_history_and_its_lines_reflow_after_restore() {
        let mut t = super::Term::new(10, 3, 100);
        t.feed(b"\x1b[32mgreen\x1b[0m\r\n0123456789abcdef\r\nx\r\ny\r\nz");
        let mut u = super::Term::new(10, 3, 100);
        u.restore(&t.dump(), 0);
        assert_eq!(u.all_lines(), t.all_lines());
        u.resize(20, 3);
        assert!(u.all_lines().contains(&"0123456789abcdef".to_string()), "{:?}", u.all_lines());
        u.scroll_by(-10);
        let (offset, _, _) = u.history();
        let top = u.screen().cell(0, 0).expect("a history row");
        assert!(offset > 0 && top.contents() == "g" && top.fgcolor() == super::Color::Idx(2), "green, scrolled into view");
    }

    #[test]
    fn text_keeps_the_last_lines_under_the_cap() {
        let mut t = super::Term::new(20, 4, 50);
        for i in 0..200 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let text = t.text(40);
        let s = String::from_utf8(text.clone()).unwrap();
        assert!(text.len() <= 40, "cap respected: {}", text.len());
        assert!(s.ends_with("line 199\r\n"), "ends with the last line: {s:?}");
        assert!(!s.ends_with("\r\n\r\n"), "no trailing blank lines: {s:?}");
        let mut u = super::Term::new(20, 4, 50);
        u.feed(&text);
        assert!(u.screen().contents().contains("line 199"));
    }

    #[test]
    fn answers_da_and_cursor_position() {
        let mut t = super::Term::new(20, 5, 0);
        t.feed(b"ab");
        let (fg, bg) = ((0x1e, 0x20, 0x24), (0xfa, 0xfa, 0xfb));
        assert_eq!(t.answers(b"x\x1b[c\x1b[6n\x1b[?6n\x1b[?25l", fg, bg), b"\x1b[?1;2c\x1b[1;3R\x1b[?1;3R".to_vec());
        assert!(t.answers(b"plain \x1b[31m text", fg, bg).is_empty());
    }

    #[test]
    fn a_program_that_asks_to_hear_of_theme_changes_is_remembered_and_answered() {
        let mut t = super::Term::new(20, 5, 0);
        let (fg, bg) = ((0x1e, 0x20, 0x24), (0xfa, 0xfa, 0xfb));
        assert_eq!(t.answers(b"\x1b[?2031$p", fg, bg), b"\x1b[?2031;2$y".to_vec(), "known, not set yet");
        t.feed(b"\x1b[?2004;2031h");
        assert!(t.theme_notify, "set among other modes");
        assert_eq!(t.answers(b"\x1b[?2031$p", fg, bg), b"\x1b[?2031;1$y".to_vec());
        assert_eq!(t.answers(b"\x1b[?996n", fg, bg), b"\x1b[?997;2n".to_vec(), "a light ground");
        assert_eq!(t.answers(b"\x1b[?996n", fg, (0x13, 0x14, 0x17)), b"\x1b[?997;1n".to_vec(), "a dark ground");
        t.feed(b"\x1b[?2031l");
        assert!(!t.theme_notify, "and reset");
        t.feed(b"\x1b[?20312h");
        assert!(!t.theme_notify, "another mode that starts with the same digits is not it");
    }

    #[test]
    fn answers_the_colour_questions_with_the_theme() {
        let t = super::Term::new(20, 5, 0);
        let (fg, bg) = ((0x1e, 0x20, 0x24), (0xfa, 0xfa, 0xfb));
        assert_eq!(t.answers(b"\x1b]11;?\x07", fg, bg), b"\x1b]11;rgb:fafa/fafa/fbfb\x07".to_vec(), "the ground, ended with BEL as asked");
        assert_eq!(t.answers(b"\x1b]10;?\x1b\\", fg, bg), b"\x1b]10;rgb:1e1e/2020/2424\x1b\\".to_vec(), "the text, ended with ST as asked");
        assert!(t.answers(b"\x1b]11;#000000\x07", fg, bg).is_empty(), "setting the colour is not a question");
    }

    use super::*;

    #[test]
    fn scroll_counter_never_overshoots_the_history() {
        let mut t = Term::new(10, 3, 100);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        t.scroll_by(-50);
        let (offset, above, _) = t.history();
        assert_eq!(above, 0, "hit the top of history");
        assert_eq!(t.scroll, offset, "the counter must match the emulator");
        assert!(t.scroll_by(1), "a step down should work right away");
    }

    #[test]
    fn history_counts_lines_scrolled_off_and_restores_offset() {
        let mut t = Term::new(10, 3, 100);
        assert_eq!(t.history(), (0, 0, false));
        t.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        let (offset, above, alt) = t.history();
        assert_eq!((offset, alt), (0, false));
        assert!(above >= 2, "there should be at least two lines above the screen, not {above}");
        t.scroll_by(-1);
        let (offset, _, _) = t.history();
        assert_eq!(offset, 1, "the probe shouldn't disturb the scroll position");
        assert_eq!(t.history().0, 1);
    }

    #[test]
    fn all_lines_gives_history_then_screen_and_restores_offset() {
        let mut t = Term::new(10, 3, 100);
        for i in 1..=8 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        t.scroll_by(-2);
        let lines = t.all_lines();
        assert_eq!(t.history().0, 2, "the probe doesn't disturb the scroll position");

        let text: Vec<&str> = lines.iter().map(|s| s.trim_end()).collect();
        assert_eq!(text, ["1", "2", "3", "4", "5", "6", "7", "8", ""]);
        assert_eq!(lines.len(), 6 + 3, "history plus screen height");
    }

    #[test]
    fn all_lines_handles_an_empty_history() {
        let mut t = Term::new(10, 3, 100);
        t.feed(b"hi");
        let lines = t.all_lines();
        assert_eq!(lines.len(), 3, "screen only");
        assert_eq!(lines[0].trim_end(), "hi");
    }

    #[test]
    fn title_from_osc_becomes_agent_name() {
        let mut t = Term::new(80, 24, 100);
        t.feed(b"\x1b]0;\xe2\x9c\xb3 [tmux] Sandbox AI\x07");
        assert_eq!(t.title(), Some("✳ [tmux] Sandbox AI"));
        assert_eq!(t.clean_title(), Some("[tmux] Sandbox AI"));
        t.feed(b"\x1b]0;(draft) ~/x\x07");
        assert_eq!(t.clean_title(), Some("(draft) ~/x"));
    }

    #[test]
    fn wheel_goes_to_child_only_when_it_asked() {
        let mut t = Term::new(80, 24, 100);
        assert_eq!(t.wheel_routing(), WheelRouting::Scrollback);

        t.feed(b"\x1b[?1049h");
        assert_eq!(t.wheel_routing(), WheelRouting::Arrows { app_cursor: false });
        t.feed(b"\x1b[?1h");
        assert_eq!(t.wheel_routing(), WheelRouting::Arrows { app_cursor: true });

        t.feed(b"\x1b[?1000h");
        assert!(t.wants_mouse());
        assert_eq!(t.wheel_routing(), WheelRouting::ToChild);
    }

    #[test]
    fn blocked_is_detected_from_permission_prompt() {
        let mut t = Term::new(80, 24, 100);
        t.feed("Do you want to proceed?".as_bytes());
        assert_ne!(t.state(), AgentState::Blocked, "a shell's text is not a question");
        t.set_blocked_text(crate::agents::get("claude").unwrap().agent.blocked_text);
        assert_eq!(t.state(), AgentState::Blocked);
    }

    #[test]
    fn finished_work_shows_done_until_seen() {
        let mut t = Term::new(80, 24, 100);
        t.feed(b"hello");
        t.tick_state(); // working
        t.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        t.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        assert!(t.tick_state(), "finished, and nobody's looked yet: the caller must redraw");
        assert_eq!(t.state(), AgentState::Done);
        t.mark_seen();
        assert!(!t.tick_state(), "nothing new");
        assert_eq!(t.state(), AgentState::Idle);
    }

    #[test]
    fn activity_is_a_change_of_state_not_every_byte() {
        let mut t = Term::new(80, 24, 100);
        t.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        t.feed(b"prompt");
        t.tick_state();
        assert_eq!(t.changed_at(), None, "launch is not activity");
        t.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        t.tick_state();
        let finished = t.changed_at().expect("working -> idle is activity");
        t.feed(b"more");
        t.tick_state();
        let started = t.changed_at().unwrap();
        assert!(started > finished, "starting again is newer");
        t.feed(b"still going");
        t.tick_state();
        assert_eq!(t.changed_at(), Some(started), "output while working moves nothing");
    }

    #[test]
    fn a_replayed_screen_is_neither_activity_nor_done() {
        let mut t = Term::new(80, 24, 100);
        t.replay(b"old text from before the restart");
        t.tick_state();
        t.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        t.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        t.tick_state();
        assert_eq!(t.state(), AgentState::Idle);
        assert_eq!(t.changed_at(), None);
    }

    #[test]
    fn an_update_keeps_recent_activity_and_unseen_done() {
        let mut t = Term::new(80, 24, 100);
        t.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        t.feed(b"work");
        t.tick_state();
        t.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        t.tick_state();
        assert_eq!(t.state(), AgentState::Done);

        let (since, unseen) = t.activity();
        let mut u = Term::new(80, 24, 100);
        u.restore(&t.dump(), 0);
        u.carry(since, unseen);
        u.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        u.tick_state();
        assert_eq!(u.state(), AgentState::Done, "the ✓ survives");
        let (was, now) = (t.changed_at().unwrap(), u.changed_at().unwrap());
        assert!(now.max(was) - now.min(was) < std::time::Duration::from_millis(100), "so does the moment");
    }

    #[test]
    fn fresh_empty_pane_is_unknown_not_idle() {
        let t = Term::new(80, 24, 100);
        let mut t2 = t;
        t2.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        assert_eq!(t2.state(), AgentState::Unknown);
    }

    #[test]
    fn a_quiet_shell_builds_no_screen_text_per_tick() {
        let mut t = Term::new(200, 50, 100);
        t.feed(b"$ ls");
        t.started = std::time::Instant::now() - STARTUP_GRACE * 2;
        t.last_output = std::time::Instant::now() - IDLE_AFTER * 2;
        CONTENTS.with(|c| c.set(0));
        for _ in 0..10 {
            t.tick_state();
        }
        assert_eq!(t.state(), AgentState::Idle);
        assert_eq!(CONTENTS.with(|c| c.get()), 0);
        t.set_blocked_text(vec!["Do you want".into()]);
        t.tick_state();
        assert_eq!(CONTENTS.with(|c| c.get()), 1, "an agent's prompt is still read");
    }

    #[test]
    fn priority_orders_loud_before_quiet() {
        assert!(AgentState::Failed.priority() > AgentState::Blocked.priority());
        assert!(AgentState::Blocked.priority() > AgentState::Done.priority());
        assert!(AgentState::Done.priority() > AgentState::Working.priority());
        assert!(AgentState::Working.priority() > AgentState::Idle.priority());
        assert!(AgentState::Idle.priority() > AgentState::Unknown.priority());
    }
}
