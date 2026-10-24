//! Tab bar and git branches, drawn directly into `Buffer` for per-cell control ratatui widgets don't give.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear, Widget as _};

use crate::render::Theme;

#[derive(Default)]
pub struct TabItem {
    pub index: usize,
    pub name: String,
    pub active: bool,
    /// After the name: one per plugin.
    pub badges: Vec<(String, Color)>,
}

/// Padding on each side of a chip's label.
const CHIP_PAD: u16 = 2;
/// Sets a tab's badges off from its name; plain spaces join badges to each other.
const BADGE_SEP: &str = " \u{b7} ";
/// Dark fill painted around a dialog so its border never lands on a pane's own lines. One cell on
/// every side: a row is the thinnest band a terminal has, and matching it across keeps the ring light.
const MOAT: Margin = Margin { horizontal: 1, vertical: 1 };

/// Which sides of the ring to paint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Moat {
    /// A dialog floating over panes: every side.
    All,
    /// A popup hanging from a button: every side but the one facing it. That side is nerd's own
    /// chrome rather than a pane, and a moat there would paint over the control that opened it.
    NotAbove,
}
/// Border plus one cell, so a dialog's text never touches its frame. Both the drawing and the
/// sizing derive their widths from this: disagree by a cell and a wrapped body overflows its box.
pub const DIALOG_FRAME: Margin = Margin { horizontal: 2, vertical: 1 };
/// A right one-eighth block in the ground colour on a segment's last cell: a hairline gap of a pixel or two
/// between joined chips, the finest step a terminal cell allows.
const SEAM: &str = "\u{2595}";
/// The frame every floating dialog shares: the moat, the border, an optional title in the top
/// line, and the padded rect to draw into. `r` is the dialog; the moat is painted outside it.
pub fn dialog(buf: &mut Buffer, r: Rect, title: Option<&str>, border: Color, theme: &Theme, moat: Moat) -> Rect {
    let above = if moat == Moat::All { MOAT.vertical } else { 0 };
    let x0 = r.x.saturating_sub(MOAT.horizontal);
    let y0 = r.y.saturating_sub(above);
    let x1 = r.right().saturating_add(MOAT.horizontal);
    let y1 = r.bottom().saturating_add(MOAT.vertical);
    let moat = Rect::new(x0, y0, x1 - x0, y1 - y0).intersection(buf.area);
    // Clear first: without it the pane beneath shows through every cell we do not write.
    Clear.render(moat, buf);
    buf.set_style(moat, Style::default().bg(theme.ground).fg(theme.ground));

    Block::bordered().style(Style::default().bg(theme.ground).fg(border)).render(r, buf);

    if let Some(t) = title
        && r.width > 5
    {
        let room = r.width as usize - 5;
        let cap: String = " ".chars().chain(t.chars()).chain(" ".chars()).take(room + 2).collect();
        // The title wears the frame's own colour: on a dialog that takes something away the
        // frame is red, and a green title over it would argue with it.
        buf.set_stringn(r.x + 2, r.y, &cap, room + 2, Style::default().fg(border));
    }
    r.inner(DIALOG_FRAME)
}

/// Each window type's width, frame included (docs/menus.md): a one-line field, most windows, the wide tables.
pub const FIELD_W: u16 = 48;
pub const WINDOW_W: u16 = 68;
pub const WIDE_W: u16 = 80;

/// The most a window may take: the screen less 1/16 at each side and 1/10 above and below.
pub fn limits(screen: Rect) -> (u16, u16) {
    (screen.width - screen.width / 16 * 2, screen.height - screen.height / 10 * 2)
}

/// The window, centred, `w` wide with its frame and tall enough for `content_h` rows inside it, the blank row and
/// the close button under them: at most `limits`.
pub fn fit(screen: Rect, w: u16, content_h: u16) -> Rect {
    let (max_w, max_h) = limits(screen);
    let w = w.min(max_w);
    let h = content_h.saturating_add(2 * DIALOG_FRAME.vertical + 2).min(max_h);
    Rect::new(screen.x + (screen.width - w) / 2, screen.y + (screen.height - h) / 2, w, h)
}

/// A key strip broken between keys into lines of at most `width`; a key with no label is a second code for the one before.
pub fn hint<'a>(keys: impl IntoIterator<Item = (&'a str, &'a str)>, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (label, word) in keys.into_iter().filter(|k| !k.0.is_empty()) {
        let item = if word.is_empty() { label.to_string() } else { format!("{label} {word}") };
        match lines.last_mut() {
            Some(l) if l.chars().count() + 3 + item.chars().count() <= width => *l = format!("{l} \u{b7} {item}"),
            _ => lines.push(item),
        }
    }
    lines
}

/// Draws a key strip on the last rows of `inner`; returns the row it starts on.
pub fn draw_hint(buf: &mut Buffer, inner: Rect, lines: &[String], theme: &Theme) -> u16 {
    let top = inner.bottom().saturating_sub(lines.len() as u16);
    for (k, line) in lines.iter().enumerate() {
        let y = top + k as u16;
        if y < inner.bottom() {
            buf.set_stringn(inner.x, y, line, inner.width as usize, Style::default().fg(theme.faint));
        }
    }
    top
}

pub const CLOSE: &str = " [Esc] close ";

/// Where the close button sits in dialog `r`: the last row inside the frame, centred.
pub fn close_rect(r: Rect) -> Rect {
    button_rect(r, CLOSE)
}

/// Where a button labelled `label` sits in the close button's place.
pub fn button_rect(r: Rect, label: &str) -> Rect {
    let inner = r.inner(DIALOG_FRAME);
    let len = (label.chars().count() as u16).min(inner.width);
    Rect::new(inner.x + (inner.width - len) / 2, inner.bottom().saturating_sub(1), len, 1)
}

/// Draws the close button of dialog `r`; returns what is left of its inside above the button and a blank row.
pub fn close_button(buf: &mut Buffer, r: Rect, theme: &Theme) -> Rect {
    button(buf, r, CLOSE, theme)
}

/// The close button with another label, as `close_button`.
pub fn button(buf: &mut Buffer, r: Rect, label: &str, theme: &Theme) -> Rect {
    let b = button_rect(r, label);
    buf.set_stringn(b.x, b.y, label, b.width as usize, Style::default().bg(theme.ground).fg(theme.text));
    let inner = r.inner(DIALOG_FRAME);
    Rect { height: inner.height.saturating_sub(2), ..inner }
}

/// A window's content taller than its room (docs/menus.md): the first line shown, and the lines shown and all of
/// them as the last frame drew them, which the keys and the wheel go by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scroll {
    from: std::cell::Cell<usize>,
    shown: std::cell::Cell<usize>,
    of: std::cell::Cell<usize>,
}

impl Scroll {
    pub fn get(&self) -> usize {
        self.from.get()
    }

    pub fn set(&self, from: usize) {
        self.from.set(from);
    }

    /// The first of `of` lines to draw in `shown` rows.
    pub fn place(&self, shown: usize, of: usize) -> usize {
        self.shown.set(shown);
        self.of.set(of);
        self.from.set(self.from.get().min(of.saturating_sub(shown)));
        self.from.get()
    }

    /// As `place`, with row `at` of a list in view.
    pub fn follow(&self, at: usize, shown: usize, of: usize) -> usize {
        self.from.set(self.from.get().min(at).max((at + 1).saturating_sub(shown)));
        self.place(shown, of)
    }

    pub fn page(&self) -> usize {
        self.shown.get().max(1)
    }

    /// The last frame left lines out.
    pub fn hides(&self) -> bool {
        self.of.get() > self.shown.get()
    }

    fn by(&self, n: isize) {
        let last = self.of.get().saturating_sub(self.shown.get());
        self.from.set(self.from.get().saturating_add_signed(n).min(last));
    }

    /// Up and Down a line, Page Up and Page Down a page, Home and End to the ends; false for any other key.
    pub fn key(&self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode::*;
        let page = self.page() as isize;
        match code {
            Up => self.by(-1),
            Down => self.by(1),
            PageUp => self.by(-page),
            PageDown => self.by(page),
            Home => self.from.set(0),
            End => self.by(isize::MAX),
            _ => return false,
        }
        true
    }

    /// The wheel, three lines a notch; false for any other event.
    pub fn wheel(&self, kind: crossterm::event::MouseEventKind) -> bool {
        use crossterm::event::MouseEventKind::*;
        match kind {
            ScrollUp => self.by(-3),
            ScrollDown => self.by(3),
            _ => return false,
        }
        true
    }

    /// The row Page Up, Page Down, Home or End chooses in a list of `n` from row `at`; None for any other key.
    pub fn pick(&self, code: crossterm::event::KeyCode, at: usize, n: usize) -> Option<usize> {
        use crossterm::event::KeyCode::*;
        let last = n.checked_sub(1)?;
        match code {
            PageUp => Some(at.saturating_sub(self.page())),
            PageDown => Some((at + self.page()).min(last)),
            Home => Some(0),
            End => Some(last),
            _ => None,
        }
    }

    /// Row `at` of a list, moved into the lines shown.
    pub fn within(&self, at: usize) -> usize {
        let from = self.from.get();
        at.clamp(from, from + self.shown.get().max(1) - 1)
    }

    /// `<word> N–M of K` while anything is hidden.
    pub fn hidden(&self, word: &str) -> Option<String> {
        let (from, shown, of) = (self.from.get(), self.shown.get(), self.of.get());
        (of > shown && shown > 0).then(|| format!("{word} {}–{} of {of}", from + 1, from + shown))
    }

    /// While anything is hidden, `lines N–M of K` on row `y` of window `r`: the row over its buttons.
    pub fn draw(&self, buf: &mut Buffer, r: Rect, y: u16, theme: &Theme) {
        if let Some(at) = self.hidden("lines") {
            say(buf, r, y, &at, theme);
        }
    }
}

/// What a window hides, on row `y` of window `r`.
pub fn say(buf: &mut Buffer, r: Rect, y: u16, text: &str, theme: &Theme) {
    let inner = r.inner(DIALOG_FRAME);
    if y >= inner.y && y < inner.bottom() {
        buf.set_stringn(inner.x, y, text, inner.width as usize, Style::default().bg(theme.ground).fg(theme.faint));
    }
}

/// What scrolls a window taller than the screen, as Keys and mouse lists it.
pub const SCROLL_KEYS: &[(&str, &str)] = &[
    ("wheel", "three lines"),
    ("↑↓", "a line; in a list, the selection, and the list follows it"),
    ("PgUp PgDn", "a page"),
    ("Home End", "the start and the end"),
];

/// A button of the top bar: a filled shape with padding, one cell high. The active tab is the tab
/// bar's cursor: filled with the accent while the keys are on the bar, its name in the accent while they are elsewhere.
#[allow(clippy::too_many_arguments)]
pub fn draw_chip(buf: &mut Buffer, x: u16, y: u16, label: &str, badges: &[(String, Color)], active: bool, keys: bool, theme: &Theme) -> Rect {
    let pad = " ".repeat(CHIP_PAD as usize);
    let tail: String = badges.iter().enumerate().map(|(i, (t, _))| format!("{}{t}", badge_sep(i))).collect();
    let text = format!("{pad}{label}{tail}{pad}");
    let style = if active && keys {
        theme.cursor().add_modifier(Modifier::BOLD)
    } else if active {
        theme.chosen().bg(theme.surface)
    } else {
        Style::new().fg(theme.muted).bg(theme.surface)
    };
    buf.set_string(x, y, &text, style);
    let mut bx = x + CHIP_PAD + cells(label);
    for (i, (t, c)) in badges.iter().enumerate() {
        let sep = cells(badge_sep(i));
        buf.set_string(bx + sep, y, t, style.fg(*c));
        bx += sep + cells(t);
    }
    Rect::new(x, y, cells(&text), 1)
}

/// The name-to-badge separator before the first badge; a plain space joins the rest.
fn badge_sep(i: usize) -> &'static str {
    if i == 0 { BADGE_SEP } else { " " }
}

fn cells(s: &str) -> u16 {
    crate::slots::cells(s) as u16
}

/// Width a chip with this label and these badges takes, in cells.
pub fn chip_width(label: &str, badges: &[(String, Color)]) -> u16 {
    cells(label) + badges.iter().enumerate().map(|(i, (t, _))| cells(badge_sep(i)) + cells(t)).sum::<u16>() + 2 * CHIP_PAD
}

pub fn draw_tabs(
    buf: &mut Buffer,
    area: Rect,
    tabs: &[TabItem],
    keys: bool,
    theme: &Theme,
) -> (Vec<(Rect, usize)>, Option<Rect>) {
    if area.is_empty() {
        return (Vec::new(), None);
    }
    let right = area.right();
    // Badges drop before any tab does.
    fn shown(t: &TabItem, all: bool) -> &[(String, Color)] {
        if all { &t.badges } else { &[] }
    }
    let all = tabs.iter().map(|t| chip_width(&t.name, &t.badges) as u32).sum::<u32>() + chip_width("+", &[]) as u32 <= area.width as u32;
    let mut rects = Vec::with_capacity(tabs.len());
    let mut x = area.x;
    for tab in tabs {
        // stop rather than truncate a tab that doesn't fit
        if x + chip_width(&tab.name, shown(tab, all)) > right {
            break;
        }
        let rect = draw_chip(buf, x, area.y, &tab.name, shown(tab, all), tab.active, keys, theme);
        rects.push((rect, tab.index));
        x = rect.right();
    }
    let plus = (x + chip_width("+", &[]) <= right).then(|| draw_chip(buf, x, area.y, "+", &[], false, false, theme));
    // side by side, the chips join into one bar, parted by hairline seams
    for (rect, _) in &rects {
        if let Some(cell) = buf.cell_mut((rect.right() - 1, rect.y)) {
            cell.set_symbol(SEAM).set_fg(theme.ground);
        }
    }
    (rects, plus)
}

/// Reads .git/HEAD directly - running git every frame is too expensive. None outside a repository.
pub fn git_branch(dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(dir)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    parse_head(&head)
}

/// Finds `.git` (or its target for a worktree/submodule), walking up since a pane may be in a subfolder.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let content = std::fs::read_to_string(&candidate).ok()?;
            let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = PathBuf::from(gitdir);
            return Some(if gitdir.is_absolute() { gitdir } else { dir.join(gitdir) });
        }
        dir = dir.parent()?;
    }
}

fn parse_head(content: &str) -> Option<String> {
    let content = content.trim();
    if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
        return Some(branch.to_string());
    }
    if content.len() >= 7 && content.is_char_boundary(7) {
        return Some(content[..7].to_string());
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tab(index: usize, name: &str, active: bool) -> TabItem {
        TabItem { index, name: name.into(), active, ..TabItem::default() }
    }

    #[test]
    fn a_popup_does_not_paint_over_the_button_it_hangs_from() {
        let theme = Theme::dark();
        let screen = Rect::new(0, 0, 24, 9);
        let mut buf = Buffer::empty(screen);
        // the button the menu hangs from, on the row above it
        buf[(9, 2)].set_symbol("\u{2261}").set_fg(theme.muted);

        let r = Rect::new(8, 3, 10, 4);
        dialog(&mut buf, r, None, theme.border, &theme, Moat::NotAbove);
        assert_eq!(buf[(9, 2)].symbol(), "\u{2261}", "the button survives the popup that opened it");

        assert_eq!(buf[(r.x - 1, r.y + 1)].bg, theme.ground, "the sides keep their moat");
        assert_eq!(buf[(r.right(), r.y + 1)].bg, theme.ground);
        assert_eq!(buf[(r.x, r.bottom())].bg, theme.ground, "and so does the bottom");

        let mut buf = Buffer::empty(screen);
        buf[(9, 2)].set_symbol("\u{2261}").set_fg(theme.muted);
        dialog(&mut buf, r, None, theme.border, &theme, Moat::All);
        assert_eq!(buf[(9, 2)].symbol(), " ", "a floating dialog still covers what is above it");
    }

    #[test]
    fn a_dialog_sits_in_a_moat_and_pads_both_sides_alike() {
        let theme = Theme::dark();
        let screen = Rect::new(0, 0, 24, 9);
        let mut buf = Buffer::empty(screen);
        // something for the moat to cover: a pane's own line running across the whole row
        for x in 0..screen.width {
            buf[(x, 2)].set_symbol("─").set_fg(theme.border);
        }

        let r = Rect::new(8, 3, 10, 3);
        let inner = dialog(&mut buf, r, Some("Hi"), theme.border_focus, &theme, Moat::All);

        assert_eq!(inner, Rect::new(10, 4, 6, 1), "border and one cell of padding on every side");
        assert_eq!(inner.x - r.x, r.right() - inner.right(), "the same padding left and right");

        for x in r.x - MOAT.horizontal..r.right() + MOAT.horizontal {
            assert_eq!(buf[(x, r.y - MOAT.vertical)].bg, theme.ground, "the moat covers the row above");
            assert_eq!(buf[(x, r.y - MOAT.vertical)].symbol(), " ", "and nothing of the line survives it");
        }
        assert_eq!(buf[(r.x - 1, r.y + 1)].bg, theme.ground, "moat beside the left border");
        assert_eq!(buf[(r.right(), r.y + 1)].bg, theme.ground, "and beside the right");

        assert_eq!(buf[(r.x, r.y)].symbol(), "┌");
        assert_eq!(buf[(r.x + 3, r.y)].symbol(), "H", "the title starts two cells in");
        assert_eq!(buf[(0, 2)].symbol(), "─", "the pane's line is untouched outside the moat");
    }

    #[test]
    fn tabs_join_into_one_bar_of_padded_segments() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let (rects, plus) = draw_tabs(&mut buf, area, &[tab(0, "main", true), tab(1, "logo", false)], false, &theme);
        assert_eq!(rects.iter().map(|(r, _)| (r.x, r.width)).collect::<Vec<_>>(), vec![(0, 8), (8, 8)], "two cells of padding, no gap");
        let line: String = (0..22).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(line, "  main ▕  logo ▕  +   ", "the seam takes the last padding cell");
        assert_eq!((buf[(7, 0)].fg, buf[(15, 0)].fg), (theme.ground, theme.ground), "hairline seams in the ground colour");
        assert_eq!(plus, Some(Rect::new(16, 0, 5, 1)));

        // one filled bar from the first tab to +, the current one included
        assert!((0..21).all(|x| buf[(x, 0)].bg == theme.surface));
        assert_ne!(buf[(21, 0)].bg, theme.surface, "the bar ends after +");
    }

    #[test]
    fn the_active_tab_is_the_bars_cursor_and_the_others_stay_quiet() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 40, 1);
        for (keys, want) in [(true, theme.cursor()), (false, theme.chosen().bg(theme.surface))] {
            let mut buf = Buffer::empty(area);
            draw_tabs(&mut buf, area, &[tab(0, "main", true), tab(1, "logo", false)], keys, &theme);
            let (active, quiet) = (&buf[(2, 0)], &buf[(11, 0)]);
            assert_eq!((active.fg, active.bg), (want.fg.unwrap(), want.bg.unwrap()), "keys on the bar: {keys}");
            assert!(active.modifier.contains(Modifier::BOLD), "the active tab is chosen, so it is bold");
            assert_eq!(buf[(0, 0)].bg, active.bg, "the fill takes the padding too");
            assert_eq!((quiet.fg, quiet.bg), (theme.muted, theme.surface));
        }
    }

    #[test]
    fn a_numeric_tab_name_with_a_todo_badge_does_not_read_as_two_tabs() {
        let theme = Theme::dark();
        let one = TabItem { index: 0, name: "1".into(), active: true, badges: vec![("12 todo".into(), theme.teal)] };
        let chip = chip_width(&one.name, &one.badges);
        let area = Rect::new(0, 0, chip + chip_width("+", &[]), 1);
        let mut buf = Buffer::empty(area);
        draw_tabs(&mut buf, area, &[one], false, &theme);
        let line: String = (0..chip).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(line, "  1 \u{b7} 12 todo \u{2595}", "the separator and the unit word keep it one tab, not \"1\" and \"12\"");
    }

    #[test]
    fn a_longer_badge_still_drops_before_any_tab_does() {
        let theme = Theme::dark();
        // a plugin's count at the 8-cell cap
        let tabs = || {
            vec![
                TabItem { index: 0, name: "main".into(), active: true, badges: vec![("999 todo".into(), theme.teal)] },
                TabItem { index: 1, name: "logs".into(), active: false, ..TabItem::default() },
            ]
        };
        let all = tabs().iter().map(|t| chip_width(&t.name, &t.badges)).sum::<u16>() + chip_width("+", &[]);
        assert!(chip_width("main", &[]) + chip_width("logs", &[]) + chip_width("+", &[]) < all, "dropping the plugin's badge frees cells");

        let area = Rect::new(0, 0, all, 1);
        let mut buf = Buffer::empty(area);
        let (rects, _) = draw_tabs(&mut buf, area, &tabs(), false, &theme);
        assert_eq!(rects.len(), 2);
        let row: String = (0..all).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(row.contains("main \u{b7} 999 todo"), "the badge fits: {row}");

        // one cell too narrow for the plugin's badge: it drops before either tab does
        let area = Rect::new(0, 0, all - 1, 1);
        let mut buf = Buffer::empty(area);
        let (rects, _) = draw_tabs(&mut buf, area, &tabs(), false, &theme);
        assert_eq!(rects.len(), 2, "both tabs still show");
        let row: String = (0..all - 1).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(row.contains("main") && !row.contains("todo"), "the plugin's badge is gone: {row}");
    }

    #[test]
    fn a_tab_that_does_not_fit_is_left_out_whole() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 14, 1);
        let mut buf = Buffer::empty(area);
        let (rects, plus) = draw_tabs(&mut buf, area, &[tab(0, "main", true), tab(1, "a long name", false)], false, &theme);
        assert_eq!(rects.len(), 1);
        assert_eq!(plus, Some(Rect::new(8, 0, 5, 1)), "+ still fits after the first");
    }

    #[test]
    fn git_branch_parses_ref_head() {
        let dir = std::env::temp_dir().join(format!("nerd-chrome-test-ref-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/foo\n").unwrap();
        assert_eq!(git_branch(&dir), Some("feature/foo".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_branch_parses_detached_head() {
        let dir = std::env::temp_dir().join(format!("nerd-chrome-test-detached-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "3f5a1e9c2b7d4a6f8e0d1c2b3a4f5e6d7c8b9a0f\n").unwrap();
        assert_eq!(git_branch(&dir), Some("3f5a1e9".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_branch_none_outside_repo() {
        let dir = std::env::temp_dir().join(format!("nerd-chrome-test-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(git_branch(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_branch_resolves_from_subdirectory() {
        let dir = std::env::temp_dir().join(format!("nerd-chrome-test-sub-{}", std::process::id()));
        let sub = dir.join("src/inner");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(git_branch(&sub), Some("main".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }


    #[test]
    fn git_branch_follows_worktree_gitfile() {
        let dir = std::env::temp_dir().join(format!("nerd-chrome-test-wt-{}", std::process::id()));
        let real_git = dir.join("real-git");
        std::fs::create_dir_all(&real_git).unwrap();
        std::fs::write(real_git.join("HEAD"), "ref: refs/heads/wt-branch\n").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".git"), format!("gitdir: {}\n", real_git.display())).unwrap();
        assert_eq!(git_branch(&dir), Some("wt-branch".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The window drawn in `buf`: from its top-left corner to its bottom-right one, and the text of its top row.
    fn frame(buf: &Buffer) -> (Rect, String) {
        let cells = |sym: &str| (0..buf.area.height).flat_map(|y| (0..buf.area.width).map(move |x| (x, y))).filter(|&p| buf[p].symbol() == sym).collect::<Vec<_>>();
        let (tl, br) = (cells("\u{250c}")[0], *cells("\u{2518}").last().unwrap());
        let top = (tl.0..=br.0).map(|x| buf[(x, tl.1)].symbol()).collect();
        (Rect::new(tl.0, tl.1, br.0 - tl.0 + 1, br.1 - tl.1 + 1), top)
    }

    #[test]
    fn a_window_fits_its_content_within_the_screen_margins() {
        let big = Rect::new(0, 0, 200, 60);
        let r = fit(big, 54, 10);
        assert_eq!((r.width, r.height), (54, 14), "its width; the content, the frame, the blank row and the close button");
        assert_eq!((r.x, r.y), ((200 - 54) / 2, (60 - 14) / 2), "centred");
        let r = fit(Rect::new(0, 0, 80, 30), 500, 500);
        assert_eq!((r.width, r.height, r.x, r.y), (70, 24, 5, 3), "less 1/16 at each side and 1/10 above and below");
    }

    #[test]
    fn the_key_strip_breaks_between_keys() {
        let keys = [("↑↓", ""), ("", ""), ("enter", "open"), ("esc", "close")];
        assert_eq!(hint(keys, 100), ["↑↓ \u{b7} enter open \u{b7} esc close"]);
        assert_eq!(hint(keys, 16), ["↑↓ \u{b7} enter open", "esc close"]);
    }

    #[test]
    fn every_window_has_its_title_in_the_frame_and_fits_its_content() {
        use crate::config::{Config, Prefix};
        use crate::menu::{Confirm, Picker, PickerItem, Prompt};
        use crossterm::event::{KeyCode, KeyEvent};
        let theme = Theme::dark();
        let item = |label: &str| PickerItem { label: label.into(), detail: String::new(), id: 0 };
        let mut picker = Picker::new("Pick", vec![item("a"), item("b"), item(&"c".repeat(50))]);
        let mut prompt = Prompt::new("Rename pane", "x");
        let mut confirm = Confirm::new("Close pane", "Sure?");
        let mut find = crate::find::Find::new();
        let mut keys = crate::keyshelp::KeysHelp::new(Prefix::Off);
        let settings = crate::settings::Settings::new(&Config::default());
        let sessions = crate::clients::Sessions::new(Vec::new(), None);
        let panel = crate::panel::Open::new((true, "p".into()), "id".into(), "Panel".into());
        type Draw<'a> = Box<dyn Fn(&mut Buffer, Rect) + 'a>;
        let draws: Vec<(&str, Draw)> = vec![
            ("Settings", Box::new(|b, s| { settings.draw(b, s, &theme); })),
            ("Keys and mouse", Box::new(|b, s| keys.draw(b, s, &theme))),
            ("Search", Box::new(|b, s| { find.draw(b, s, &theme); })),
            ("Pick", Box::new(|b, s| { picker.draw(b, s, &theme); })),
            ("Rename pane", Box::new(|b, s| prompt.draw(b, s, &theme))),
            ("Close pane", Box::new(|b, s| { confirm.draw(b, s, &theme); })),
            ("Session history", Box::new(|b, s| { sessions.draw(b, s, &theme); })),
            ("Panel", Box::new(|b, s| { panel.draw(b, s, &theme, None); })),
        ];
        for screen in [Rect::new(0, 0, 300, 200), Rect::new(0, 0, 60, 20)] {
            let (max_w, max_h) = limits(screen);
            for (title, draw) in &draws {
                let mut buf = Buffer::empty(screen);
                draw(&mut buf, screen);
                let (r, top) = frame(&buf);
                assert!(top.contains(&format!(" {title} ")), "{title}: the title is in the frame: {top:?}");
                assert!(r.width <= max_w && r.height <= max_h && r.width >= 40.min(max_w), "{title}: {r:?} inside the margins");
                assert_eq!(r.x, (screen.width - r.width) / 2, "{title}: centred");
                // Search keeps its top where its longest list would put it, and grows downward.
                if *title != "Search" {
                    assert_eq!(r.y, (screen.height - r.height) / 2, "{title}: centred");
                }
            }
        }
        let big = Rect::new(0, 0, 300, 200);
        let size = |draw: &dyn Fn(&mut Buffer, Rect)| {
            let mut buf = Buffer::empty(big);
            draw(&mut buf, big);
            let r = frame(&buf).0;
            (r.width, r.height)
        };
        drop(draws);
        assert_eq!(picker.rect(big).width, WINDOW_W, "the window's width, whatever its items");
        let before = picker.rect(big);
        picker.key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(picker.rect(big), before, "typing does not resize a list");
        assert_eq!(size(&|b, s| { picker.draw(b, s, &theme); }), (before.width, before.height), "and it draws what it measured");
        assert_eq!(prompt.rect(big).height, 5);
        prompt.key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(prompt.rect(big).width, 48, "a fixed input line");
        let short = confirm.rect(big).height;
        confirm.body = "word ".repeat(30);
        assert!(confirm.rect(big).height > short, "a longer body makes it taller");
        let r = find.rect(big);
        find.query = "zzz".into();
        let typed = find.rect(big);
        assert!(typed.x == r.x && typed.y == r.y && typed.width == r.width, "typing does not move Search, it only grows down");
        assert_eq!(size(&|b, s| { find.draw(b, s, &theme); }), (typed.width, typed.height), "and it draws what it measured");
        let r = keys.rect(big);
        keys.key(KeyEvent::from(KeyCode::Char('/')));
        keys.key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(keys.rect(big), r, "typing a search does not resize Keys and mouse");
        assert_eq!(size(&|b, s| keys.draw(b, s, &theme)), (r.width, r.height));
    }

    #[test]
    fn each_window_type_has_its_own_width() {
        use crate::menu::{Confirm, Picker, PickerItem, Prompt};
        let theme = Theme::dark();
        let screen = Rect::new(0, 0, 160, 45);
        let width = |draw: &dyn Fn(&mut Buffer)| {
            let mut buf = Buffer::empty(screen);
            draw(&mut buf);
            frame(&buf).0.width
        };
        let item = |label: &str, detail: &str| PickerItem { label: label.into(), detail: detail.into(), id: 0 };
        let picker = Picker::new("Open worktree", vec![item("main", &"/a/long/path".repeat(10))]);
        let confirm = Confirm::new("Close pane", "word ".repeat(40));
        let hook = Confirm::new("Install the hook", "x").table(vec![vec!["Agent".into(), "Settings file".into(), "Resumes with".into()], vec!["Copilot CLI".into(), "~/.config/opencode/plugins/nerd.js".into(), "opencode --session <id>".into()]]);
        let settings = crate::settings::Settings::new(&crate::config::Config::default());
        let keys = crate::keyshelp::KeysHelp::new(crate::config::Prefix::Off);
        let space = crate::space_picker::SpacePicker::new(PathBuf::from("/"));
        let sessions = crate::clients::Sessions::new(Vec::new(), None);
        let panel = crate::panel::Open::new((true, "p".into()), "id".into(), "Panel".into());
        let servers = crate::server_list::tests::fixture(true);
        let plugins = crate::plugin_list::PluginList::new(Vec::new(), Default::default());
        let messages = crate::messages::Messages::new();
        let mail = crate::messages::tests::view();
        let mut held = crate::messages::Messages::at(mail.waiting[0].id.clone());
        assert!(held.settle(&mail));
        let wide_buttons = Confirm::new("Remove plugin", "x").does("remove plugin").other(" [d] remove it and its data ");
        let isolation = crate::isolation_ui::IsolationWin { home: true, ..crate::isolation_ui::tests::grouped() };
        let accounts = crate::isolation_ui::Accounts::for_name("scout");
        let mut providers = crate::isolation_ui::tests::grouped();
        providers.view = Some(crate::isolation_ui::View { agent: "fake".into(), draft: None, scroll: Default::default(), empty: false });
        type Draw<'a> = Box<dyn Fn(&mut Buffer) + 'a>;
        let windows: Vec<(&str, u16, Draw)> = vec![
            ("Prompt", FIELD_W, Box::new(|b| Prompt::new("Rename tab", "x").draw(b, screen, &theme))),
            ("Prompt with a note", FIELD_W, Box::new(|b| Prompt::new("Rename pane", "x").note(&"a note ".repeat(20)).draw(b, screen, &theme))),
            ("Confirm", WINDOW_W, Box::new(|b| { confirm.draw(b, screen, &theme); })),
            ("Confirm with a table", WINDOW_W, Box::new(|b| { hook.draw(b, screen, &theme); })),
            ("Confirm with wide buttons", WINDOW_W, Box::new(|b| { wide_buttons.draw(b, screen, &theme); })),
            ("Search", WINDOW_W, Box::new(|b| { crate::find::Find::new().draw(b, screen, &theme); })),
            ("Settings", WINDOW_W, Box::new(|b| { settings.draw(b, screen, &theme); })),
            ("Keys and mouse", WINDOW_W, Box::new(|b| keys.draw(b, screen, &theme))),
            ("New space", WINDOW_W, Box::new(|b| { space.draw(b, screen, &theme); })),
            ("Picker", WINDOW_W, Box::new(|b| { picker.draw(b, screen, &theme); })),
            ("Session history", WINDOW_W, Box::new(|b| { sessions.draw(b, screen, &theme); })),
            ("Plugin panel", WINDOW_W, Box::new(|b| { panel.draw(b, screen, &theme, None); })),
            ("Servers", WIDE_W, Box::new(|b| { servers.draw(b, screen, &theme); })),
            ("Plugins", WIDE_W, Box::new(|b| { plugins.draw(b, screen, &theme); })),
            ("Messages", WIDE_W, Box::new(|b| { messages.draw(b, screen, &theme, &mail, &crate::messages::tests::verdict); })),
            ("Held message", WIDE_W, Box::new(|b| { held.draw(b, screen, &theme, &mail, &crate::messages::tests::verdict); })),
            ("Choose providers", WIDE_W, Box::new(|b| { providers.draw(b, screen, &theme); })),
            ("Isolation", WIDE_W, Box::new(|b| { isolation.draw(b, screen, &theme); })),
            ("Accounts", WIDE_W, Box::new(|b| { accounts.draw(b, screen, &theme, &[]); })),
        ];
        for (name, want, draw) in &windows {
            assert_eq!(width(draw.as_ref()), *want, "{name}");
        }
        let mut buf = Buffer::empty(screen);
        let row = picker.draw(&mut buf, screen, &theme)[0];
        let text: String = (row.x..row.right()).map(|x| buf[(x, row.y)].symbol()).collect();
        assert!(text.starts_with(" main …") && text.ends_with("/a/long/path"), "a long second column is cut with … and keeps its end: {text:?}");
    }

    /// One window too tall for `screen` through the scrolling rule in docs/menus.md: `draw` draws it, `key` and `wheel`
    /// send it input. `list`: Up and Down move a selection, which leads the scroll.
    pub(crate) fn scrolls(name: &str, screen: Rect, list: bool, draw: &dyn Fn(&mut Buffer), key: &dyn Fn(crossterm::event::KeyCode), wheel: &dyn Fn(crossterm::event::MouseEventKind)) {
        use crossterm::event::{KeyCode::*, MouseEventKind::{ScrollDown, ScrollUp}};
        // the row saying what is hidden and its numbers, and where `[Esc]` is
        let shot = || -> (usize, [usize; 3], (usize, usize), String) {
            let mut buf = Buffer::empty(screen);
            draw(&mut buf);
            let text = crate::app::snapshots::dump(&buf, screen);
            let rows: Vec<&str> = text.lines().take(screen.height as usize).collect();
            let all = rows.join("\n");
            let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains("│ lines ")).unwrap_or_else(|| panic!("{name}: nothing says what is hidden\n{all}"));
            let n: Vec<usize> = row.split("│ lines ").nth(1).unwrap().split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).take(3).map(|s| s.parse().unwrap()).collect();
            let b = rows.iter().rposition(|r| r.contains("[Esc]")).unwrap_or_else(|| panic!("{name}: no close button\n{all}"));
            // right over a button; a help page's `[o] open` and the blank row under it come before its close
            let blank = |r: &&str| r.trim_matches(|c| c == ' ' || c == '│').is_empty();
            assert!(y < b && rows[y + 1].contains("] ") && rows[y + 1..=b].iter().all(|r| r.contains("] ") || blank(r)), "{name}: the row right over the buttons\n{all}");
            (y, [n[0], n[1], n[2]], (b, rows[b].find("[Esc]").unwrap()), all)
        };
        let (y, [first, page, of], esc, all) = shot();
        assert!(first == 1 && page < of, "{name}: the top, and something hidden\n{all}");
        let after = |code| {
            key(code);
            shot()
        };
        let (at, n, keep, all) = after(End);
        assert_eq!((at, n[1], n[2], keep), (y, of, of, esc), "{name}: End shows the last line; the row and the buttons stay\n{all}");
        assert_eq!(after(Home).1[0], 1, "{name}: Home");
        let (_, n, _, all) = after(PageDown);
        if list {
            assert!(n[0] > 1, "{name}: Page Down\n{all}");
        } else {
            assert_eq!(n[0], (page + 1).min(of - page + 1), "{name}: Page Down moves a page\n{all}");
        }
        assert_eq!(after(PageUp).1[0], 1, "{name}: Page Up");
        wheel(ScrollDown);
        let (_, n, keep, all) = shot();
        assert_eq!((n[0], keep), (4, esc), "{name}: three lines a notch, the buttons stay\n{all}");
        wheel(ScrollUp);
        assert_eq!(shot().1[0], 1, "{name}: and back");
        // a list's selection leads: the view moves once it passes the last row shown
        key(Home);
        let steps = if list { page } else { 1 };
        for _ in 0..steps {
            key(Down);
        }
        let (_, n, _, all) = shot();
        // a row of Settings is more than a line under a heading
        assert!(n[0] == 2 || list && n[0] > 2, "{name}: Down\n{all}");
        for _ in 0..steps {
            key(Up);
        }
        assert_eq!(shot().1[0], 1, "{name}: Up");
    }

    /// Nothing says what is hidden when nothing is.
    pub(crate) fn fits(name: &str, screen: Rect, draw: &dyn Fn(&mut Buffer)) {
        let mut buf = Buffer::empty(screen);
        draw(&mut buf);
        let text = crate::app::snapshots::dump(&buf, screen);
        assert!(!text.contains("│ lines "), "{name}\n{text}");
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn wheel(kind: crossterm::event::MouseEventKind, screen: Rect) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent { kind, column: screen.width / 2, row: screen.height / 2, modifiers: crossterm::event::KeyModifiers::NONE }
    }

    /// New space listing the folders of `/`.
    fn root() -> crate::space_picker::SpacePicker {
        let mut n = crate::space_picker::SpacePicker::new(std::path::PathBuf::from("/"));
        for _ in 0..200 {
            n.key(key(crossterm::event::KeyCode::Backspace));
        }
        n.paste("/");
        n
    }

    const SHORT: Rect = Rect { x: 0, y: 0, width: 160, height: 24 };
    const TALL: Rect = Rect { x: 0, y: 0, width: 160, height: 300 };

    #[test]
    fn every_window_too_tall_for_the_screen_scrolls_the_same_way() {
        use crate::menu::{Confirm, Picker, PickerItem};
        use std::cell::RefCell;
        let theme = Theme::dark();
        let s = SHORT;

        let c = RefCell::new(Confirm::new("About", "word ".repeat(300)).info());
        scrolls("Confirm", s, false, &|b| _ = c.borrow().draw(b, s, &theme), &|k| _ = c.borrow_mut().key(key(k)), &|w| _ = c.borrow_mut().mouse(wheel(w, s), s, &[]));
        let table: Vec<Vec<String>> = (0..30).map(|i| vec![format!("agent {i}"), "~/.config/x".into()]).collect();
        let c = RefCell::new(Confirm::new("Install the hook", "x").table(table).does("install"));
        scrolls("Confirm with a table", s, false, &|b| _ = c.borrow().draw(b, s, &theme), &|k| _ = c.borrow_mut().key(key(k)), &|w| _ = c.borrow_mut().mouse(wheel(w, s), s, &[]));

        let items = |n: usize| (0..n).map(|i| PickerItem { label: format!("item {i}"), detail: String::new(), id: i }).collect::<Vec<_>>();
        let p = RefCell::new(Picker::new("Open worktree", items(40)));
        scrolls("Picker", s, true, &|b| _ = p.borrow().draw(b, s, &theme), &|k| _ = p.borrow_mut().key(key(k)), &|w| _ = p.borrow_mut().mouse(wheel(w, s), s, &[]));

        let n = RefCell::new(root());
        scrolls("New space", s, true, &|b| _ = n.borrow().draw(b, s, &theme), &|k| _ = n.borrow_mut().key(key(k)), &|w| _ = n.borrow_mut().mouse(wheel(w, s), s, &[]));

        let cfg = RefCell::new(crate::config::Config::default());
        let st = RefCell::new(crate::settings::Settings::new(&cfg.borrow()));
        scrolls("Settings", s, true, &|b| _ = st.borrow().draw(b, s, &theme), &|k| _ = st.borrow_mut().key(key(k), &mut cfg.borrow_mut()), &|w| _ = st.borrow_mut().mouse(wheel(w, s), s, &mut cfg.borrow_mut(), &[]));

        let h = RefCell::new(crate::keyshelp::KeysHelp::new(crate::config::Prefix::Off));
        scrolls("Keys and mouse", s, false, &|b| h.borrow().draw(b, s, &theme), &|k| _ = h.borrow_mut().key(key(k)), &|w| _ = h.borrow_mut().mouse(wheel(w, s), s));

        let isolation = || crate::isolation_ui::IsolationWin { home: true, accounts: (0..12).map(|i| format!("acct{i}")).collect(), ..crate::isolation_ui::tests::grouped() };
        let i = RefCell::new(isolation());
        scrolls("Isolation", s, false, &|b| _ = i.borrow().draw(b, s, &theme), &|k| _ = i.borrow_mut().key(key(k)), &|w| {
            let d = i.borrow().draw(&mut Buffer::empty(s), s, &theme);
            i.borrow_mut().mouse(wheel(w, s), &d);
        });

        let a = RefCell::new(crate::isolation_ui::Accounts::for_name("scout"));
        scrolls("Accounts", s, false, &|b| _ = a.borrow().draw(b, s, &theme, &[]), &|k| _ = a.borrow_mut().key(key(k), &[]), &|w| {
            let d = a.borrow().draw(&mut Buffer::empty(s), s, &theme, &[]);
            a.borrow_mut().mouse(wheel(w, s), &d);
        });

        let data = crate::panel::Data { kind: crate::panel::Kind::List, title: "Panel".into(), items: (0..40).map(|i| crate::panel::Item { id: format!("{i}"), label: format!("row {i}"), detail: String::new(), role: crate::slots::Role::Text, children: Vec::new(), opens: false }).collect(), lines: Vec::new() };
        let o = RefCell::new(crate::panel::Open::new((true, "p".into()), "id".into(), "Panel".into()));
        scrolls("Plugin panel", s, true, &|b| _ = o.borrow().draw(b, s, &theme, Some(&data)), &|k| _ = o.borrow_mut().key(key(k), s, Some(&data)), &|w| {
            let d = o.borrow().draw(&mut Buffer::empty(s), s, &theme, Some(&data));
            o.borrow_mut().mouse(wheel(w, s), s, Some(&data), &d);
        });

        // the same windows with room for everything
        let t = TALL;
        fits("Confirm", t, &|b| _ = Confirm::new("Close pane", "x").draw(b, t, &theme));
        fits("Picker", t, &|b| _ = Picker::new("Open worktree", items(15)).draw(b, t, &theme));
        fits("Settings", t, &|b| _ = crate::settings::Settings::new(&cfg.borrow()).draw(b, t, &theme));
        fits("Keys and mouse", t, &|b| crate::keyshelp::KeysHelp::new(crate::config::Prefix::Off).draw(b, t, &theme));
        fits("Isolation", t, &|b| _ = isolation().draw(b, t, &theme));
        fits("Accounts", t, &|b| _ = crate::isolation_ui::Accounts::for_name("scout").draw(b, t, &theme, &[]));
        let few = crate::panel::Data { items: data.items[..15].to_vec(), ..data.clone() };
        fits("Plugin panel", t, &|b| _ = crate::panel::Open::new((true, "p".into()), "id".into(), "Panel".into()).draw(b, t, &theme, Some(&few)));
    }

    #[test]
    fn a_picker_draws_its_selection_on_every_screen_height() {
        use crate::menu::{Picker, PickerItem};
        let theme = Theme::dark();
        for h in 8..=40 {
            let screen = Rect::new(0, 0, 160, h);
            let mut p = Picker::new("Open worktree", (0..30).map(|i| PickerItem { label: format!("item {i}"), detail: String::new(), id: i }).collect());
            let mut n = root();
            for code in [crossterm::event::KeyCode::End, crossterm::event::KeyCode::Up, crossterm::event::KeyCode::PageUp, crossterm::event::KeyCode::Down] {
                p.key(key(code));
                n.key(key(code));
                let lit = |rows: Vec<Rect>, buf: &Buffer| rows.is_empty() || rows.iter().any(|r| buf[(r.x, r.y)].bg == theme.cursor().bg.unwrap());
                let mut buf = Buffer::empty(screen);
                assert!(lit(p.draw(&mut buf, screen, &theme), &buf), "Picker at {h} rows after {code:?}: the selection is drawn");
                let mut buf = Buffer::empty(screen);
                assert!(lit(n.draw(&mut buf, screen, &theme), &buf), "New space at {h} rows after {code:?}: the selection is drawn");
            }
        }
    }
}
