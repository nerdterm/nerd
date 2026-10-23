//! Search over a pane: the visible screen plus the whole scrollback history.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::render::Theme;
use crate::term::Term;

/// `row` is counted from the top of the live screen (negative is history), so it
/// doesn't depend on scroll offset and the match list survives paging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: isize,
    pub col: u16,
    pub len: u16,
}

pub enum SearchOutcome {
    None,
    /// The query or current match changed: rebuild, scroll, redraw.
    Changed,
    Close,
}

pub struct Search {
    pub query: String,
    matches: Vec<Match>,
    current: usize,
    /// While true, letters go into the query and `n` is a letter; after Enter, `n` means "next match".
    typing: bool,
    /// The pane's edits and the query at the last run: the same pair finds the same matches.
    ran: Option<(u64, String)>,
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}

impl Search {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            matches: Vec::new(),
            current: 0,
            typing: true,
            ran: None,
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> SearchOutcome {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            // Only closes on an empty query, so one stray Esc doesn't lose both the search and the place in history.
            KeyCode::Esc => {
                if self.query.is_empty() {
                    SearchOutcome::Close
                } else {
                    self.query.clear();
                    self.matches.clear();
                    self.ran = None;
                    self.current = 0;
                    self.typing = true;
                    SearchOutcome::Changed
                }
            }
            KeyCode::Backspace if self.typing => {
                self.query.pop();
                self.current = 0;
                SearchOutcome::Changed
            }
            // The match is already chosen in run(), so Enter only ends typing.
            KeyCode::Enter if self.typing && !shift => {
                self.typing = false;
                SearchOutcome::None
            }
            KeyCode::Enter if shift => self.step(-1),
            KeyCode::Enter => self.step(1),
            KeyCode::Char(c) if self.typing => {
                self.query.push(c);
                self.current = 0;
                SearchOutcome::Changed
            }
            _ => SearchOutcome::None,
        }
    }

    fn step(&mut self, d: isize) -> SearchOutcome {
        if self.matches.is_empty() {
            return SearchOutcome::None;
        }
        let n = self.matches.len() as isize;
        self.current = ((self.current as isize + d).rem_euclid(n)) as usize;
        SearchOutcome::Changed
    }

    /// Rebuilds the match list over the whole pane; editing the query is what resets the current match.
    pub fn run(&mut self, term: &mut Term) {
        let key = (term.edits(), self.query.clone());
        if self.ran.as_ref() == Some(&key) {
            self.current = self.current.min(self.matches.len().saturating_sub(1));
            return;
        }
        self.ran = Some(key);
        self.matches.clear();
        if self.query.is_empty() {
            self.current = 0;
            return;
        }
        let needle: Vec<char> = self.query.chars().map(lower).collect();
        let ascii = self.query.to_ascii_lowercase().into_bytes();
        let rows = term.screen().size().0 as usize;
        let lines = term.all_lines();
        let history = lines.len().saturating_sub(rows) as isize;
        let (mut bytes, mut chars, plain) = (Vec::new(), Vec::new(), self.query.is_ascii());
        for (i, line) in lines.iter().enumerate() {
            let row = i as isize - history;
            let mut hit = |col: usize| self.matches.push(Match { row, col: col as u16, len: needle.len() as u16 });
            // A byte is a column only when the whole line is ASCII.
            if plain && line.is_ascii() {
                bytes.clear();
                bytes.extend(line.bytes().map(|b| b.to_ascii_lowercase()));
                find(&bytes, &ascii, &mut hit);
            } else {
                chars.clear();
                chars.extend(line.chars().map(lower));
                find(&chars, &needle, &mut hit);
            }
        }
        self.current = self.current.min(self.matches.len().saturating_sub(1));
    }

    /// Of the matches on rows `same` accepts, else of all: the first on this row, else the nearest above it, else the
    /// nearest below. What the pane printed since the row was taken mostly moves its lines up.
    pub fn choose_row(&mut self, row: isize, same: impl Fn(isize) -> bool) {
        let near = |i: &usize| (self.matches[*i].row > row, self.matches[*i].row.abs_diff(row));
        let pick = (0..self.matches.len()).filter(|&i| same(self.matches[i].row)).min_by_key(near).or_else(|| (0..self.matches.len()).min_by_key(near));
        if let Some(i) = pick {
            self.current = i;
        }
    }

    /// A pasted line into the query, as typing it would.
    pub fn paste(&mut self, s: &str) {
        self.query.push_str(s);
        self.current = 0;
        self.typing = true;
    }

    pub fn current(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    pub fn counter(&self) -> String {
        if self.matches.is_empty() {
            "0/0".into()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        }
    }

    /// The scroll offset that puts the current match on screen: `-row` for history, 0 if already visible.
    /// Caller: `term.scroll_by(term.scroll as isize - target as isize)`.
    pub fn scroll_target(&self) -> Option<usize> {
        self.current()
            .map(|m| if m.row < 0 { (-m.row) as usize } else { 0 })
    }

    pub fn draw_bar(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let y = area.y;
        let base = Style::new().bg(theme.surface).fg(theme.text);
        for x in area.x..area.x + area.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_symbol(" ").set_style(base);
            }
        }
        let counter = self.counter();
        let cw = counter.chars().count() as u16;
        buf.set_stringn(
            area.x,
            y,
            format!("/{}", self.query),
            area.width.saturating_sub(cw + 1) as usize,
            base,
        );
        if area.width > cw {
            buf.set_string(
                area.x + area.width - cw,
                y,
                &counter,
                base.fg(theme.muted),
            );
        }
    }

    /// The current match is yellow, the rest muted, so "where am I" reads at a glance.
    pub fn highlight(&self, buf: &mut Buffer, inner: Rect, offset: usize, theme: &Theme) {
        for (i, m) in self.matches.iter().enumerate() {
            let y = m.row + offset as isize;
            if y < 0 || y >= inner.height as isize {
                continue;
            }
            let bg = if i == self.current { theme.warn } else { theme.muted };
            for dx in 0..m.len {
                let x = m.col + dx;
                if x >= inner.width {
                    break;
                }
                if let Some(c) = buf.cell_mut((inner.x + x, inner.y + y as u16)) {
                    c.set_bg(bg).set_fg(theme.ground);
                }
            }
        }
    }
}

/// Each non-overlapping place `needle` starts in `hay`, left to right.
fn find<T: PartialEq>(hay: &[T], needle: &[T], hit: &mut impl FnMut(usize)) {
    let mut from = 0;
    while from + needle.len() <= hay.len() {
        if hay[from..from + needle.len()] == needle[..] {
            hit(from);
            from += needle.len();
        } else {
            from += 1;
        }
    }
}

/// ceiling: matches by character index, not display width; wide/CJK characters will misalign the highlight - needs a width table.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn changed(o: SearchOutcome) -> bool {
        matches!(o, SearchOutcome::Changed)
    }

    fn term_with(rows: u16, lines: &[&str]) -> Term {
        let mut t = Term::new(40, rows, 10_000);
        for l in lines {
            t.feed(l.as_bytes());
            t.feed(b"\r\n");
        }
        t
    }

    fn typed(s: &mut Search, q: &str) {
        for c in q.chars() {
            s.key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn finds_matches_on_screen_and_in_history() {
        let mut t = term_with(3, &["needle one", "x", "y", "z", "needle two"]);
        let mut s = Search::new();
        typed(&mut s, "needle");
        s.run(&mut t);
        assert_eq!(s.counter(), "1/2");
        let rows: Vec<isize> = s.matches.iter().map(|m| m.row).collect();
        assert!(rows[0] < 0, "the first match should be in history, not {}", rows[0]);
        assert!(rows[1] >= 0, "the second should be on the live screen, not {}", rows[1]);
        assert_eq!(s.matches[0].col, 0);
        assert_eq!(s.matches[0].len, 6);
    }

    #[test]
    fn a_rerun_reads_the_pane_again_only_after_output() {
        let mut t = term_with(5, &["needle"]);
        let mut s = Search::new();
        typed(&mut s, "needle");
        s.run(&mut t);
        crate::term::LINES.with(|c| c.set(0));
        s.run(&mut t);
        assert_eq!(crate::term::LINES.with(|c| c.get()), 0, "nothing new: the matches stand");
        t.feed(b"needle again");
        s.run(&mut t);
        assert_eq!((crate::term::LINES.with(|c| c.get()), s.counter()), (1, "1/2".into()));
        t.resize(20, 5);
        s.run(&mut t);
        assert_eq!(crate::term::LINES.with(|c| c.get()), 2, "a resize moves the rows");
    }

    #[test]
    fn a_line_with_wide_text_keeps_columns_in_characters() {
        let mut t = term_with(5, &["\u{e9}t\u{e9} Needle", "plain needle"]);
        let mut s = Search::new();
        typed(&mut s, "NEEDLE");
        s.run(&mut t);
        assert_eq!(s.matches.iter().map(|m| (m.col, m.len)).collect::<Vec<_>>(), [(4, 6), (6, 6)]);
    }

    #[test]
    fn matching_ignores_case() {
        let mut t = term_with(5, &["Error: BOOM", "error again"]);
        let mut s = Search::new();
        typed(&mut s, "ERROR");
        s.run(&mut t);
        assert_eq!(s.counter(), "1/2");
    }

    #[test]
    fn enter_and_shift_enter_wrap_around() {
        let mut t = term_with(5, &["aa", "aa", "aa"]);
        let mut s = Search::new();
        typed(&mut s, "aa");
        s.run(&mut t);
        s.key(key(KeyCode::Enter));
        assert_eq!(s.counter(), "1/3");
        assert!(changed(s.key(key(KeyCode::Enter))));
        assert_eq!(s.counter(), "2/3");
        s.key(key(KeyCode::Enter));
        assert_eq!(s.counter(), "3/3");
        s.key(key(KeyCode::Enter));
        assert_eq!(s.counter(), "1/3", "from the last one, wraps to the first");
        s.key(shift_key(KeyCode::Enter));
        assert_eq!(s.counter(), "3/3", "backward from the first, wraps to the last");
    }

    #[test]
    fn enter_steps_when_the_app_reruns_the_search_after_each_key() {
        let mut t = term_with(5, &["aa", "aa", "aa"]);
        let mut s = Search::new();
        typed(&mut s, "aa");
        s.run(&mut t);
        s.key(key(KeyCode::Enter));
        for want in ["2/3", "3/3", "1/3"] {
            s.key(key(KeyCode::Enter));
            s.run(&mut t);
            assert_eq!(s.counter(), want);
        }
        s.key(shift_key(KeyCode::Enter));
        s.run(&mut t);
        assert_eq!(s.counter(), "3/3");
        s.key(key(KeyCode::Esc));
        typed(&mut s, "a");
        s.run(&mut t);
        assert_eq!(s.counter(), "1/6", "a new query starts at the first match");
    }

    #[test]
    fn a_letter_edits_the_query_and_only_enter_steps() {
        let mut t = term_with(5, &["none", "nn"]);
        let mut s = Search::new();
        typed(&mut s, "n");
        assert_eq!(s.query, "n", "while typing, a letter goes into the query");
        s.run(&mut t);
        s.key(key(KeyCode::Enter));
        assert_eq!(s.counter(), "1/4");
        s.key(key(KeyCode::Char('n')));
        assert_eq!(s.query, "n", "typing has ended, so the letter no longer edits the query");
        assert_eq!(s.counter(), "1/4", "and it does not step either - only Enter does");
        s.key(key(KeyCode::Enter));
        assert_eq!(s.counter(), "2/4");
    }

    #[test]
    fn scroll_target_brings_a_history_match_into_view() {
        let mut t = term_with(3, &["top", "junk1", "junk2", "junk3", "junk4", "junk5"]);
        let mut s = Search::new();
        typed(&mut s, "top");
        s.run(&mut t);
        let m = s.current().expect("there is a match");
        assert!(m.row < 0, "\"top\" should have scrolled off into history");

        let target = s.scroll_target().expect("there is a target");
        let d = t.scroll as isize - target as isize;
        t.scroll_by(d);
        assert_eq!(t.scroll, target, "scroll landed on the target");

        let y = (m.row + target as isize) as u16;
        let line: String = (0..3u16)
            .map(|c| t.screen().cell(y, c).map_or(' ', |c| c.contents().chars().next().unwrap_or(' ')))
            .collect();
        assert_eq!(line, "top");
    }

    #[test]
    fn scroll_target_is_zero_for_a_match_on_the_live_screen() {
        let mut t = term_with(5, &["hello"]);
        let mut s = Search::new();
        typed(&mut s, "hello");
        s.run(&mut t);
        assert_eq!(s.scroll_target(), Some(0));
    }

    #[test]
    fn esc_clears_the_query_then_closes() {
        let mut t = term_with(5, &["abc"]);
        let mut s = Search::new();
        typed(&mut s, "abc");
        s.run(&mut t);
        assert!(changed(s.key(key(KeyCode::Esc))), "first Esc clears it");
        assert_eq!(s.query, "");
        assert_eq!(s.counter(), "0/0");
        assert!(matches!(s.key(key(KeyCode::Esc)), SearchOutcome::Close));
    }

    #[test]
    fn empty_query_matches_nothing() {
        let mut t = term_with(5, &["anything"]);
        let mut s = Search::new();
        s.run(&mut t);
        assert_eq!(s.counter(), "0/0");
        assert_eq!(s.current(), None);
        assert_eq!(s.scroll_target(), None);
    }

    #[test]
    fn bar_shows_the_query_and_the_counter() {
        let mut t = term_with(5, &["aa", "aa"]);
        let mut s = Search::new();
        typed(&mut s, "aa");
        s.run(&mut t);
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        s.draw_bar(&mut buf, area, &Theme::dark());
        let line: String = (0..20).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(line.trim_end(), "/aa              1/2");
    }

    #[test]
    fn highlight_paints_only_what_the_offset_shows() {
        let mut t = term_with(3, &["aa", "b", "c", "d", "aa"]);
        let mut s = Search::new();
        typed(&mut s, "aa");
        s.run(&mut t);
        let theme = Theme::dark();
        let inner = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(inner);

        s.highlight(&mut buf, inner, 0, &theme);
        let painted: Vec<u16> = (0..3)
            .filter(|y| buf[(0, *y)].bg != ratatui::style::Color::Reset)
            .collect();
        assert_eq!(painted.len(), 1, "exactly one match on the live screen");

        let target = s.scroll_target().unwrap();
        let mut buf = Buffer::empty(inner);
        s.highlight(&mut buf, inner, target, &theme);
        assert_eq!(buf[(0, 0)].bg, theme.warn, "the current one is yellow");
        assert_eq!(buf[(1, 0)].bg, theme.warn, "both characters");
    }

    #[test]
    fn ten_thousand_lines_search_in_well_under_a_second() {
        let mut t = Term::new(80, 24, 10_000);
        for i in 0..10_000 {
            t.feed(format!("line {i} some filler text here\r\n").as_bytes());
        }
        let mut s = Search::new();
        typed(&mut s, "filler");
        let started = std::time::Instant::now();
        s.run(&mut t);
        let took = started.elapsed();
        assert!(s.matches.len() > 9_000, "found {}", s.matches.len());
        assert!(took < std::time::Duration::from_secs(1), "search took {took:?}");
        eprintln!("10k lines: {took:?}, matches {}", s.matches.len());
    }
}
