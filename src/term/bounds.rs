//! What a pane's output may make the emulator hold or do: a byte filter in front of vte for the caps that are byte
//! properties, and a Handler in front of alacritty's Term for the ones that are emulator state.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{
    self, Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes, KeyboardModesApplyBehavior, LineClearMode,
    Mode, ModifyOtherKeys, PrivateMode, Rgb, ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode, cursor_icon::CursorIcon,
};

/// GNOME VTE's cap on an OSC string, in code points; the OSC number and its `;` count.
const OSC_CHARS: u16 = 4096;
/// The same in bytes, for runs that are not UTF-8: 4096 four-byte characters.
const OSC_BYTES: u16 = 16384;
// ceiling: 30 marks per cell (UAX #15 Stream-Safe); a pane full of marked cells still holds ~10× a plain one, bounded by Scrollback
pub const ZERO_WIDTH: usize = 30;
/// GNOME VTE's title stack depth; the oldest goes when it is full.
const TITLES: usize = 8;
/// vte's own private mode number for a synchronized update.
const SYNC: u16 = 2026;

pub const HIT_OSC: u8 = 1;
pub const HIT_REP: u8 = 2;
pub const HIT_MARKS: u8 = 4;
pub const HIT_TITLES: u8 = 8;

/// The caps that fired, as the log names them.
pub fn describe(hits: u8) -> String {
    [
        (HIT_OSC, "an OSC over 4096 characters"),
        (HIT_REP, "a repeat wider than the pane"),
        (HIT_MARKS, "a character with over 30 combining marks"),
        (HIT_TITLES, "over 8 saved titles"),
    ]
    .iter()
    .filter(|(bit, _)| hits & bit != 0)
    .map(|(_, what)| *what)
    .collect::<Vec<_>>()
    .join(", ")
}

/// A CSI sequence as far as the caps need it: REP's count and whether it names mode 2026.
#[derive(Clone, Copy, Default)]
struct Csi {
    first: u16,
    first_done: bool,
    seps: u8,
    marker: u8,
    /// No byte vte collects has come yet: the next 0x3C–0x3F is the marker.
    entry: bool,
    /// An intermediate, or vte's CsiIgnore: neither REP nor a mode.
    other: bool,
    cur: u16,
    sub: bool,
    named: bool,
}

#[derive(Clone, Copy, Default)]
enum State {
    /// Ground, DCS, SOS/PM/APC and EscapeIntermediate: only ESC matters there.
    #[default]
    Other,
    Esc,
    Csi(Csi),
    Osc { cps: u16, bytes: u16 },
    OscCut,
}

/// Mirrors the part of vte 0.15's state machine the caps need; every byte passes except where a cap cuts.
#[derive(Default)]
pub struct Filter {
    state: State,
}

impl Filter {
    /// Filters `bytes` into `out`; `marks` gets the offsets in `out` right after each begin (true) or end of a
    /// synchronized update, as vte dispatches them.
    pub fn pass(&mut self, bytes: &[u8], cols: u16, hits: &mut u8, out: &mut Vec<u8>, marks: &mut Vec<(usize, bool)>) {
        // Bytes pass in runs: `out` is written only where a cap cuts, from `from` on.
        let (mut i, mut from) = (0, 0);
        while i < bytes.len() {
            let rest = &bytes[i..];
            match &mut self.state {
                State::Other => {
                    let n = find_esc(rest);
                    i += n;
                    if n == rest.len() {
                        continue;
                    }
                    i += 1;
                    // The common case at once: a whole CSI whose final needs nothing (SGR, cursor moves, erases).
                    if let [b'[', tail @ ..] = &bytes[i..]
                        && let Some(m) = tail.iter().position(|&b| matches!(b, 0x18 | 0x1a | 0x1b | 0x40..=0x7e))
                        && matches!(tail[m], 0x40..=0x7e)
                        && !matches!(tail[m], b'b' | b'h' | b'l')
                    {
                        i += 2 + m;
                        continue;
                    }
                    self.state = State::Esc;
                }
                State::Esc => {
                    i += 1;
                    self.state = match rest[0] {
                        0x18 | 0x1a => State::Other,
                        0x00..=0x1f | 0x7f..=0xff => State::Esc,
                        b'[' => State::Csi(Csi { entry: true, ..Default::default() }),
                        b']' => State::Osc { cps: 0, bytes: 0 },
                        _ => State::Other,
                    };
                }
                State::Csi(c) => {
                    let Some(n) = rest.iter().position(|&b| matches!(b, 0x18 | 0x1a | 0x1b | 0x40..=0x7e)) else {
                        rest.iter().for_each(|&b| c.step(b));
                        break;
                    };
                    i += n + 1;
                    self.state = match rest[n] {
                        0x1b => State::Esc,
                        // Only these finals need the parameters, so only they are stepped through.
                        f @ (b'b' | b'h' | b'l') => {
                            let mut c = *c;
                            rest[..n].iter().for_each(|&b| c.step(b));
                            match c.end(f, cols) {
                                End::Rep => {
                                    *hits |= HIT_REP;
                                    out.extend_from_slice(&bytes[from..i - 1]);
                                    out.push(0x18);
                                    out.extend_from_slice(format!("\x1b[{cols}b").as_bytes());
                                    from = i;
                                }
                                End::Mark(begin) => marks.push((out.len() + i - from, begin)),
                                End::Pass => {}
                            }
                            State::Other
                        }
                        _ => State::Other,
                    };
                }
                State::Osc { cps, bytes: n } => {
                    let end = rest.iter().position(|&b| matches!(b, 0x07 | 0x18 | 0x1a | 0x1b)).unwrap_or(rest.len());
                    let mut k = 0;
                    while k < end {
                        let starts = !(0x80..=0xbf).contains(&rest[k]);
                        if (starts && *cps >= OSC_CHARS) || *n >= OSC_BYTES {
                            break;
                        }
                        *cps += starts as u16;
                        *n += 1;
                        k += 1;
                    }
                    i += k;
                    if k < end {
                        *hits |= HIT_OSC;
                        out.extend_from_slice(&bytes[from..i]);
                        from = i;
                        self.state = State::OscCut;
                    } else if end < rest.len() {
                        i += 1;
                        self.state = after_osc(rest[end]);
                    }
                }
                State::OscCut => {
                    let Some(end) = rest.iter().position(|&b| matches!(b, 0x07 | 0x18 | 0x1a | 0x1b)) else {
                        i = bytes.len();
                        from = i;
                        break;
                    };
                    i += end;
                    from = i;
                    i += 1;
                    self.state = after_osc(rest[end]);
                }
            }
        }
        out.extend_from_slice(&bytes[from..]);
    }
}

/// How many continuation bytes the character cut at the end of `slice` still needs, `before` being the last slice's.
pub fn pending(slice: &[u8], before: u8) -> u8 {
    for (after, &b) in slice.iter().rev().take(4).enumerate() {
        let need = match b {
            0x80..=0xbf => continue,
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => return 0,
        };
        return (need as usize).saturating_sub(after) as u8;
    }
    if slice.len() < 4 { before.saturating_sub(slice.len() as u8) } else { 0 }
}

/// After an OSC's terminator: ESC begins the next sequence.
fn after_osc(b: u8) -> State {
    if b == 0x1b { State::Esc } else { State::Other }
}

/// What a CSI's final byte asks of the filter.
enum End {
    Pass,
    /// A REP wider than the pane: CAN in place of its final, then one of the pane's width.
    Rep,
    /// A mode naming 2026: true for a begin.
    Mark(bool),
}

/// The first ESC, eight bytes at a time.
fn find_esc(b: &[u8]) -> usize {
    let mut i = 0;
    while let Some(w) = b.get(i..i + 8) {
        let x = u64::from_le_bytes(w.try_into().unwrap()) ^ 0x1b1b_1b1b_1b1b_1b1b;
        if x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080 != 0 {
            break;
        }
        i += 8;
    }
    i + b[i..].iter().position(|&x| x == 0x1b).unwrap_or(b.len() - i)
}

impl Csi {
    /// A byte inside the sequence: not CAN, SUB, ESC or a final.
    fn step(&mut self, b: u8) {
        if self.other {
            return;
        }
        match b {
            0x20..=0x2f => self.other = true,
            b'0'..=b'9' => {
                self.entry = false;
                let d = (b - b'0') as u16;
                if !self.first_done {
                    self.first = self.first.saturating_mul(10).saturating_add(d);
                }
                if !self.sub {
                    self.cur = self.cur.saturating_mul(10).saturating_add(d);
                }
            }
            b':' | b';' => {
                self.entry = false;
                self.first_done = true;
                self.seps = self.seps.saturating_add(1);
                if self.cur == SYNC {
                    self.named = true;
                }
                if b == b';' {
                    (self.cur, self.sub) = (0, false);
                } else {
                    self.sub = true;
                }
            }
            0x3c..=0x3f if self.entry => {
                self.marker = b;
                self.entry = false;
            }
            0x3c..=0x3f => self.other = true,
            // Executed or ignored by vte without leaving the sequence.
            _ => {}
        }
    }

    /// The final byte, as vte dispatches it: REP with a count, or a private mode.
    fn end(mut self, f: u8, cols: u16) -> End {
        if self.cur == SYNC && !self.other {
            self.named = true;
        }
        let plain = !self.other && self.seps < 32;
        if f == b'b' && plain && self.marker == 0 && self.first.max(1) > cols {
            End::Rep
        } else if matches!(f, b'h' | b'l') && plain && self.marker == b'?' && self.named {
            End::Mark(f == b'h')
        } else {
            End::Pass
        }
    }
}

/// The title stack nerd keeps in place of alacritty's 4096-deep one.
#[derive(Default)]
pub struct Titles {
    stack: Vec<Option<String>>,
    now: Option<String>,
}

#[cfg(test)]
impl Titles {
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

/// vte never buffers a synchronized update: Term holds it, in slices it can bound
#[derive(Default)]
pub struct NoSync;

impl ansi::Timeout for NoSync {
    fn set_timeout(&mut self, _: std::time::Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// alacritty's Term behind the caps that are its state: marks per cell, the title stack, links, back tabs.
pub struct Guard<'a, T> {
    pub term: &'a mut alacritty_terminal::Term<T>,
    pub titles: &'a mut Titles,
    pub hits: &'a mut u8,
}

#[cfg(test)]
thread_local! {
    /// How many back-tab passes alacritty was asked for, so a test can count them.
    pub static BACK_TABS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

macro_rules! guard {
    ({ $($own:tt)* } $($name:ident($($arg:ident: $ty:ty),*);)*) => {
        impl<T: EventListener> Handler for Guard<'_, T> {
            $($own)*
            $(#[inline] fn $name(&mut self, $($arg: $ty),*) { Handler::$name(self.term, $($arg),*) })*
        }
        #[cfg(test)]
        pub const FORWARDED: &[&str] = &[$(stringify!($name)),*];
    };
}

guard! {
    {
        #[inline]
        fn input(&mut self, c: char) {
            if c.is_ascii() {
                return self.term.input(c);
            }
            // The cell a zero-width char would join, found as alacritty's input finds it.
            let g = self.term.grid();
            let mut at = g.cursor.point;
            if !g.cursor.input_needs_wrap {
                at.column = Column(at.column.saturating_sub(1));
            }
            if g[at].flags.contains(Flags::WIDE_CHAR_SPACER) {
                at.column = Column(at.column.saturating_sub(1));
            }
            let marks = |t: &alacritty_terminal::Term<T>, p: Point| t.grid()[p].zerowidth().map_or(0, <[char]>::len);
            if marks(self.term, at) < ZERO_WIDTH {
                return self.term.input(c);
            }
            // alacritty's own width table: a zero-width char here would only be undone below, at a deep copy.
            if unicode_width::UnicodeWidthChar::width(c) == Some(0) {
                *self.hits |= HIT_MARKS;
                return;
            }
            let kept = g[at].clone();
            self.term.input(c);
            if marks(self.term, at) > ZERO_WIDTH {
                self.term.grid_mut()[at] = kept;
                *self.hits |= HIT_MARKS;
            }
        }

        fn move_backward_tabs(&mut self, count: u16) {
            for _ in 0..count {
                let before = self.term.grid().cursor.point.column;
                #[cfg(test)]
                BACK_TABS.with(|n| n.set(n.get() + 1));
                self.term.move_backward_tabs(1);
                if self.term.grid().cursor.point.column == before {
                    break;
                }
            }
        }

        fn set_title(&mut self, title: Option<String>) {
            self.titles.now = title.clone();
            self.term.set_title(title);
        }

        fn push_title(&mut self) {
            if self.titles.stack.len() >= TITLES {
                self.titles.stack.remove(0);
                *self.hits |= HIT_TITLES;
            }
            self.titles.stack.push(self.titles.now.clone());
        }

        fn pop_title(&mut self) {
            if let Some(t) = self.titles.stack.pop() {
                self.set_title(t);
            }
        }

        fn reset_state(&mut self) {
            *self.titles = Titles::default();
            self.term.reset_state();
        }

        // nerd shows no links: a link kept per cell would keep its URI per cell.
        fn set_hyperlink(&mut self, _: Option<Hyperlink>) {}
    }
    set_cursor_style(s: Option<CursorStyle>);
    set_cursor_shape(s: CursorShape);
    goto(line: i32, col: usize);
    goto_line(line: i32);
    goto_col(col: usize);
    insert_blank(n: usize);
    move_up(n: usize);
    move_down(n: usize);
    identify_terminal(i: Option<char>);
    device_status(n: usize);
    move_forward(n: usize);
    move_backward(n: usize);
    move_down_and_cr(n: usize);
    move_up_and_cr(n: usize);
    put_tab(n: u16);
    backspace();
    carriage_return();
    linefeed();
    bell();
    substitute();
    newline();
    set_horizontal_tabstop();
    scroll_up(n: usize);
    scroll_down(n: usize);
    insert_blank_lines(n: usize);
    delete_lines(n: usize);
    erase_chars(n: usize);
    delete_chars(n: usize);
    move_forward_tabs(n: u16);
    save_cursor_position();
    restore_cursor_position();
    clear_line(m: LineClearMode);
    clear_screen(m: ClearMode);
    clear_tabs(m: TabulationClearMode);
    set_tabs(n: u16);
    reverse_index();
    terminal_attribute(a: Attr);
    set_mode(m: Mode);
    unset_mode(m: Mode);
    report_mode(m: Mode);
    set_private_mode(m: PrivateMode);
    unset_private_mode(m: PrivateMode);
    report_private_mode(m: PrivateMode);
    set_scrolling_region(top: usize, bottom: Option<usize>);
    set_keypad_application_mode();
    unset_keypad_application_mode();
    set_active_charset(i: CharsetIndex);
    configure_charset(i: CharsetIndex, c: StandardCharset);
    set_color(i: usize, c: Rgb);
    dynamic_color_sequence(p: String, i: usize, t: &str);
    reset_color(i: usize);
    clipboard_store(c: u8, b: &[u8]);
    clipboard_load(c: u8, t: &str);
    decaln();
    text_area_size_pixels();
    text_area_size_chars();
    set_mouse_cursor_icon(i: CursorIcon);
    report_keyboard_mode();
    push_keyboard_mode(m: KeyboardModes);
    pop_keyboard_modes(n: u16);
    set_keyboard_mode(m: KeyboardModes, b: KeyboardModesApplyBehavior);
    set_modify_other_keys(m: ModifyOtherKeys);
    report_modify_other_keys();
    set_scp(p: ScpCharPath, u: ScpUpdateMode);
}

/// The methods Guard answers itself rather than forwarding.
#[cfg(test)]
pub const OWN: &[&str] = &["input", "move_backward_tabs", "set_title", "push_title", "pop_title", "reset_state", "set_hyperlink"];

#[cfg(test)]
pub(crate) mod tests;
