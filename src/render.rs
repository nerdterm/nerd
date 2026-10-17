//! Copies a terminal grid into the ratatui buffer, and joins adjacent pane borders into a grid.

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::config::BorderMode;

/// Palette. nerd owns every color on screen, pane output's default and named colors included.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub ground: Color,
    pub surface: Color,
    /// The cursor's row while the keys are elsewhere: 3:1 against `ground`, `text` 4.5:1 on it.
    pub slab: Color,
    pub border: Color,
    pub border_focus: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    /// The cursor and selections are filled with this, `on_fill` on it.
    pub fill: Color,
    pub on_fill: Color,
    pub ok: Color,
    pub warn: Color,
    pub bad: Color,
    /// An agent waits for you: your turn, not a problem, so not `bad`.
    pub wait: Color,
    /// Teal - the "done" state.
    pub teal: Color,
    /// The 16 named colors, for pane output that asks for them by number.
    pub ansi: [Color; 16],
}

impl Theme {
    /// nerd, dark: every text tone passes WCAG 2.2 against `ground`.
    pub fn dark() -> Self {
        Self {
            ground: Color::Rgb(0x13, 0x14, 0x17),
            surface: Color::Rgb(0x1b, 0x1d, 0x21),
            slab: Color::Rgb(0x60, 0x62, 0x66),
            border: Color::Rgb(0x2f, 0x32, 0x38),
            border_focus: Color::Rgb(0x9f, 0xd0, 0x8a),
            text: Color::Rgb(0xdd, 0xe0, 0xe5),
            muted: Color::Rgb(0x9b, 0xa0, 0xa8),
            faint: Color::Rgb(0x74, 0x79, 0x7f),
            accent: Color::Rgb(0x9f, 0xd0, 0x8a),
            fill: Color::Rgb(0x9f, 0xd0, 0x8a),
            on_fill: Color::Rgb(0x13, 0x14, 0x17),
            // A green of its own, not the accent's: the accent says where the keys are, and a
            // state that borrows it says that twice with one color.
            ok: Color::Rgb(0x6e, 0xd6, 0x9b),
            warn: Color::Rgb(0xe6, 0xc1, 0x78),
            bad: Color::Rgb(0xef, 0x82, 0x88),
            wait: Color::Rgb(0x8a, 0xb4, 0xf0),
            teal: Color::Rgb(0x6f, 0xc9, 0xd4),
            ansi: [
                Color::Rgb(0x21, 0x23, 0x28),
                Color::Rgb(0xef, 0x82, 0x88),
                Color::Rgb(0x9f, 0xd0, 0x8a),
                Color::Rgb(0xe6, 0xc1, 0x78),
                Color::Rgb(0x8a, 0xb4, 0xf0),
                Color::Rgb(0xc4, 0xa5, 0xee),
                Color::Rgb(0x6f, 0xc9, 0xd4),
                Color::Rgb(0xc6, 0xc9, 0xcf),
                Color::Rgb(0x47, 0x4b, 0x52),
                Color::Rgb(0xf5, 0x9a, 0xa0),
                Color::Rgb(0xb8, 0xde, 0xa5),
                Color::Rgb(0xef, 0xd1, 0x93),
                Color::Rgb(0xa9, 0xc8, 0xf6),
                Color::Rgb(0xd6, 0xbe, 0xf5),
                Color::Rgb(0x8c, 0xd8, 0xe1),
                Color::Rgb(0xec, 0xee, 0xf2),
            ],
        }
    }

    /// nerd, light: the same roles, re-picked for a light ground rather than inverted.
    pub fn light() -> Self {
        Self {
            ground: Color::Rgb(0xfa, 0xfa, 0xfb),
            surface: Color::Rgb(0xec, 0xee, 0xf1),
            slab: Color::Rgb(0x8a, 0x8c, 0x8f),
            border: Color::Rgb(0xd3, 0xd6, 0xdb),
            border_focus: Color::Rgb(0x56, 0xa0, 0x3a),
            text: Color::Rgb(0x1e, 0x20, 0x24),
            muted: Color::Rgb(0x54, 0x58, 0x60),
            faint: Color::Rgb(0x6c, 0x70, 0x77),
            accent: Color::Rgb(0x2e, 0x7d, 0x1f),
            fill: Color::Rgb(0x9f, 0xd0, 0x8a),
            on_fill: Color::Rgb(0x14, 0x21, 0x0f),
            ok: Color::Rgb(0x0f, 0x7a, 0x52),
            warn: Color::Rgb(0xb4, 0x53, 0x09),
            bad: Color::Rgb(0xbc, 0x2c, 0x43),
            wait: Color::Rgb(0x2b, 0x5c, 0xa8),
            teal: Color::Rgb(0x14, 0x6d, 0x79),
            ansi: [
                Color::Rgb(0xdc, 0xde, 0xe2),
                Color::Rgb(0xbc, 0x2c, 0x43),
                Color::Rgb(0x3d, 0x7d, 0x2a),
                Color::Rgb(0x8a, 0x5c, 0x00),
                Color::Rgb(0x2b, 0x5c, 0xa8),
                Color::Rgb(0x6b, 0x3f, 0xb4),
                Color::Rgb(0x14, 0x6d, 0x79),
                Color::Rgb(0x3a, 0x3d, 0x43),
                Color::Rgb(0xa8, 0xac, 0xb3),
                Color::Rgb(0xa2, 0x24, 0x38),
                Color::Rgb(0x33, 0x67, 0x23),
                Color::Rgb(0x74, 0x4d, 0x00),
                Color::Rgb(0x23, 0x4d, 0x8e),
                Color::Rgb(0x5a, 0x34, 0x99),
                Color::Rgb(0x10, 0x5a, 0x64),
                Color::Rgb(0x1e, 0x20, 0x24),
            ],
        }
    }
}

impl Theme {
    /// The row the keys act on. Painted over the whole row, so nothing on it keeps its own colour.
    pub fn cursor(&self) -> Style {
        Style::new().fg(self.on_fill).bg(self.fill)
    }

    /// The same row while the keys are in another area, so it shows where they come back to.
    pub fn cursor_away(&self) -> Style {
        Style::new().fg(self.text).bg(self.slab)
    }

    /// A frame's title follows its frame: the accent while the frame is lit, muted otherwise.
    /// Never bold, since bold says chosen.
    pub fn title(&self, lit: bool) -> Style {
        Style::new().fg(if lit { self.accent } else { self.muted })
    }

    /// What is current or chosen. Bold as well as the accent: colour alone is too close to `text`,
    /// and the weight survives the cursor's fill.
    pub fn chosen(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }
}

/// `#rrggbb` or `rrggbb`; anything else is None, so a typo in the config keeps the built-in color.
fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl Theme {
    /// Config overrides by role name, over the built-in palette. `accent` carries `border_focus`,
    /// and the fill with `ground` on it, unless the config names them too; the sixteen are `ansi0`..`ansi15`.
    pub fn with_overrides(mut self, colors: &std::collections::BTreeMap<String, String>) -> Self {
        for (role, value) in colors {
            let Some(c) = parse_hex(value) else { continue };
            match role.as_str() {
                "ground" => self.ground = c,
                "surface" => self.surface = c,
                "slab" => self.slab = c,
                "border" => self.border = c,
                "border_focus" => self.border_focus = c,
                "text" => self.text = c,
                "muted" => self.muted = c,
                "faint" => self.faint = c,
                "accent" => {
                    self.accent = c;
                    self.border_focus = c;
                    self.fill = c;
                    self.on_fill = self.ground;
                }
                "fill" => self.fill = c,
                "on_fill" => self.on_fill = c,
                "ok" => self.ok = c,
                "warn" => self.warn = c,
                "bad" => self.bad = c,
                "wait" => self.wait = c,
                "teal" => self.teal = c,
                _ => {
                    if let Some(n) = role.strip_prefix("ansi").and_then(|n| n.parse::<usize>().ok())
                        && n < self.ansi.len()
                    {
                        self.ansi[n] = c;
                    }
                }
            }
        }
        // named after accent, so it wins over the accent's carry whatever order the map iterated in
        if let Some(c) = colors.get("border_focus").and_then(|v| parse_hex(v)) {
            self.border_focus = c;
        }
        if colors.contains_key("accent") && !colors.contains_key("on_fill") {
            self.on_fill = self.ground;
        }
        for (role, slot) in [("fill", &mut self.fill), ("on_fill", &mut self.on_fill)] {
            if let Some(c) = colors.get(role).and_then(|v| parse_hex(v)) {
                *slot = c;
            }
        }
        self
    }
}

/// Copies the terminal grid into a buffer rect: glyphs, colors, styles, wide characters.
pub fn blit_grid(buf: &mut Buffer, area: Rect, screen: crate::term::Screen<'_>, theme: &Theme) {
    let (rows, cols) = screen.size();
    let h = area.height.min(rows);
    let w = area.width.min(cols);

    for y in 0..h {
        for x in 0..w {
            let Some(src) = screen.cell(y, x) else {
                continue;
            };
            let Some(dst) = buf.cell_mut((area.x + x, area.y + y)) else {
                continue;
            };

            // ratatui expects a wide char's second cell empty - it infers the width itself.
            if src.is_wide_continuation() {
                dst.reset();
                dst.set_style(cell_style(&src, theme));
                continue;
            }

            // a wide char in the last column has nowhere to draw its second half
            let clipped_wide = src.is_wide() && x + 1 >= w;
            if clipped_wide || !src.has_contents() {
                dst.set_symbol(" ");
            } else if !src.is_wide() && crate::slots::cells(src.contents()) > 1 {
                // U+FE0F after a narrow character: the terminal would draw two cells where the pane has one.
                let narrow: String = src.contents().chars().filter(|c| *c != '\u{fe0f}').collect();
                dst.set_symbol(if crate::slots::cells(&narrow) > 1 { &narrow[..narrow.chars().next().map_or(0, char::len_utf8)] } else { &narrow });
            } else {
                dst.set_symbol(src.contents());
            }
            dst.set_style(cell_style(&src, theme));
        }
    }
}

/// Characters that aren't visible but change how the terminal renders the
/// rest: control characters, bidi overrides, invisible separators, BOM.
/// U+200C and U+200D stay: emoji sequences and some scripts are built with them.
pub fn is_spoofing(c: char) -> bool {
    c.is_control()
        || matches!(c as u32, 0x061C | 0x200B | 0x200E..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF)
}

pub fn unspoofed(s: &str) -> String {
    s.chars().filter(|c| !is_spoofing(*c)).collect()
}

/// Over the finished frame, so no pane, file name, hook field or message reaches the terminal with them.
pub fn strip_spoofing(buf: &mut Buffer) {
    for cell in &mut buf.content {
        if cell.symbol().chars().any(is_spoofing) {
            let clean = unspoofed(cell.symbol());
            cell.set_symbol(if clean.is_empty() { " " } else { &clean });
        }
    }
}

pub fn cursor_at(area: Rect, screen: crate::term::Screen<'_>) -> Option<(u16, u16)> {
    if screen.hide_cursor() {
        return None;
    }
    let (row, col) = screen.cursor_position();
    (row < area.height && col < area.width).then(|| (area.x + col, area.y + row))
}

/// Joins shared corners into ┼ ┬ ┴ ├ ┤ instead of leaving gaps between adjacent borders.
pub fn draw_frames(buf: &mut Buffer, panes: &[(Rect, String, bool)], theme: &Theme) {
    // accumulate directions per point before picking a glyph, so the last border drawn
    // doesn't overwrite a neighbor's corner
    let mut lines: std::collections::HashMap<(u16, u16), (u8, bool)> =
        std::collections::HashMap::new();

    for (area, _, focused) in panes {
        if area.width < 2 || area.height < 2 {
            continue;
        }
        let (l, r) = (area.x, area.x + area.width - 1);
        let (t, b) = (area.y, area.y + area.height - 1);

        for x in l..=r {
            let ends = (u8::from(x > l) * W) | (u8::from(x < r) * E);
            for y in [t, b] {
                mark(&mut lines, x, y, ends, *focused);
            }
        }
        for y in t..=b {
            let ends = (u8::from(y > t) * N) | (u8::from(y < b) * S);
            for x in [l, r] {
                mark(&mut lines, x, y, ends, *focused);
            }
        }
    }

    for ((x, y), (mask, focused)) in lines {
        let style = Style::default().fg(if focused {
            theme.border_focus
        } else {
            theme.border
        });
        put(buf, x, y, box_char(mask), style);
    }

    for (area, title, focused) in panes {
        draw_title(buf, *area, title, *focused, theme);
    }
}

/// A pane's inner content area - the border eats one cell off each edge.
pub fn inner(area: Rect) -> Rect {
    area.inner(Margin::new(1, 1))
}

impl BorderMode {
    fn draws(self, pane_count: usize) -> bool {
        match self {
            Self::Always => true,
            Self::Multiple => pane_count > 1,
            Self::Never => false,
        }
    }
}

/// Without a border the pane gets the whole rect - the layout already separated neighbors via pane_gap.
pub fn pane_inner(rect: Rect, mode: BorderMode, pane_count: usize) -> Rect {
    if mode.draws(pane_count) {
        inner(rect)
    } else {
        rect
    }
}

pub fn draw_panes(buf: &mut Buffer, panes: &[(Rect, String, bool)], mode: BorderMode, theme: &Theme) {
    if mode.draws(panes.len()) {
        draw_frames(buf, panes, theme);
    }
}

/// Not on the alt screen, and either scrolled up already or there's more history above.
pub fn has_scrollback(offset: usize, above: usize, alt_screen: bool) -> bool {
    !alt_screen && (offset > 0 || above > 0)
}

pub fn content_area(inner: Rect, show_scrollbar: bool) -> Rect {
    if show_scrollbar && inner.width > 0 {
        Rect { width: inner.width - 1, ..inner }
    } else {
        inner
    }
}

/// `offset` is how far the view is scrolled back, `above` how many rows are still hidden higher up.
pub fn draw_scrollbar(buf: &mut Buffer, inner: Rect, offset: usize, above: usize, colour: Color, theme: &Theme) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let x = inner.right() - 1;
    let rows = inner.height as usize;
    let total = rows + above + offset;

    // no track: next to the pane's own border it read as a second line
    let thumb_style = Style::default().fg(colour).bg(theme.ground);
    let (top, len) = thumb_span(above, rows, total);
    for y in (inner.y as usize + top)..(inner.y as usize + top + len) {
        put(buf, x, y as u16, '│', thumb_style);
    }
}

/// Thumb position and length, proportional to the visible share of `above + rows + offset`.
fn thumb_span(above: usize, rows: usize, total: usize) -> (usize, usize) {
    if total <= rows || total == 0 {
        return (0, rows);
    }
    let len = (rows * rows / total).clamp(1, rows);
    let top = (above * rows / total).min(rows - len);
    (top, len)
}

const N: u8 = 1;
const S: u8 = 2;
const W: u8 = 4;
const E: u8 = 8;

fn mark(
    map: &mut std::collections::HashMap<(u16, u16), (u8, bool)>,
    x: u16,
    y: u16,
    dirs: u8,
    focused: bool,
) {
    let slot = map.entry((x, y)).or_insert((0, false));
    slot.0 |= dirs;
    // the active pane paints the shared line, keeping its border fully closed
    slot.1 |= focused;
}

fn box_char(mask: u8) -> char {
    match mask {
        m if m == N | S | W | E => '┼',
        m if m == N | S | E => '├',
        m if m == N | S | W => '┤',
        m if m == S | W | E => '┬',
        m if m == N | W | E => '┴',
        m if m == S | E => '┌',
        m if m == S | W => '┐',
        m if m == N | E => '└',
        m if m == N | W => '┘',
        m if m & (N | S) != 0 => '│',
        _ => '─',
    }
}

/// Title format: `┌─ claude ──┐` - the dash at area.x + 1 is drawn by the shared border pass.
fn draw_title(buf: &mut Buffer, area: Rect, title: &str, focused: bool, theme: &Theme) {
    if area.width < 6 || area.height < 2 {
        return;
    }
    let name = theme.title(focused);

    put(buf, area.x + 2, area.y, ' ', name);

    // leave a cell for the right corner and a space before it
    let room = area.width.saturating_sub(5) as usize;
    if room > 0 && buf.area.contains((area.x + 3, area.y).into()) {
        let (end, _) = buf.set_stringn(area.x + 3, area.y, title, room, name);
        put(buf, end, area.y, ' ', name);
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, symbol: char, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(symbol);
        cell.set_style(style);
    }
}

fn cell_style(cell: &crate::term::Cell, theme: &Theme) -> Style {
    let mut style = Style::default()
        .fg(convert(cell.fgcolor(), theme.text, theme))
        .bg(convert(cell.bgcolor(), theme.ground, theme));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        // pass SGR 7 through instead of swapping colors ourselves - swapping Reset for Reset would do nothing
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// The theme owns default and named colors; an exact color a program chose stays its own.
fn convert(color: crate::term::Color, default: Color, theme: &Theme) -> Color {
    match color {
        crate::term::Color::Default => default,
        crate::term::Color::Idx(i) if i < 16 => theme.ansi[i as usize],
        crate::term::Color::Idx(i) => Color::Indexed(i),
        crate::term::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The fill instead of inverted colors, so the selection stays visible over colored output.
pub fn highlight(buf: &mut Buffer, inner: Rect, sel: &crate::select::Selection, theme: &Theme) {
    for y in 0..inner.height {
        for x in 0..inner.width {
            if sel.contains(y, x) {
                let (bx, by) = (inner.x + x, inner.y + y);
                if let Some(cell) = buf.cell_mut((bx, by)) {
                    cell.set_bg(theme.fill).set_fg(theme.on_fill);
                }
            }
        }
    }
}

/// Auto follows the appearance; an unknown one gets dark, the owner's default.
/// The theme and whether it is the dark one.
pub(crate) fn pick_theme(cfg: &crate::config::Config, dark: Option<bool>) -> (Theme, bool) {
    use crate::config::ThemeMode;
    let dark = !matches!((cfg.theme, dark), (ThemeMode::Light, _) | (ThemeMode::Auto, Some(false)));
    let base = if dark { Theme::dark() } else { Theme::light() };
    (base.with_overrides(&cfg.colors), dark)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_overrides_from_the_pane_never_reach_the_client() {
        // RLO after "a" reverses the line in the other terminal - it doesn't make it into the buffer.
        let mut parser = crate::term::Term::new(10, 2, 0);
        parser.feed("ab\u{202e}cd".as_bytes());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        blit_grid(&mut buf, Rect::new(0, 0, 10, 2), parser.screen(), &Theme::dark());
        // As ui::frame does over the whole frame.
        strip_spoofing(&mut buf);
        let line: String = (0..4).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(line, "abcd");
    }

    #[test]
    fn a_cell_draws_as_wide_on_screen_as_it_is_in_the_pane() {
        // U+FE0F makes the outer terminal draw the arrow two cells wide; the pane gave it one.
        let pane = screen_of(12, 1, "A\u{2b07}\u{fe0f}bc\u{2764}\u{fe0f}d\u{2705}e".as_bytes());
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
        blit_grid(&mut buf, Rect::new(0, 0, 12, 1), pane.screen(), &Theme::dark());
        let widths: Vec<usize> = (0..12).map(|x| crate::slots::cells(buf[(x, 0)].symbol())).collect();
        assert_eq!(&widths[..9], &[1, 1, 1, 1, 1, 1, 2, 1, 1], "each cell as wide as the pane made it, a wide emoji's second half a blank: {widths:?}");
        assert_eq!(text_at(&buf, 0, 0, 6), "A\u{2b07}bc\u{2764}d", "the character stays, only its emoji form goes");
    }

    #[test]
    fn the_frame_pass_takes_out_what_reached_a_cell_and_keeps_joiners() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(0, 0)].set_symbol("a\u{202E}");
        buf[(1, 0)].set_symbol("\u{200B}");
        buf[(2, 0)].set_symbol("\u{1F469}\u{200D}\u{1F4BB}");
        strip_spoofing(&mut buf);
        assert_eq!((buf[(0, 0)].symbol(), buf[(1, 0)].symbol()), ("a", " "));
        assert_eq!(buf[(2, 0)].symbol(), "\u{1F469}\u{200D}\u{1F4BB}", "an emoji sequence keeps its joiner");
        assert_eq!(unspoofed("a\u{202E}b\u{200C}c\x1b"), "ab\u{200C}c");
    }

    fn screen_of(cols: u16, rows: u16, bytes: &[u8]) -> crate::term::Term {
        let mut p = crate::term::Term::new(cols, rows, 0);
        p.feed(bytes);
        p
    }

    fn text_at(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
        (x..x + len).map(|i| buf[(i, y)].symbol()).collect()
    }

    #[test]
    fn plain_text_lands_in_the_right_cells() {
        let p = screen_of(20, 3, b"hello\r\nworld");
        let mut buf = Buffer::empty(Rect::new(0, 0, 24, 5));
        blit_grid(&mut buf, Rect::new(2, 1, 20, 3), p.screen(), &Theme::dark());

        assert_eq!(text_at(&buf, 2, 1, 5), "hello");
        assert_eq!(text_at(&buf, 2, 2, 5), "world");
        // didn't spill past area
        assert_eq!(buf[(1, 1)].symbol(), " ");
    }

    #[test]
    fn colours_and_bold_survive_the_trip() {
        let p = screen_of(20, 2, b"\x1b[1;31;48;2;10;20;30mRED\x1b[m ok");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        blit_grid(&mut buf, Rect::new(0, 0, 20, 2), p.screen(), &Theme::dark());

        let red = &buf[(0, 0)];
        assert_eq!(red.symbol(), "R");
        assert_eq!(red.fg, Theme::dark().ansi[1], "a named color is the theme's");
        assert_eq!(red.bg, Color::Rgb(10, 20, 30), "an exact one stays the program's");
        assert!(red.modifier.contains(Modifier::BOLD));

        let plain = &buf[(4, 0)];
        assert_eq!((plain.fg, plain.bg), (Theme::dark().text, Theme::dark().ground), "default colors are the theme's too");
        assert!(!plain.modifier.contains(Modifier::BOLD));

        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        let p = screen_of(20, 1, b"\x1b[38;5;200mX");
        blit_grid(&mut buf, Rect::new(0, 0, 20, 1), p.screen(), &Theme::dark());
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(200), "past the 16 named colors the number stays");
    }

    #[test]
    fn config_colors_override_the_built_in_palette() {
        let map = |pairs: &[(&str, &str)]| {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::BTreeMap<_, _>>()
        };

        let t = Theme::dark().with_overrides(&map(&[("accent", "#ff0000")]));
        assert_eq!(t.accent, Color::Rgb(0xff, 0, 0));
        assert_eq!(t.border_focus, t.accent, "the focus ring is the accent unless the config says otherwise");
        assert_eq!((t.fill, t.on_fill), (t.accent, t.ground), "so is the fill, with the ground on it");

        let t = Theme::light().with_overrides(&map(&[("accent", "#ff0000"), ("fill", "#00ff00"), ("on_fill", "#000000")]));
        assert_eq!((t.fill, t.on_fill), (Color::Rgb(0, 0xff, 0), Color::Rgb(0, 0, 0)), "named fill and on_fill win over the accent's carry");

        let t = Theme::dark().with_overrides(&map(&[("accent", "#ff0000"), ("border_focus", "#0000ff")]));
        assert_eq!(t.border_focus, Color::Rgb(0, 0, 0xff), "a named border_focus wins over the accent's carry");

        let t = Theme::dark().with_overrides(&map(&[("ground", "102030"), ("ansi5", "#abcdef")]));
        assert_eq!(t.ground, Color::Rgb(0x10, 0x20, 0x30), "the hash is optional");
        assert_eq!(t.ansi[5], Color::Rgb(0xab, 0xcd, 0xef));

        let t = Theme::dark().with_overrides(&map(&[
            ("accent", "#12345"),
            ("text", "not a color"),
            ("nosuchrole", "#ffffff"),
            ("ansi16", "#ffffff"),
            ("ansi99", "#ffffff"),
        ]));
        assert_eq!(t.accent, Theme::dark().accent, "a malformed value keeps the built-in color");
        assert_eq!(t.text, Theme::dark().text);
        assert_eq!(t.ansi, Theme::dark().ansi, "an out-of-range index changes nothing");

        assert_eq!(Theme::dark().with_overrides(&Default::default()).ground, Theme::dark().ground);
    }

    #[test]
    fn every_text_tone_passes_wcag_in_both_themes() {
        fn lum(c: Color) -> f64 {
            let Color::Rgb(r, g, b) = c else { panic!("{c:?} is not an exact color") };
            let ch = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
        }
        fn ratio(a: Color, b: Color) -> f64 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        for t in [Theme::dark(), Theme::light()] {
            for (name, c, min) in [("text", t.text, 4.5), ("muted", t.muted, 4.5), ("faint", t.faint, 3.0),
                ("accent", t.accent, 3.0), ("ok", t.ok, 3.0), ("warn", t.warn, 3.0), ("bad", t.bad, 3.0), ("wait", t.wait, 3.0), ("teal", t.teal, 3.0)]
            {
                let r = ratio(c, t.ground);
                assert!(r >= min, "{name} is {r:.2}:1 against the ground, needs {min}:1");
            }
            // WCAG 1.4.11: a state is marked at 3:1 against what is next to it
            assert!(ratio(t.slab, t.ground) >= 3.0, "the away cursor is {:.2}:1 against the ground", ratio(t.slab, t.ground));
            assert!(ratio(t.text, t.slab) >= 4.5, "text on the away cursor is {:.2}:1", ratio(t.text, t.slab));
            assert!(ratio(t.on_fill, t.fill) >= 4.5, "text on the cursor is {:.2}:1", ratio(t.on_fill, t.fill));
            assert!(ratio(t.border_focus, t.ground) >= 3.0, "the focus ring is {:.2}:1 against the ground", ratio(t.border_focus, t.ground));
            // The accent says where the keys are. A state that borrows it says two things
            // with one color, and the reader has to know which one is meant.
            assert_ne!(t.ok, t.accent, "the ok green is not the accent");
        }
    }

    #[test]
    fn wide_char_keeps_its_second_cell_blank() {
        let p = screen_of(10, 1, "日x".as_bytes());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        blit_grid(&mut buf, Rect::new(0, 0, 10, 1), p.screen(), &Theme::dark());

        assert_eq!(buf[(0, 0)].symbol(), "日");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].symbol(), "x");
    }

    #[test]
    fn content_is_clipped_to_a_smaller_area() {
        let p = screen_of(20, 4, b"abcdefgh\r\nijklmnop\r\nqrstuvwx");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        // area is narrower and below the screen - nothing should panic or overflow
        blit_grid(&mut buf, Rect::new(0, 0, 3, 2), p.screen(), &Theme::dark());

        assert_eq!(text_at(&buf, 0, 0, 4), "abc ");
        assert_eq!(text_at(&buf, 0, 1, 4), "ijk ");
        assert_eq!(text_at(&buf, 0, 2, 3), "   ");
    }

    #[test]
    fn blit_survives_an_area_outside_the_buffer() {
        let p = screen_of(20, 4, b"abc");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        blit_grid(&mut buf, Rect::new(40, 40, 20, 4), p.screen(), &Theme::dark());
    }

    #[test]
    fn inner_shrinks_by_one_on_every_side() {
        assert_eq!(inner(Rect::new(3, 4, 10, 6)), Rect::new(4, 5, 8, 4));
        // a degenerate pane must not produce a negative size
        assert_eq!(inner(Rect::new(0, 0, 1, 1)).width, 0);
    }

    #[test]
    fn four_panes_join_into_a_cross() {
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 11));
        let panes = vec![
            (Rect::new(0, 0, 11, 6), "a".into(), false),
            (Rect::new(10, 0, 10, 6), "b".into(), false),
            (Rect::new(0, 5, 11, 6), "c".into(), false),
            (Rect::new(10, 5, 10, 6), "d".into(), false),
        ];
        draw_frames(&mut buf, &panes, &theme);

        assert_eq!(buf[(10, 5)].symbol(), "┼");
        assert_eq!(buf[(10, 0)].symbol(), "┬");
        assert_eq!(buf[(10, 10)].symbol(), "┴");
        assert_eq!(buf[(0, 5)].symbol(), "├");
        assert_eq!(buf[(19, 5)].symbol(), "┤");
        assert_eq!(buf[(0, 0)].symbol(), "┌");
        assert_eq!(buf[(19, 10)].symbol(), "┘");
    }

    #[test]
    fn title_is_the_command_name_after_a_line_char() {
        let theme = Theme::dark();
        // A fresh buffer each time: styles in ratatui layer on top of old ones.
        let drawn = |name: &str, focused| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
            let panes = vec![(Rect::new(0, 0, 20, 4), name.to_string(), focused)];
            draw_frames(&mut buf, &panes, &theme);
            buf
        };

        let buf = drawn("claude", true);
        assert_eq!(text_at(&buf, 0, 0, 12), "┌─ claude ──");
        assert_eq!(buf[(3, 0)].fg, theme.accent, "the title follows its lit frame");
        assert!(!buf[(3, 0)].modifier.contains(Modifier::BOLD), "never bold: bold says chosen");
        assert_eq!(buf[(19, 0)].symbol(), "┐");

        // inactive pane - dimmed label without bold
        let buf = drawn("bun", false);
        assert_eq!(text_at(&buf, 0, 0, 8), "┌─ bun ─");
        assert_eq!(buf[(3, 0)].fg, theme.muted);
        assert!(!buf[(3, 0)].modifier.contains(Modifier::BOLD));

        // long name gets truncated, right corner stays intact
        let buf = drawn("extremely-long-pane-name-indeed", false);
        assert_eq!(buf[(19, 0)].symbol(), "┐");
    }

    #[test]
    fn border_mode_always_draws_regardless_of_pane_count() {
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        let panes = vec![(Rect::new(0, 0, 10, 5), "a".into(), false)];
        draw_panes(&mut buf, &panes, BorderMode::Always, &theme);
        assert_eq!(buf[(0, 0)].symbol(), "┌");
    }

    #[test]
    fn border_mode_multiple_only_with_more_than_one_pane() {
        let theme = Theme::dark();

        let mut solo = Buffer::empty(Rect::new(0, 0, 10, 5));
        let one = vec![(Rect::new(0, 0, 10, 5), "a".into(), false)];
        draw_panes(&mut solo, &one, BorderMode::Multiple, &theme);
        assert_eq!(solo[(0, 0)].symbol(), " ");

        let mut pair = Buffer::empty(Rect::new(0, 0, 10, 5));
        let two = vec![
            (Rect::new(0, 0, 5, 5), "a".into(), false),
            (Rect::new(5, 0, 5, 5), "b".into(), false),
        ];
        draw_panes(&mut pair, &two, BorderMode::Multiple, &theme);
        assert_eq!(pair[(0, 0)].symbol(), "┌");
    }

    #[test]
    fn border_mode_never_draws_nothing() {
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        let two = vec![
            (Rect::new(0, 0, 5, 5), "a".into(), false),
            (Rect::new(5, 0, 5, 5), "b".into(), false),
        ];
        draw_panes(&mut buf, &two, BorderMode::Never, &theme);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].symbol(), " "); // and no label either
    }

    #[test]
    fn pane_inner_follows_the_mode() {
        let rect = Rect::new(3, 4, 10, 6);
        assert_eq!(pane_inner(rect, BorderMode::Always, 2), inner(rect));
        assert_eq!(pane_inner(rect, BorderMode::Never, 2), rect);
        assert_eq!(pane_inner(rect, BorderMode::Multiple, 1), rect);
        assert_eq!(pane_inner(rect, BorderMode::Multiple, 2), inner(rect));
    }

    #[test]
    fn scrollbar_hidden_without_offset_or_history() {
        assert!(!has_scrollback(0, 0, false));
        assert!(has_scrollback(1, 0, false));
        assert!(has_scrollback(0, 5, false));
    }

    #[test]
    fn scrollbar_hidden_on_alt_screen_even_when_scrolled() {
        assert!(!has_scrollback(10, 10, true));
    }

    #[test]
    fn content_area_shrinks_only_when_scrollbar_is_shown() {
        let r = Rect::new(2, 3, 10, 5);
        assert_eq!(content_area(r, false), r);
        assert_eq!(content_area(r, true), Rect::new(2, 3, 9, 5));
    }

    #[test]
    fn scrollbar_thumb_sits_at_the_bottom_when_viewing_the_live_screen() {
        // offset 0 - at the bottom, live screen; all history above is hidden
        let (top, len) = thumb_span(10, 10, 20);
        assert_eq!((top, len), (5, 5));
    }

    #[test]
    fn scrollbar_thumb_sits_at_the_top_when_scrolled_to_the_very_start() {
        // above 0 - nothing left above, the rest of history is below (offset)
        let (top, len) = thumb_span(0, 10, 20);
        assert_eq!((top, len), (0, 5));
    }

    #[test]
    fn scrollbar_thumb_fills_the_track_when_everything_fits() {
        let (top, len) = thumb_span(0, 10, 10);
        assert_eq!((top, len), (0, 10));
    }

    #[test]
    fn scrollbar_paints_only_the_thumb_in_the_right_column() {
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        let inner = Rect::new(0, 0, 10, 10);

        draw_scrollbar(&mut buf, inner, 0, 10, theme.muted, &theme);

        // above=10, rows=10, offset=0 -> total=20 -> thumb in the lower half, and no track above it
        for y in 0..5u16 {
            assert_eq!(buf[(9, y)].symbol(), " ", "no track on row {y}");
        }
        for y in 5..10u16 {
            assert_eq!(buf[(9, y)].symbol(), "│");
            assert_eq!(buf[(9, y)].fg, theme.muted);
        }
        // the neighboring column is untouched
        assert_eq!(buf[(8, 0)].symbol(), " ");
    }

    #[test]
    fn cursor_is_none_when_hidden_and_when_off_screen() {
        let p = screen_of(20, 4, b"ab");
        assert_eq!(cursor_at(Rect::new(5, 5, 20, 4), p.screen()), Some((7, 5)));

        let hidden = screen_of(20, 4, b"ab\x1b[?25l");
        assert_eq!(cursor_at(Rect::new(5, 5, 20, 4), hidden.screen()), None);

        // cursor outside the pane's selected chunk
        assert_eq!(cursor_at(Rect::new(5, 5, 1, 4), p.screen()), None);
    }
}
