//! Selecting text with the mouse and copying it to the clipboard.

use crate::layout::PaneId;

/// Two corners of a selection, in whatever coordinates the area that made it uses.
pub type Span = ((u16, u16), (u16, u16));

/// The corners in reading order.
pub fn ordered(span: Span) -> Span {
    if span.0 <= span.1 { span } else { (span.1, span.0) }
}

/// Whether a cell falls inside the span. Tuple order is reading order, so there is no
/// first/middle/last row to special-case.
pub fn in_span(span: Span, row: u16, col: u16) -> bool {
    let (a, b) = ordered(span);
    (row, col) >= a && (row, col) <= b
}

/// A selection in pane coordinates (cells within the inner rect), inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub pane: PaneId,
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Selection {
    /// Normalized bounds: (row, col) of the start is <= the end in reading order.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        // Tuple comparison gives reading order for free, no first/middle/last special-casing.
        let (a, b) = self.ordered();
        (row, col) >= a && (row, col) <= b
    }

    /// The selection's text from the screen: rows are joined with `\n`,
    /// trailing spaces are trimmed.
    pub fn text(&self, screen: crate::term::Screen<'_>) -> String {
        let ((r0, c0), (r1, c1)) = self.ordered();
        let (rows, cols) = screen.size();
        if rows == 0 || cols == 0 {
            return String::new();
        }
        let last_col = cols - 1;
        let mut out = String::new();
        for row in r0..=r1.min(rows - 1) {
            if row != r0 {
                out.push('\n');
            }
            let mut from = if row == r0 { c0 } else { 0 };
            // A wide character's contents live in its first cell, not this continuation cell.
            if from > 0 && screen.cell(row, from).is_some_and(|c| c.is_wide_continuation()) {
                from -= 1;
            }
            let to = if row == r1 { c1.min(last_col) } else { last_col };
            let mut line = String::new();
            for col in from..=to {
                match screen.cell(row, col) {
                    // Already taken from the first cell; the continuation cell contributes nothing.
                    Some(c) if c.is_wide_continuation() => {}
                    Some(c) if c.has_contents() => line.push_str(c.contents()),
                    _ => line.push(' '),
                }
            }
            out.push_str(line.trim_end());
        }
        out
    }
}

/// Word boundaries around a cell, including path characters so a double-click takes a whole path.
pub fn word_at(screen: crate::term::Screen<'_>, row: u16, col: u16) -> Option<((u16, u16), (u16, u16))> {
    let (rows, cols) = screen.size();
    if row >= rows || col >= cols {
        return None;
    }
    let word = |c: u16| {
        screen.cell(row, c).is_some_and(|cell| {
            let s = cell.contents();
            !s.is_empty() && s.chars().all(is_word_char)
        })
    };
    if !word(col) {
        return None;
    }
    let mut l = col;
    while l > 0 && word(l - 1) {
        l -= 1;
    }
    let mut r = col;
    while r + 1 < cols && word(r + 1) {
        r += 1;
    }
    Some(((row, l), (row, r)))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~')
}

/// Line boundaries: column zero to the last non-empty character; trailing spaces are grid padding, not text.
pub fn line_at(screen: crate::term::Screen<'_>, row: u16) -> ((u16, u16), (u16, u16)) {
    let (_, cols) = screen.size();
    let mut end = 0;
    for col in 0..cols {
        if screen
            .cell(row, col)
            .is_some_and(|c| c.has_contents() && c.contents() != " ")
        {
            end = col;
        }
    }
    ((row, 0), (row, end))
}

/// Copies to the system clipboard via `pbcopy` and returns an OSC 52 write (`52;c;<base64>`) for the client's terminal; never a read, so it can't be used to exfiltrate the terminal's buffer.
pub fn copy(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    crate::log::info(&format!("copied {} characters", text.chars().count()));
    // a test run must not replace whatever the human has on the clipboard
    #[cfg(all(target_os = "macos", not(test)))]
    if let Ok(mut child) = crate::spawn::cmd("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes()
}

// ceiling: hand-rolled base64 (encode-only) instead of a dependency.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_of(cols: u16, rows: u16, bytes: &[u8]) -> crate::term::Term {
        let mut p = crate::term::Term::new(cols, rows, 0);
        p.feed(bytes);
        p
    }

    fn sel(start: (u16, u16), end: (u16, u16)) -> Selection {
        Selection { pane: 1, start, end }
    }

    #[test]
    fn ordered_swaps_a_backwards_selection() {
        let back = sel((3, 7), (1, 2));
        assert_eq!(back.ordered(), ((1, 2), (3, 7)));
        assert_eq!(sel((0, 9), (0, 4)).ordered(), ((0, 4), (0, 9)));
    }

    #[test]
    fn contains_covers_whole_middle_lines() {
        let s = sel((1, 5), (3, 2));
        assert!(s.contains(1, 5));
        assert!(!s.contains(1, 4));
        assert!(s.contains(2, 0));
        assert!(s.contains(2, 200));
        assert!(s.contains(3, 2));
        assert!(!s.contains(3, 3));
        assert!(!s.contains(4, 0));

        let one = sel((2, 3), (2, 6));
        assert!(one.contains(2, 5));
        assert!(!one.contains(2, 7));
        assert!(!one.contains(1, 5));
    }

    #[test]
    fn text_joins_lines_and_trims_the_tails() {
        let p = screen_of(20, 3, b"hello world\r\nsecond line");
        let s = sel((0, 0), (1, 19));
        assert_eq!(s.text(p.screen()), "hello world\nsecond line");

        assert_eq!(sel((0, 6), (0, 10)).text(p.screen()), "world");

        assert_eq!(sel((1, 19), (0, 0)).text(p.screen()), "hello world\nsecond line");
    }

    #[test]
    fn word_at_takes_the_whole_identifier() {
        let p = screen_of(30, 1, b"  hello_world  ");
        assert_eq!(word_at(p.screen(), 0, 5), Some(((0, 2), (0, 12))));
        assert_eq!(word_at(p.screen(), 0, 2), Some(((0, 2), (0, 12))));
        assert_eq!(word_at(p.screen(), 0, 12), Some(((0, 2), (0, 12))));
        assert_eq!(word_at(p.screen(), 0, 1), None);
        assert_eq!(word_at(p.screen(), 0, 13), None);
    }

    #[test]
    fn word_at_keeps_paths_together() {
        let p = screen_of(40, 1, b"see ~/src/mouse.rs:12 here");
        let (start, end) = word_at(p.screen(), 0, 8).expect("a path is one word");
        let s = Selection { pane: 1, start, end };
        assert_eq!(s.text(p.screen()), "~/src/mouse.rs");
    }

    #[test]
    fn line_at_stops_at_the_last_visible_char() {
        let p = screen_of(20, 2, b"  hi there   \r\n");
        assert_eq!(line_at(p.screen(), 0), ((0, 0), (0, 9)));
        let s = Selection {
            pane: 1,
            start: (0, 0),
            end: (0, 9),
        };
        assert_eq!(s.text(p.screen()), "  hi there");
        assert_eq!(line_at(p.screen(), 1), ((1, 0), (1, 0)));
    }

    #[test]
    fn base64_matches_the_known_answers() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
