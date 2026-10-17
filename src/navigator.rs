//! "Jump to pane" overlay: a spaces -> tabs -> panes tree, full screen.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::layout::PaneId;
use crate::render::Theme;
use crate::term::AgentState;

#[derive(Debug, Clone)]
pub struct Entry {
    pub space: usize,
    pub tab: usize,
    pub pane: PaneId,
    pub space_name: String,
    pub tab_label: String,
    pub title: String, // agent or command name
    pub state: AgentState,
    pub is_current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Blocked,
    Working,
    Idle,
    Done,
}

pub enum Outcome {
    None,
    Cancelled,
    Open(Entry),
}

pub struct Navigator {
    pub entries: Vec<Entry>,
    /// Index into `visible()`, not into `entries` - so moving doesn't need to skip headings.
    pub selected: usize,
    pub query: String,
    pub filter: Filter,
    /// While typing, printable keys go into `query`, not the filter hotkeys.
    searching: bool,
}

impl Navigator {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self {
            entries,
            selected: 0,
            query: String::new(),
            filter: Filter::All,
            searching: false,
        }
    }

    pub fn visible(&self) -> Vec<&Entry> {
        let q = self.query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| matches_filter(e.state, self.filter))
            .filter(|e| {
                q.is_empty()
                    || e.space_name.to_lowercase().contains(&q)
                    || e.tab_label.to_lowercase().contains(&q)
                    || e.title.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Fits every pane and its key strip, not the filtered ones, so it does not move while searching.
    pub fn rect(&self, screen: Rect) -> Rect {
        let spaces = 1 + self.entries.windows(2).filter(|w| w[0].space != w[1].space).count();
        let widest = self
            .entries
            .iter()
            .map(|e| (e.space_name.chars().count() + 2).max(6 + e.title.chars().count() + 3 + e.tab_label.chars().count()))
            .max()
            .unwrap_or(0) as u16;
        let strip = hint(crate::chrome::fit(screen, widest, 0).width.saturating_sub(4) as usize).len();
        // search, the space headers and panes, the key strip
        crate::chrome::fit(screen, widest, (1 + spaces + self.entries.len().max(1) + strip) as u16)
    }

    /// Draws the navigator; returns each pane row's rect and its index into `visible()`.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Vec<(Rect, usize)> {
        let mut rows = Vec::new();
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return rows;
        }
        crate::chrome::dialog(buf, r, Some("Navigator"), theme.border_focus, theme, crate::chrome::Moat::All);
        let inner = crate::chrome::close_button(buf, r, theme);
        if inner.width == 0 || inner.height == 0 {
            return rows;
        }
        let mut y = inner.y;

        let (text, style) = if self.query.is_empty() {
            ("/ search panes".to_string(), Style::default().fg(theme.faint))
        } else {
            (self.query.clone(), Style::default().fg(theme.text))
        };
        buf.set_stringn(inner.x, y, &text, inner.width as usize, style);
        // The count is of the whole list, not the filtered/searched slice.
        let n = self.entries.len();
        let count = format!("{n} pane{}", if n == 1 { "" } else { "s" });
        let cx = inner.right().saturating_sub(count.chars().count() as u16);
        if cx > inner.x + text.chars().count() as u16 {
            buf.set_stringn(cx, y, &count, inner.width as usize, Style::default().fg(theme.faint));
        }
        y += 1;

        let tree_bottom = crate::chrome::draw_hint(buf, inner, &hint(inner.width as usize), theme).max(y);

        let visible: Vec<Entry> = self.visible().into_iter().cloned().collect();
        if visible.is_empty() {
            if y < tree_bottom {
                buf.set_stringn(inner.x, y, " no panes", inner.width as usize, Style::default().fg(theme.faint));
            }
            return rows;
        }

        // A space's header, then its panes: (the pane's index, a header, its group's last, the group spans tabs).
        let mut lines: Vec<(usize, bool, bool, bool)> = Vec::new();
        let mut i = 0;
        while i < visible.len() {
            let mut j = i;
            while j < visible.len() && visible[j].space == visible[i].space {
                j += 1;
            }
            let multi_tab = visible[i..j].iter().map(|e| e.tab).collect::<HashSet<_>>().len() > 1;
            lines.push((i, true, false, multi_tab));
            lines.extend((i..j).map(|idx| (idx, false, idx + 1 == j, multi_tab)));
            i = j;
        }
        // A list longer than the window scrolls so the selected pane stays in view.
        let room = tree_bottom.saturating_sub(y) as usize;
        let at = lines.iter().position(|l| !l.1 && l.0 == self.selected).unwrap_or(0);
        let skip = (at + 1).saturating_sub(room);

        for &(idx, head, last, multi_tab) in lines.iter().skip(skip) {
            if y >= tree_bottom {
                break;
            }
            let e = &visible[idx];
            if head {
                buf.set_stringn(inner.x, y, format!("{} ▾", e.space_name), inner.width as usize, Style::default().fg(theme.text).add_modifier(Modifier::BOLD));
                y += 1;
                continue;
            }
            let selected = idx == self.selected;
            let connector = if last { "└── " } else { "├── " };
            // the branch starts under the space name's first letter
            let mut line = String::from(connector);
            let dot_col = line.chars().count() as u16;
            line.push(e.state.mark());
            line.push(' ');
            let title_col = line.chars().count() as u16;
            line.push_str(&e.title);
            if multi_tab {
                line.push_str(" · ");
                line.push_str(&e.tab_label);
            }

            let row = Rect::new(inner.x, y, inner.width, 1);
            let padded: String = line.chars().chain(std::iter::repeat(' ')).take(inner.width as usize).collect();
            buf.set_stringn(inner.x, y, &padded, inner.width as usize, Style::default().fg(theme.text).bg(theme.ground));
            if dot_col < inner.width {
                let dot_style = Style::default().fg(crate::sidebar::state_color(e.state, theme));
                buf.set_stringn(inner.x + dot_col, y, e.state.mark().to_string(), 1, dot_style);
            }
            if e.is_current && title_col < inner.width {
                let w = (inner.width - title_col) as usize;
                buf.set_stringn(inner.x + title_col, y, &e.title, w, theme.chosen());
            }
            if selected {
                buf.set_style(row, theme.cursor());
            }

            rows.push((row, idx));
            y += 1;
        }
        rows
    }

}

fn hint(width: usize) -> Vec<String> {
    crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), width)
}

/// One key the navigator answers. The handler dispatches on these rows and the strip prints
/// them, so the two cannot describe different things.
pub struct Key {
    pub code: crossterm::event::KeyCode,
    pub label: &'static str,
    pub word: &'static str,
    pub what: &'static str,
    act: fn(&mut Navigator, crossterm::event::KeyCode) -> Outcome,
}

pub const KEYS: &[Key] = &[
    Key { code: crossterm::event::KeyCode::Up, label: "↑↓", word: "", what: "up and down the list",
          act: |n, _| { n.step(-1); Outcome::None } },
    Key { code: crossterm::event::KeyCode::Down, label: "", word: "", what: "up and down the list",
          act: |n, _| { n.step(1); Outcome::None } },
    Key { code: crossterm::event::KeyCode::Enter, label: "enter", word: "open", what: "go to the selected pane",
          act: |n, _| n.open_selected() },
    Key { code: crossterm::event::KeyCode::Char('/'), label: "/", word: "search", what: "search by space, tab or name",
          act: |n, _| { n.searching = true; Outcome::None } },
    Key { code: crossterm::event::KeyCode::Char('a'), label: "a", word: "all", what: "show every pane",
          act: |n, _| n.set_filter(Filter::All) },
    Key { code: crossterm::event::KeyCode::Char('b'), label: "b", word: "blocked", what: "only the ones waiting for you",
          act: |n, _| n.set_filter(Filter::Blocked) },
    Key { code: crossterm::event::KeyCode::Char('w'), label: "w", word: "working", what: "only the ones working",
          act: |n, _| n.set_filter(Filter::Working) },
    Key { code: crossterm::event::KeyCode::Char('i'), label: "i", word: "idle", what: "only the idle ones",
          act: |n, _| n.set_filter(Filter::Idle) },
    Key { code: crossterm::event::KeyCode::Char('d'), label: "d", word: "done", what: "only the finished ones",
          act: |n, _| n.set_filter(Filter::Done) },
    Key { code: crossterm::event::KeyCode::Esc, label: "esc", word: "close", what: "clear the search, then close",
          act: |n, _| {
              if n.query.is_empty() {
                  return Outcome::Cancelled;
              }
              n.query.clear();
              n.clamp_selected();
              Outcome::None
          } },
];

impl Navigator {
    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Outcome {
        use crossterm::event::KeyCode::*;

        // A command is the letter on the key; a query is what the layout printed. Normalising
        // here and not while typing is what lets a Russian keyboard do both.
        let k = if self.searching { k } else { crate::keys::normalize(k) };
        if self.searching {
            match k.code {
                Esc => {
                    self.searching = false;
                    self.query.clear();
                    self.clamp_selected();
                }
                Enter => return self.open_selected(),
                Backspace => {
                    self.query.pop();
                    self.clamp_selected();
                }
                Char(c) => {
                    self.query.push(c);
                    self.clamp_selected();
                }
                _ => {}
            }
            return Outcome::None;
        }

        if let Char(c @ '1'..='9') = k.code {
            return self.open_at(c as usize - '1' as usize);
        }
        KEYS.iter().find(|r| r.code == k.code).map_or(Outcome::None, |r| (r.act)(self, k.code))
    }

    /// `rows`: where the last frame's `draw` put them.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, rows: &[(Rect, usize)]) -> Outcome {
        use crossterm::event::MouseEventKind::*;
        let r = self.rect(screen);
        let at = row_at(rows, ev.column, ev.row);
        match ev.kind {
            Moved => {
                if let Some(i) = at {
                    self.selected = i;
                }
                Outcome::None
            }
            Down(_) => {
                if !r.contains((ev.column, ev.row).into()) || crate::chrome::close_rect(r).contains((ev.column, ev.row).into()) {
                    return Outcome::Cancelled;
                }
                match at {
                    Some(i) => {
                        self.selected = i;
                        self.open_at(i)
                    }
                    None => Outcome::None,
                }
            }
            ScrollUp => {
                self.step(-3);
                Outcome::None
            }
            ScrollDown => {
                self.step(3);
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    fn set_filter(&mut self, f: Filter) -> Outcome {
        self.filter = f;
        self.clamp_selected();
        Outcome::None
    }

    fn step(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let i = (self.selected as isize + delta).rem_euclid(n as isize);
        self.selected = i as usize;
    }

    fn clamp_selected(&mut self) {
        let n = self.visible().len();
        self.selected = if n == 0 { 0 } else { self.selected.min(n - 1) };
    }

    fn open_selected(&self) -> Outcome {
        self.open_at(self.selected)
    }

    fn open_at(&self, i: usize) -> Outcome {
        match self.visible().get(i) {
            Some(e) => Outcome::Open((*e).clone()),
            None => Outcome::None,
        }
    }

}

/// `Unknown` matches only `All` - a fresh pane is neither blocked nor working.
fn matches_filter(state: AgentState, filter: Filter) -> bool {
    match filter {
        Filter::All => true,
        Filter::Blocked => matches!(state, AgentState::Blocked | AgentState::Failed),
        Filter::Working => matches!(state, AgentState::Working),
        Filter::Idle => matches!(state, AgentState::Idle),
        Filter::Done => matches!(state, AgentState::Done),
    }
}

/// The row drawn at a cell, by its index.
fn row_at(rows: &[(Rect, usize)], col: u16, row: u16) -> Option<usize> {
    rows.iter().find(|(rect, _)| rect.contains((col, row).into())).map(|(_, i)| *i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn theme() -> Theme {
        Theme::dark()
    }

    fn entry(space: usize, tab: usize, pane: PaneId, space_name: &str, title: &str, state: AgentState, current: bool) -> Entry {
        Entry {
            space,
            tab,
            pane,
            space_name: space_name.into(),
            tab_label: (tab + 1).to_string(),
            title: title.into(),
            state,
            is_current: current,
        }
    }

    /// api (claude working, bun idle/current) + web (claude blocked).
    fn fixture() -> Vec<Entry> {
        vec![
            entry(0, 0, 1, "api", "claude", AgentState::Working, false),
            entry(0, 1, 2, "api", "bun", AgentState::Idle, true),
            entry(1, 0, 3, "web", "claude", AgentState::Blocked, false),
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    fn text_at(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
        (x..x + len).map(|i| buf[(i, y)].symbol()).collect()
    }

    fn is_open(o: Outcome, pane: PaneId) -> bool {
        matches!(o, Outcome::Open(e) if e.pane == pane)
    }

    #[test]
    fn counter_shows_the_total_pane_count() {
        let screen = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(screen);
        let nav = Navigator::new(fixture());
        nav.draw(&mut buf, screen, &theme());

        let r = nav.rect(screen);
        let inner = r.inner(crate::chrome::DIALOG_FRAME);
        assert!(text_at(&buf, inner.x, inner.y, inner.width).trim_end().ends_with("3 panes"));
    }

    #[test]
    fn a_branch_starts_under_its_space_name_first_letter() {
        let screen = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(screen);
        let nav = Navigator::new(fixture());
        nav.draw(&mut buf, screen, &theme());
        let inner = nav.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let rows: Vec<String> = (inner.y..inner.bottom()).map(|y| text_at(&buf, inner.x, y, inner.width)).collect();
        let head = rows.iter().position(|r| r.ends_with(" ▾") || r.trim_end().ends_with('▾')).expect("a space header");
        assert!(!rows[head].starts_with(' '), "{:?}", rows[head]);
        assert!(rows[head + 1].starts_with('├') || rows[head + 1].starts_with('└'), "{:?}", rows[head + 1]);
    }

    #[test]
    fn the_window_fits_its_rows_and_does_not_move_while_searching() {
        let screen = Rect::new(0, 0, 200, 50);
        let mut nav = Navigator::new(fixture());
        let r = nav.rect(screen);
        let spaces = 1 + nav.entries.windows(2).filter(|w| w[0].space != w[1].space).count() as u16;
        let hint = super::hint(r.width as usize - 4);
        assert_eq!(r.height, 2 + 1 + 2 + spaces + nav.entries.len() as u16 + hint.len() as u16, "frame, search, rows, hint, close button");
        assert!(r.width < 60 && hint.len() > 1 && hint.iter().all(|l| l.chars().count() <= r.width as usize - 4), "the rows set the width, the key strip wraps: {r:?} {hint:?}");
        assert_eq!((r.x, r.y), ((200 - r.width) / 2, (50 - r.height) / 2), "centred");
        nav.query = "zzz".into();
        assert_eq!(nav.rect(screen), r);
        let small = Rect::new(0, 0, 60, 10);
        let r = nav.rect(small);
        assert!(r.height == 8 && r.width <= 54 && r.x >= 3, "capped by the old margins: {r:?}");
    }

    #[test]
    fn a_list_longer_than_the_window_scrolls_with_the_selection() {
        let entries: Vec<Entry> = (0..30).map(|n| entry(n / 5, 0, n + 1, &format!("space-{}", n / 5), &format!("pane-{n}"), AgentState::Idle, false)).collect();
        let mut nav = Navigator::new(entries);
        let screen = Rect::new(0, 0, 80, 24);
        for step in 0..30 {
            let mut buf = Buffer::empty(screen);
            let rows = nav.draw(&mut buf, screen, &theme());
            assert!(rows.iter().any(|(_, i)| *i == nav.selected), "step {step}: pane {} is drawn", nav.selected);
            nav.key(key(KeyCode::Down));
        }
        assert_eq!(nav.selected, 0, "down from the last wraps to the first");
    }

    #[test]
    fn filter_blocked_hides_non_blocked() {
        let mut nav = Navigator::new(fixture());
        nav.filter = Filter::Blocked;
        let v = nav.visible();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pane, 3);
    }

    #[test]
    fn search_narrows_by_title_case_insensitively() {
        let mut nav = Navigator::new(fixture());
        nav.query = "CLA".into();
        let v = nav.visible();
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|e| e.title == "claude"));
    }

    #[test]
    fn arrows_move_among_visible_rows_and_wrap() {
        let mut nav = Navigator::new(fixture());
        assert_eq!(nav.selected, 0);
        nav.key(key(KeyCode::Down));
        assert_eq!(nav.selected, 1);
        nav.key(key(KeyCode::Down));
        assert_eq!(nav.selected, 2);
        nav.key(key(KeyCode::Down));
        assert_eq!(nav.selected, 0, "wraps at the bottom");
        nav.key(key(KeyCode::Up));
        assert_eq!(nav.selected, 2, "wraps at the top");
    }

    #[test]
    fn enter_opens_the_selected_entry() {
        let mut nav = Navigator::new(fixture());
        nav.selected = 1;
        assert!(is_open(nav.key(key(KeyCode::Enter)), 2));
    }

    #[test]
    fn esc_clears_the_query_before_cancelling() {
        let mut nav = Navigator::new(fixture());
        nav.key(key(KeyCode::Char('/')));
        nav.key(key(KeyCode::Char('x')));
        assert_eq!(nav.query, "x");

        assert!(matches!(nav.key(key(KeyCode::Esc)), Outcome::None));
        assert_eq!(nav.query, "");
        assert!(matches!(nav.key(key(KeyCode::Esc)), Outcome::Cancelled));
    }

    #[test]
    fn a_digit_opens_the_nth_visible_pane() {
        let mut nav = Navigator::new(fixture());
        assert!(is_open(nav.key(key(KeyCode::Char('2'))), 2));
    }

    #[test]
    fn mouse_click_on_a_row_opens_it_and_outside_cancels() {
        let screen = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(screen);
        let mut nav = Navigator::new(fixture());
        let rows = nav.draw(&mut buf, screen, &theme());

        let (row_rect, idx) = rows[1];
        assert_eq!(idx, 1);
        let click = mouse(MouseEventKind::Down(MouseButton::Left), row_rect.x, row_rect.y);
        assert!(is_open(nav.mouse(click, screen, &rows), 2));

        assert!(matches!(
            nav.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), screen, &rows),
            Outcome::Cancelled
        ));
    }

    #[test]
    fn the_current_pane_is_chosen_and_the_cursor_covers_its_row() {
        let screen = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(screen);
        let nav = Navigator::new(fixture());
        let th = theme();
        let rows = nav.draw(&mut buf, screen, &th);

        let (bun, _) = rows[1];
        let row: String = (bun.x..bun.right()).map(|x| buf[(x, bun.y)].symbol()).collect();
        assert!(!row.contains('◆'), "no glyph: the weight says it, {row:?}");
        let at = bun.x + row.find("bun").map(|b| row[..b].chars().count()).unwrap() as u16;
        assert_eq!(buf[(at, bun.y)].fg, th.accent);
        assert!(buf[(at, bun.y)].modifier.contains(Modifier::BOLD), "the current pane is chosen");

        let (cur, _) = rows[0];
        assert!((cur.x..cur.right()).all(|x| buf[(x, cur.y)].fg == th.ground && buf[(x, cur.y)].bg == th.accent),
            "the cursor repaints its whole row, the state dot included");
    }

    #[test]
    fn empty_list_and_tiny_screen_do_not_panic() {
        let nav = Navigator::new(Vec::new());
        let screen = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(screen);
        nav.draw(&mut buf, screen, &theme());

        let tiny = Rect::new(0, 0, 1, 1);
        let mut tiny_buf = Buffer::empty(tiny);
        let nav = Navigator::new(fixture());
        nav.draw(&mut tiny_buf, tiny, &theme());
    }
}
