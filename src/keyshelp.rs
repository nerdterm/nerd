//! Key reference screen (`⌥+K`). The global section comes from the table the dispatcher
//! reads; `main.rs::print_help` is still kept in step with it by hand.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::config::Prefix;
use crate::render::Theme;

/// Rows the handlers do not own: the mouse, and the keys each area answers. The `⌥` section
/// is not here at all - it is printed from `keys::GLOBAL`, the table the dispatcher reads.
/// The only rows still written by hand: the ones no key handler owns. The mouse has no key
/// table to come from, and the two app-level rows describe `App::key` itself.
const ROWS: &[(Option<&str>, &str, &str)] = &[
    (None, "⌥+shift+arrow", "next pane that way, then the sidebar, the tree or the tabs"),
    (None, "arrows, return", "inside the sidebar or the tabs, without ⌥"),
    (Some("Mouse"), "click", "focus a pane, switch a tab, press a button"),
    (None, "right click", "menu for a pane, tab, space, sidebar row or tree row"),
    (None, "tree header", "full screen and close buttons"),
    (None, "drag", "on a border resizes, on text selects"),
    (None, "double click in a pane", "select a word"),
    (None, "double click in the tree", "open a file"),
    (None, "double click in New space", "open the folder"),
    (None, "triple click", "select a line in a pane"),
    (None, "wheel", "scroll pane history"),
    (None, "ctrl+click", "open link under the pointer"),
];


fn hint(width: usize) -> Vec<String> {
    crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), width)
}

enum Line {
    Head(&'static str),
    Bind(&'static str, &'static str),
}

/// The whole list: the global shortcuts, straight out of the table the dispatcher reads, then
/// the rows above. Nothing about a `⌥` key is spelled twice.
const CHORD: &str = "what ⌥ and that key do: a letter, a digit, or an arrow without shift";

/// Shown only while the prefix is on, or the list would teach a key that does nothing.
fn chord_rows(p: Prefix) -> &'static [(&'static str, &'static str)] {
    match p {
        Prefix::Off => &[],
        Prefix::CtrlB => &[("⌃+B, then a key", CHORD), ("⌃+B twice", "sends ⌃+B to the pane")],
        Prefix::CtrlA => &[("⌃+A, then a key", CHORD), ("⌃+A twice", "sends ⌃+A to the pane")],
    }
}

fn rows(prefix: Prefix) -> Vec<(Option<&'static str>, &'static str, &'static str)> {
    let mut out: Vec<(Option<&'static str>, &'static str, &'static str)> = Vec::new();
    for (i, g) in crate::keys::GLOBAL.iter().enumerate() {
        out.push((i.eq(&0).then_some("⌥ and a letter, made for a Latin layout (U.S., ABC)"), g.label, g.what));
    }
    out.push((None, "⌥+1..9", "switch to that tab"));
    // the arrow rows are headless and still belong to the `⌥` section above
    let (arrows, rest) = ROWS.split_at(ROWS.iter().position(|r| r.0.is_some()).unwrap_or(ROWS.len()));
    out.extend(arrows.iter().copied());
    for (i, (label, what)) in chord_rows(prefix).iter().enumerate() {
        out.push((i.eq(&0).then_some("The tmux-style prefix"), label, what));
    }
    out.extend(rest.iter().copied());

    let mut section = |head: &'static str, rows: Vec<(&'static str, &'static str)>| {
        let mut first = Some(head);
        for (label, what) in rows {
            if label.is_empty() {
                continue;
            }
            out.push((first.take(), label, what));
        }
    };
    section("Navigator", crate::navigator::KEYS.iter().map(|r| (r.label, r.what)).collect());
    section("Tree", crate::tree::COMMAND.iter().map(|r| (r.label, r.what)).collect());
    section("Tree, in the preview", crate::tree::PREVIEW.iter().map(|r| (r.label, r.what)).collect());
    section("Settings", crate::settings::KEYS.iter().map(|r| (r.label, r.what)).collect());
    section("This screen", KEYS.iter().map(|r| (r.label, r.what)).collect());
    section("A menu", crate::menu::MENU_KEYS.iter().map(|r| (r.0, r.2)).collect());
    section("A list to pick from", crate::menu::PICKER_KEYS.iter().map(|r| (r.0, r.2)).collect());
    section("A confirmation", crate::menu::CONFIRM_KEYS.iter().map(|r| (r.0, r.2)).collect());
    section("A text field", crate::menu::FIELD_KEYS.iter().map(|r| (r.0, r.2)).collect());
    section("New space", crate::space_picker::KEYS.iter().map(|r| (r.0, r.2)).collect());
    out
}

/// One key this screen answers, in the same shape as every other area: the handler dispatches
/// on these rows, its own strip prints them, and the list below carries them as a section.
pub struct Key {
    pub code: crossterm::event::KeyCode,
    /// Empty when the row is a second code for the row above.
    pub label: &'static str,
    pub word: &'static str,
    pub what: &'static str,
    act: fn(&mut KeysHelp) -> bool,
}

pub const KEYS: &[Key] = &[
    Key { code: crossterm::event::KeyCode::Up, label: "↑↓", word: "", what: "scroll the list",
          act: |h| { h.scroll(-1); false } },
    Key { code: crossterm::event::KeyCode::Down, label: "", word: "", what: "scroll the list",
          act: |h| { h.scroll(1); false } },
    Key { code: crossterm::event::KeyCode::PageUp, label: "PgUp PgDn", word: "", what: "a screen at a time",
          act: |h| { h.scroll(-10); false } },
    Key { code: crossterm::event::KeyCode::PageDown, label: "", word: "", what: "a screen at a time",
          act: |h| { h.scroll(10); false } },
    Key { code: crossterm::event::KeyCode::Home, label: "Home End", word: "", what: "the top and the bottom",
          act: |h| { h.offset = 0; false } },
    Key { code: crossterm::event::KeyCode::End, label: "", word: "", what: "the top and the bottom",
          act: |h| { h.offset = h.lines().len().saturating_sub(1); false } },
    Key { code: crossterm::event::KeyCode::Char('/'), label: "/", word: "search", what: "filter the list",
          act: |h| { h.filtering = true; false } },
    Key { code: crossterm::event::KeyCode::Esc, label: "esc", word: "close", what: "clear the filter, then close",
          act: |h| {
              if h.query.is_empty() {
                  return true;
              }
              h.query.clear();
              h.offset = 0;
              false
          } },
];

pub struct KeysHelp {
    pub query: String,
    /// While typing, printable keys go into `query`, not `q`-to-close.
    filtering: bool,
    offset: usize,
    prefix: Prefix,
}

impl KeysHelp {
    pub fn new(prefix: Prefix) -> Self {
        Self { query: String::new(), filtering: false, offset: 0, prefix }
    }

    /// Fits the whole list, not the filtered part, so it does not move while typing; a longer list scrolls.
    pub fn rect(&self, screen: Rect) -> Rect {
        let all = rows(self.prefix);
        let (keys, desc) = all.iter().fold((0, 0), |(k, d), r| (k.max(r.1.chars().count()), d.max(r.2.chars().count())));
        let widest = (3 + keys + desc) as u16;
        let strip = hint(crate::chrome::fit(screen, widest, 0).width.saturating_sub(4) as usize).len();
        // the search field, the list, the key strip
        crate::chrome::fit(screen, widest, (1 + Self::lines_for(self.prefix, "").len() + strip) as u16)
    }

    /// A section heading is included only when something in it matched.
    fn lines(&self) -> Vec<Line> {
        Self::lines_for(self.prefix, &self.query)
    }

    fn lines_for(prefix: Prefix, query: &str) -> Vec<Line> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        let mut pending: Option<&'static str> = None;
        for (head, keys, desc) in rows(prefix) {
            if let Some(h) = head {
                pending = Some(h);
            }
            if !q.is_empty() && !keys.to_lowercase().contains(&q) && !desc.to_lowercase().contains(&q) {
                continue;
            }
            if let Some(h) = pending.take() {
                out.push(Line::Head(h));
            }
            out.push(Line::Bind(keys, desc));
        }
        out
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) {
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return;
        }
        crate::chrome::dialog(buf, r, Some("Keys and mouse"), theme.border_focus, theme, crate::chrome::Moat::All);
        let inner = crate::chrome::close_button(buf, r, theme);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let width = inner.width as usize;
        let mut y = inner.y;

        let (text, style) = if self.query.is_empty() {
            ("/ search keys".to_string(), Style::default().fg(theme.faint))
        } else {
            (self.query.clone(), Style::default().fg(theme.text))
        };
        buf.set_stringn(inner.x, y, &text, width, style);
        y += 1;

        // Carve off the bottom before the list, or a long list overwrites the hint.
        let bottom = crate::chrome::draw_hint(buf, inner, &hint(width), theme).max(y);

        let lines = self.lines();
        // Filtering can shorten the list past the previous offset, leaving the window empty.
        let page = bottom.saturating_sub(y) as usize;
        let offset = self.offset.min(lines.len().saturating_sub(1));
        if lines.is_empty() {
            if y < bottom {
                buf.set_stringn(inner.x, y, " no keys", width, Style::default().fg(theme.faint));
            }
            return;
        }

        let keys_w = rows(self.prefix).iter().map(|r| r.1.chars().count()).max().unwrap_or(0) as u16;

        for line in lines.iter().skip(offset).take(page) {
            match line {
                Line::Head(h) => {
                    buf.set_stringn(inner.x, y, *h, width, Style::default().fg(theme.muted));
                }
                Line::Bind(keys, desc) => {
                    buf.set_stringn(
                        inner.x + 2,
                        y,
                        *keys,
                        width.saturating_sub(2),
                        Style::default().fg(theme.muted),
                    );
                    let dx = inner.x + 3 + keys_w;
                    if dx < inner.right() {
                        buf.set_stringn(
                            dx,
                            y,
                            *desc,
                            (inner.right() - dx) as usize,
                            Style::default().fg(theme.text),
                        );
                    }
                }
            }
            y += 1;
        }
    }

    /// `true` means close the screen.
    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode::*;

        // A command is the letter on the key; a query is what the layout printed. Normalising
        // here and not while typing is what lets a Russian keyboard do both.
        let k = if self.filtering { k } else { crate::keys::normalize(k) };
        if self.filtering {
            match k.code {
                Esc => {
                    self.filtering = false;
                    self.query.clear();
                    self.offset = 0;
                }
                Enter => self.filtering = false,
                Backspace => {
                    self.query.pop();
                    self.offset = 0;
                }
                Char(c) => {
                    self.query.push(c);
                    self.offset = 0;
                }
                Up => self.scroll(-1),
                Down => self.scroll(1),
                _ => {}
            }
            return false;
        }

        if let Some(r) = KEYS.iter().find(|r| r.code == k.code) {
            return (r.act)(self);
        }
        false
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect) -> bool {
        use crossterm::event::MouseEventKind::*;
        match ev.kind {
            Down(_) => {
                let (r, at) = (self.rect(screen), (ev.column, ev.row).into());
                !r.contains(at) || crate::chrome::close_rect(r).contains(at)
            }
            ScrollUp => {
                self.scroll(-3);
                false
            }
            ScrollDown => {
                self.scroll(3);
                false
            }
            _ => false,
        }
    }

    fn scroll(&mut self, delta: isize) {
        let last = self.lines().len().saturating_sub(1) as isize;
        self.offset = (self.offset as isize + delta).clamp(0, last) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn theme() -> Theme {
        Theme::dark()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    fn binds(h: &KeysHelp) -> Vec<(&'static str, &'static str)> {
        h.lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Bind(k, d) => Some((k, d)),
                Line::Head(_) => None,
            })
            .collect()
    }

    fn heads(h: &KeysHelp) -> Vec<&'static str> {
        h.lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Head(s) => Some(s),
                Line::Bind(..) => None,
            })
            .collect()
    }

    fn typed(query: &str) -> KeysHelp {
        let mut h = KeysHelp::new(Prefix::Off);
        h.key(key(KeyCode::Char('/')));
        for c in query.chars() {
            h.key(key(KeyCode::Char(c)));
        }
        h
    }

    #[test]
    fn one_mouse_section_says_where_each_double_click_acts_and_promises_no_desktop_menu() {
        let all = rows(Prefix::Off);
        assert_eq!(all.iter().filter(|r| r.0.is_some_and(|h| h.starts_with("Mouse"))).count(), 1, "one mouse section");
        let text: String = all.iter().map(|r| format!("{} {}\n", r.1, r.2)).collect();
        assert!(!text.contains("desktop"), "right click on empty space opens nothing");
        for (label, what) in [("double click in a pane", "select a word"), ("double click in the tree", "open a file")] {
            assert!(all.iter().any(|r| r.1 == label && r.2 == what), "{label}");
        }
        assert!(!all.iter().any(|r| r.1 == "double click"), "no double click without a place");
        let section = |head: &str| {
            let at = all.iter().position(|r| r.0 == Some(head)).unwrap_or_else(|| panic!("{head}"));
            all[at..].iter().enumerate().take_while(|(i, r)| *i == 0 || r.0.is_none()).map(|(_, r)| r.1).collect::<Vec<_>>()
        };
        assert_eq!(section("A confirmation"), ["enter", "y", "n, esc", "d", "←→ tab"]);
        assert_eq!(section("A text field"), ["enter", "esc", "←→ Home End", "ctrl+u"]);
        assert_eq!(section("New space"), ["tab", "↑↓", "enter", "esc"]);
        assert!(!crate::keys::GLOBAL.iter().any(|g| g.what.contains("confirmations are off")));
    }

    #[test]
    fn the_whole_list_shows_with_every_section() {
        let h = KeysHelp::new(Prefix::Off);
        assert_eq!(binds(&h).len(), rows(Prefix::Off).len());
        assert_eq!(
            heads(&h),
            vec![
                "⌥ and a letter, made for a Latin layout (U.S., ABC)",
                "Mouse",
                "Navigator",
                "Tree",
                "Tree, in the preview",
                "Settings",
                "This screen",
                "A menu",
                "A list to pick from",
                "A confirmation",
                "A text field",
                "New space",
            ],
            "every area that answers a key has a section, and they come from its own table"
        );
    }

    #[test]
    fn the_prefix_has_a_section_only_while_it_is_on() {
        assert!(!heads(&KeysHelp::new(Prefix::Off)).contains(&"The tmux-style prefix"));
        let listed: Vec<&str> = rows(Prefix::CtrlA).iter().map(|r| r.1).collect();
        assert!(listed.contains(&"⌃+A, then a key"), "{listed:?}");
        let with = rows(Prefix::CtrlB);
        let at = with.iter().position(|r| r.0 == Some("The tmux-style prefix")).unwrap();
        let arrow = with.iter().position(|r| r.1 == "⌥+shift+arrow").unwrap();
        assert!(arrow < at, "the arrow row stays under the ⌥ heading");
        assert_eq!(with[at + 2].0, Some("Mouse"), "and the section holds its own two rows only");
    }

    #[test]
    fn the_list_says_only_what_a_handler_answers() {
        // every tree key that carries a label is in the list, and nothing about the tree is
        // spelled here by hand: the section is the table itself
        let listed: Vec<&str> = rows(Prefix::Off).iter().map(|r| r.1).collect();
        for r in crate::tree::COMMAND.iter().filter(|r| !r.label.is_empty()) {
            assert!(listed.contains(&r.label), "the tree answers {:?} and the list omits it", r.label);
        }
        for r in crate::tree::PREVIEW.iter().filter(|r| !r.label.is_empty()) {
            assert!(listed.contains(&r.label), "the preview answers {:?} and the list omits it", r.label);
        }
        for r in crate::navigator::KEYS.iter().filter(|r| !r.label.is_empty()) {
            assert!(listed.contains(&r.label), "the navigator answers {:?} and the list omits it", r.label);
        }
    }

    #[test]
    fn filter_narrows_by_key_name() {
        let h = typed("⌥+");
        let found = binds(&h);
        assert!(!found.is_empty());
        assert!(found.iter().any(|(k, _)| *k == "⌥+K"));
        assert!(found.iter().any(|(k, _)| *k == "⌥+Q"));
        assert!(found.len() < rows(Prefix::Off).len(), "list narrowed");
    }

    #[test]
    fn filter_narrows_by_description_text() {
        let h = typed("sidebar");
        let found = binds(&h);
        assert!(found.iter().any(|(k, _)| *k == "⌥+S"), "the row whose description carries the word");
        assert!(
            found.iter().all(|(k, d)| k.contains("sidebar") || d.contains("sidebar")),
            "and nothing that does not: {found:?}"
        );
        assert!(found.len() < rows(Prefix::Off).len(), "list narrowed");
        assert!(
            heads(&h).contains(&"⌥ and a letter, made for a Latin layout (U.S., ABC)"),
            "the heading of a section that kept a row"
        );
    }

    #[test]
    fn a_filter_matching_nothing_leaves_no_headers() {
        let h = typed("nosuchcombination");
        assert!(binds(&h).is_empty());
        assert!(heads(&h).is_empty());
    }

    #[test]
    fn filter_is_case_insensitive() {
        assert_eq!(binds(&typed("ENTER")).len(), binds(&typed("enter")).len());
        assert!(!binds(&typed("ENTER")).is_empty());
    }

    #[test]
    fn esc_clears_the_query_before_closing() {
        let mut h = typed("tab");
        assert_eq!(h.query, "tab");

        assert!(!h.key(key(KeyCode::Esc)), "first esc only clears the search");
        assert_eq!(h.query, "");
        assert!(h.key(key(KeyCode::Esc)), "second one closes");
    }

    #[test]
    fn typing_goes_into_the_query_not_into_the_hotkeys() {
        let mut h = typed("q");
        assert_eq!(h.query, "q", "\"q\" while searching doesn't close the screen");
        h.key(key(KeyCode::Backspace));
        assert_eq!(h.query, "");
        h.key(key(KeyCode::Enter));
        assert!(h.key(key(KeyCode::Esc)));
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut h = KeysHelp::new(Prefix::Off);
        h.key(key(KeyCode::Down));
        assert_eq!(h.offset, 1);
        h.key(key(KeyCode::Up));
        assert_eq!(h.offset, 0);
        h.key(key(KeyCode::Up));
        assert_eq!(h.offset, 0, "didn't go past the start");

        h.key(key(KeyCode::End));
        let last = h.lines().len() - 1;
        assert_eq!(h.offset, last);
        h.key(key(KeyCode::PageDown));
        assert_eq!(h.offset, last, "didn't go past the end");
        h.key(key(KeyCode::Home));
        assert_eq!(h.offset, 0);
    }

    #[test]
    fn the_filter_starts_on_the_key_whatever_the_layout_prints() {
        let mut h = KeysHelp::new(Prefix::Off);
        // the physical slash key prints a full stop on a Russian layout
        h.key(key(KeyCode::Char('.')));
        h.key(key(KeyCode::Char('z')));
        assert_eq!(h.query, "z", "the filter opened and took the letter");
    }

    #[test]
    fn a_query_keeps_the_letters_the_layout_printed() {
        let mut h = KeysHelp::new(Prefix::Off);
        h.key(key(KeyCode::Char('/')));
        for c in "щи".chars() {
            h.key(key(KeyCode::Char(c)));
        }
        assert_eq!(h.query, "щи", "typing is text, not commands");
    }

    #[test]
    fn filtering_resets_the_scroll() {
        let mut h = KeysHelp::new(Prefix::Off);
        h.key(key(KeyCode::End));
        assert!(h.offset > 0);
        h.key(key(KeyCode::Char('/')));
        h.key(key(KeyCode::Char('z')));
        assert_eq!(h.offset, 0);
    }

    #[test]
    fn a_click_outside_closes_and_the_wheel_scrolls() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut h = KeysHelp::new(Prefix::Off);
        let r = h.rect(screen);

        assert!(h.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), screen));
        assert!(!h.mouse(mouse(MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y + 1), screen));

        h.mouse(mouse(MouseEventKind::ScrollDown, r.x + 1, r.y + 1), screen);
        assert_eq!(h.offset, 3);
        h.mouse(mouse(MouseEventKind::ScrollUp, r.x + 1, r.y + 1), screen);
        assert_eq!(h.offset, 0);
    }

    #[test]
    fn the_first_rows_are_drawn_with_keys_and_descriptions() {
        let screen = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(screen);
        let h = KeysHelp::new(Prefix::Off);
        h.draw(&mut buf, screen, &theme());

        let inner = h.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let row = |y: u16| -> String {
            (inner.x..inner.right()).map(|x| buf[(x, y)].symbol()).collect()
        };
        let frame: String = (h.rect(screen).x..h.rect(screen).right()).map(|x| buf[(x, h.rect(screen).y)].symbol()).collect();
        assert!(frame.contains(" Keys and mouse "), "the title is in the frame: {frame:?}");
        assert!(row(inner.y).starts_with("/ search keys"));
        assert!(row(inner.y + 1).contains("⌥"), "{:?}", row(inner.y + 1));
        let first = row(inner.y + 2);
        assert!(first.contains("⌥+K") && first.contains("keys and mouse"), "{first:?}");
    }

    #[test]
    fn the_first_heading_is_whole_in_an_80_column_terminal() {
        let screen = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(screen);
        let h = KeysHelp::new(Prefix::Off);
        h.draw(&mut buf, screen, &theme());
        let inner = h.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let row: String = (inner.x..inner.right()).map(|x| buf[(x, inner.y + 1)].symbol()).collect();
        assert_eq!(row.trim_end(), heads(&h)[0], "the heading is cut at {} columns", inner.width);
    }

    #[test]
    fn tiny_screen_does_not_panic() {
        for (w, h) in [(1, 1), (4, 3), (12, 5), (80, 4)] {
            let screen = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(screen);
            let mut help = KeysHelp::new(Prefix::Off);
            help.key(key(KeyCode::End));
            help.draw(&mut buf, screen, &theme());
        }
    }
}
