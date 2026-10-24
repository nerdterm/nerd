//! "New space": a path field with live completion, backed by the filesystem and a recent-folders
//! list. A picker with an editable query and no fixed item set, so it isn't built on `menu::Picker`.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::render::Theme;

const ROWS_SHOWN: usize = 5;
/// ceiling: reads at most this many directory entries per keystroke, so a huge folder
/// cannot stall the server's loop; raise it if a real folder is routinely missed.
const MAX_READ: usize = 200;
const NO_SUCH_FOLDER: &str = "no such folder";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Recent,
    Folders,
}

#[derive(Clone)]
struct Row {
    path: PathBuf,
    display: String,
    branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Cancelled,
    Open(PathBuf),
}

/// A folder typed once, cycling through it on repeated Tab: the parent, the fragment Tab was
/// pressed on, the candidates found then, and which one is shown now.
/// `idx`: `None` while armed but not yet shown (the Tab that found the ambiguity is a no-op;
/// the next one shows the first candidate), `Some(i)` for the one currently shown.
type TabCycle = (PathBuf, String, Vec<String>, Option<usize>);

pub struct SpacePicker {
    value: String,
    /// The active workspace's folder: where a relative path resolves from.
    base: PathBuf,
    /// Computed once, when the dialog opens.
    recent: Vec<Row>,
    rows: Vec<Row>,
    section: Section,
    selected: Option<usize>,
    scroll: usize,
    hint: Option<String>,
    /// Position, and what was there, so a double click still opens it even if the first click's
    /// descend emptied or reshuffled the list under the same screen spot.
    last_click: Option<((u16, u16), PathBuf, std::time::Instant)>,
    tab_cycle: Option<TabCycle>,
}

pub const KEYS: &[(&str, &str, &str)] = &[
    ("tab", "", "complete the folder name; again to cycle through the matches"),
    ("↑↓", "", "choose a folder from the list"),
    ("enter", "", "open the chosen folder, or the typed one, as a space"),
    ("esc", "", "close"),
];

/// Double-click threshold, as in `mouse.rs` and the file tree.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(350);

impl SpacePicker {
    pub fn new(base: PathBuf) -> Self {
        let mut w = Self {
            value: start(),
            base,
            recent: recent_rows(),
            rows: Vec::new(),
            section: Section::Recent,
            selected: None,
            scroll: 0,
            hint: None,
            last_click: None,
            tab_cycle: None,
        };
        w.recompute();
        w
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Outcome {
        use crossterm::event::{KeyCode::*, KeyModifiers};
        match k.code {
            Esc => return Outcome::Cancelled,
            Enter => {
                return match self.selected.and_then(|i| self.rows.get(i)) {
                    Some(row) => Outcome::Open(row.path.clone()),
                    // Empty field, nothing selected: closes without opening anything, as before.
                    None if self.value.trim().is_empty() => Outcome::Cancelled,
                    None => Outcome::Open(crate::paths::resolve(&self.base, &self.value)),
                };
            }
            Tab => self.tab(),
            Up => self.step(-1),
            Down => self.step(1),
            Backspace => {
                self.value.pop();
                self.tab_cycle = None;
                self.recompute();
            }
            Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.push(c);
                self.tab_cycle = None;
                self.recompute();
            }
            _ => {}
        }
        Outcome::None
    }

    /// A pasted line, at the end as typing would put it.
    pub fn paste(&mut self, s: &str) {
        self.value.push_str(s);
        self.tab_cycle = None;
        self.recompute();
    }

    /// `rows`: where the last frame's `draw` put them.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, rows: &[Rect]) -> Outcome {
        use crossterm::event::MouseEventKind::*;
        match ev.kind {
            Down(_) => {
                let at = (ev.column, ev.row);
                if let Some(i) = rows.iter().position(|r| r.contains(at.into())) {
                    let idx = self.scroll + i;
                    // Checked by screen position, before the row lookup below: a repeat click at
                    // the same spot still opens what was there even if the first click's descend
                    // emptied or reshuffled the list under it.
                    let again = self.last_click.as_ref().is_some_and(|(pos, _, t)| *pos == at && t.elapsed() <= DOUBLE_CLICK);
                    if again {
                        let (_, path, _) = self.last_click.take().unwrap();
                        return Outcome::Open(path);
                    }
                    let Some(row) = self.rows.get(idx) else { return Outcome::None };
                    let path = row.path.clone();
                    self.last_click = Some((at, path, std::time::Instant::now()));
                    self.value = format!("{}/", row.display);
                    self.tab_cycle = None;
                    self.recompute();
                    return Outcome::None;
                }
                let r = self.rect(screen);
                if !r.contains(at.into()) || crate::chrome::close_rect(r).contains(at.into()) {
                    return Outcome::Cancelled;
                }
                Outcome::None
            }
            ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                Outcome::None
            }
            ScrollDown => {
                self.scroll = (self.scroll + 1).min(self.rows.len().saturating_sub(ROWS_SHOWN));
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    /// A fixed size, so it does not move while the list changes under it.
    pub fn rect(&self, screen: Rect) -> Rect {
        // the field, the hint, the divider, the rows
        crate::chrome::fit(screen, crate::chrome::WINDOW_W, ROWS_SHOWN as u16 + 3)
    }

    /// The field's caret, for the terminal cursor: always at the end of the typed text.
    pub fn cursor(&self, screen: Rect) -> (u16, u16) {
        let inner = self.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let w = (inner.width as usize).max(1);
        let len = self.value.chars().count();
        let col = len.min(w.saturating_sub(1));
        (inner.x + col as u16, inner.y)
    }

    /// Draws the dialog; returns the visible list rows' rects, in `scroll..` order.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Vec<Rect> {
        let r = self.rect(screen).intersection(buf.area);
        let mut out = Vec::new();
        if r.is_empty() {
            return out;
        }
        crate::chrome::dialog(buf, r, Some("New space"), theme.border_focus, theme, crate::chrome::Moat::All);
        let content = crate::chrome::close_button(buf, r, theme);
        let inner_w = content.width as usize;
        if inner_w == 0 || r.height < 4 {
            return out;
        }

        let mut y = content.y;
        let field: String = self.value.chars().chain(std::iter::repeat(' ')).take(inner_w).collect();
        buf.set_stringn(content.x, y, &field, inner_w, Style::default().fg(theme.text).bg(theme.ground));
        y += 1;

        if let Some(hint) = &self.hint {
            buf.set_stringn(content.x, y, hint, inner_w, Style::default().fg(theme.bad).bg(theme.ground));
        }
        y += 1;

        let label = match self.section {
            Section::Recent => "recent",
            Section::Folders => "folders",
        };
        let show_branch = self.rows.iter().any(|r| r.branch.is_some());
        let divider = section_divider(label, show_branch, inner_w);
        // the rule wears the border colour; the words in it stay readable
        for (i, ch) in divider.chars().enumerate() {
            let fg = if ch == '\u{2500}' { theme.border } else { theme.faint };
            buf.set_string(content.x + i as u16, y, ch.to_string(), Style::default().fg(fg).bg(theme.ground));
        }
        y += 1;

        let rows_room = content.bottom().saturating_sub(y) as usize;
        for i in 0..rows_room {
            let Some(row) = self.rows.get(self.scroll + i) else { break };
            let is_selected = self.selected == Some(self.scroll + i);
            let style = if is_selected { theme.cursor() } else { Style::default().fg(theme.text).bg(theme.ground) };
            // The branch keeps its place; a path too long for the rest loses its middle.
            let path = middle(&row.display, inner_w.saturating_sub(row.branch.as_ref().map_or(0, |b| b.chars().count() + 2)).max(1));
            let line: String = path.chars().chain(std::iter::repeat(' ')).take(inner_w).collect();
            buf.set_stringn(content.x, y, &line, inner_w, style);
            if let Some(branch) = &row.branch {
                let room = inner_w.saturating_sub(path.chars().count() + 2);
                if room > 0 {
                    let text: String = branch.chars().rev().take(room).collect::<Vec<_>>().into_iter().rev().collect();
                    let bstyle = if is_selected { style } else { Style::default().fg(theme.faint).bg(theme.ground) };
                    let x = content.x + inner_w as u16 - text.chars().count() as u16;
                    buf.set_stringn(x, y, &text, room, bstyle);
                }
            }
            out.push(Rect::new(content.x, y, inner_w as u16, 1));
            y += 1;
        }
        out
    }

    /// Empty, or the home folder as the field starts: the recent list; otherwise the typed parent's subfolders.
    fn recompute(&mut self) {
        self.selected = None;
        self.scroll = 0;
        if self.value.is_empty() || self.value == "~/" || self.value == start() {
            self.section = Section::Recent;
            self.rows = self.recent.clone();
            self.hint = None;
            return;
        }
        self.section = Section::Folders;
        let (parent, fragment) = self.split();
        let (prefix, substring) = list_dir(&parent, &fragment);
        let has_prefix_match = !prefix.is_empty();
        self.rows = prefix
            .into_iter()
            .chain(substring)
            .map(|name| {
                let path = parent.join(&name);
                Row { branch: crate::chrome::git_branch(&path), display: crate::paths::shown(&path), path }
            })
            .collect();
        self.hint = hint(&parent, &fragment, has_prefix_match);
    }

    /// The typed value split into the directory to list and the fragment being completed.
    /// Split on the last `/` in the typed text itself, not via `Path::file_name`: a `Path`
    /// normalizes away a trailing `.` component, which would otherwise swallow the very
    /// fragment a leading dot is meant to type.
    fn split(&self) -> (PathBuf, String) {
        let value = self.value.trim();
        if value.is_empty() || value.ends_with('/') {
            return (crate::paths::resolve(&self.base, value), String::new());
        }
        let Some((dir, fragment)) = value.rsplit_once('/') else {
            return (self.base.clone(), value.to_string());
        };
        let parent = if dir.is_empty() {
            PathBuf::from("/")
        } else if dir == "~" {
            crate::paths::home().unwrap_or_default()
        } else {
            crate::paths::resolve(&self.base, dir)
        };
        (parent, fragment.to_string())
    }

    fn step(&mut self, delta: isize) {
        let n = self.rows.len();
        if n == 0 {
            self.selected = None;
            return;
        }
        let cur = self.selected.map(|i| i as isize).unwrap_or(if delta > 0 { -1 } else { 0 });
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.selected = Some(next);
        if next < self.scroll {
            self.scroll = next;
        } else if next >= self.scroll + ROWS_SHOWN {
            self.scroll = next + 1 - ROWS_SHOWN;
        }
    }

    /// Common-prefix completion on the first Tab; a single match also adds the trailing `/`;
    /// an unresolvable ambiguity starts cycling through the candidates on the Tabs after.
    fn tab(&mut self) {
        if let Some((parent, fragment, matches, idx)) = self.tab_cycle.take() {
            let next = idx.map_or(0, |i| (i + 1) % matches.len());
            self.value = join_display(&parent, &matches[next]);
            self.tab_cycle = Some((parent, fragment, matches, Some(next)));
            self.recompute();
            return;
        }
        let (parent, fragment) = self.split();
        let (prefix, _) = list_dir(&parent, &fragment);
        if prefix.is_empty() {
            return;
        }
        if prefix.len() == 1 {
            self.value = format!("{}/", join_display(&parent, &prefix[0]));
            self.recompute();
            return;
        }
        let lcp = common_prefix(&prefix);
        if lcp.chars().count() > fragment.chars().count() {
            self.value = join_display(&parent, &lcp);
            self.recompute();
            return;
        }
        // Already at the longest common prefix, still ambiguous: this Tab arms cycling
        // without changing anything visible; the next one shows the first candidate.
        self.tab_cycle = Some((parent, fragment, prefix, None));
    }
}

/// The "no such folder" hint: the parent doesn't exist, or the full path is neither an
/// existing folder nor a prefix of one (a substring-only match doesn't count).
fn hint(parent: &Path, fragment: &str, has_prefix_match: bool) -> Option<String> {
    if !parent.is_dir() {
        return Some(NO_SUCH_FOLDER.into());
    }
    if fragment.is_empty() || parent.join(fragment).is_dir() || has_prefix_match {
        return None;
    }
    Some(NO_SUCH_FOLDER.into())
}

/// Subfolders of `parent` matching `fragment`: names starting with it, then names containing it.
/// Hidden folders only when `fragment` itself starts with `.`. An unreadable folder is empty,
/// not an error.
fn list_dir(parent: &Path, fragment: &str) -> (Vec<String>, Vec<String>) {
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    let hidden_ok = fragment.starts_with('.');
    if let Ok(rd) = std::fs::read_dir(parent) {
        for entry in rd.flatten().take(MAX_READ) {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !hidden_ok && name.starts_with('.') {
                continue;
            }
            if fragment.is_empty() || name.starts_with(fragment) {
                prefix.push(name);
            } else if name.contains(fragment) {
                substring.push(name);
            }
        }
    }
    prefix.sort();
    substring.sort();
    (prefix, substring)
}

fn common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else { return String::new() };
    let mut prefix: Vec<char> = first.chars().collect();
    for name in &names[1..] {
        let n = prefix.iter().zip(name.chars()).take_while(|(a, b)| **a == *b).count();
        prefix.truncate(n);
    }
    prefix.into_iter().collect()
}

/// The field's first value: the home folder, as the `paths` setting shows it.
fn start() -> String {
    let home = crate::paths::shown(&crate::config::home_dir());
    if home.ends_with('/') { home } else { format!("{home}/") }
}

/// `s` in at most `max` characters, its middle replaced by `…` when cut.
fn middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head = max.saturating_sub(1) / 2;
    let tail = max.saturating_sub(1 + head);
    s.chars().take(head).chain(['…']).chain(s.chars().skip(n - tail)).collect()
}

fn join_display(parent: &Path, name: &str) -> String {
    crate::paths::shown(&parent.join(name))
}

fn recent_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = crate::session::recent_spaces()
        .into_iter()
        .map(|path| Row { branch: crate::chrome::git_branch(&path), display: crate::paths::shown(&path), path })
        .collect();
    rows.sort_by(|a, b| a.display.cmp(&b.display));
    rows
}

/// `─ recent ──────── branch ─`: the section's name, with a `branch` column header only
/// when at least one row in it has one.
fn section_divider(label: &str, show_branch: bool, width: usize) -> String {
    let left = format!("\u{2500} {label} ");
    let right = if show_branch { " branch \u{2500}".to_string() } else { "\u{2500}".to_string() };
    let (left_len, right_len) = (left.chars().count(), right.chars().count());
    if left_len + right_len >= width {
        return left.chars().chain(right.chars()).take(width).collect();
    }
    format!("{left}{}{right}", "\u{2500}".repeat(width - left_len - right_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    /// `paths::home()` accepts an override only under `temp_dir()`, so tests never touch the real HOME.
    fn with_temp_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let dir = crate::paths::test_dir("space-picker-test");
        let old = crate::paths::TEST_HOME.with(|h| h.replace(Some(dir.clone())));
        let result = f(&dir);
        crate::paths::TEST_HOME.with(|h| h.replace(old));
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(w: &mut SpacePicker, s: &str) {
        for c in s.chars() {
            w.key(key(KeyCode::Char(c)));
        }
    }

    fn init_git(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    }

    #[test]
    fn the_window_has_a_fixed_size_and_a_long_path_loses_its_middle() {
        with_temp_home(|home| {
            let mut w = SpacePicker::new(home.to_path_buf());
            let row = |display: &str, branch: Option<&str>| Row { path: PathBuf::from(display), display: display.into(), branch: branch.map(Into::into) };
            w.recent = vec![row("~/dev/a", None), row(&format!("~/{}", "d".repeat(60)), Some("main"))];
            w.recompute();
            let (big, small) = (Rect::new(0, 0, 200, 60), Rect::new(0, 0, 60, 20));
            let r = w.rect(big);
            assert_eq!((r.width, r.height), (crate::chrome::WINDOW_W, 5 + 3 + 4), "the field, hint, rule and five rows");
            let mut buf = Buffer::empty(big);
            let rows = w.draw(&mut buf, big, &Theme::dark());
            let line: String = (rows[1].x..rows[1].right()).map(|x| buf[(x, rows[1].y)].symbol()).collect();
            assert_eq!(line, format!("~/{}…{}  main", "d".repeat(26), "d".repeat(29)), "the branch whole, the path cut in its middle");
            type_str(&mut w, "dev/");
            assert_eq!(w.rect(big), r, "typing does not move it");
            assert!(w.rect(small).width <= 54, "capped by the margins");
            let mut buf = Buffer::empty(big);
            w.draw(&mut buf, big, &Theme::dark());
            let top: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
            assert!(top.contains(" New space "), "{top:?}");
        });
    }

    #[test]
    fn listing_lists_folders_prefix_before_substring_and_hides_dotfolders() {
        with_temp_home(|home| {
            let dev = home.join("dev");
            for name in ["nerd", "nervous", "xnerd", ".nerd-hidden"] {
                std::fs::create_dir_all(dev.join(name)).unwrap();
            }
            std::fs::write(dev.join("file.txt"), "not a folder").unwrap();

            let names = |w: &SpacePicker| -> Vec<String> { w.rows.iter().map(|r| r.display.rsplit('/').next().unwrap().to_string()).collect() };

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/ner");
            assert_eq!(names(&w), vec!["nerd", "nervous", "xnerd"], "prefix matches (starting with \"ner\") first, then substring (\"xnerd\"), each sorted; hidden and the plain file excluded");

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/.");
            assert_eq!(names(&w), vec![".nerd-hidden"], "typing a leading dot reveals it");

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/");
            assert_eq!(names(&w), vec!["nerd", "nervous", "xnerd"], "an empty fragment lists folders only, still hiding dotfolders");
        });
    }

    #[test]
    fn tab_completes_the_common_prefix_then_a_single_match_then_cycles() {
        with_temp_home(|home| {
            let dev = home.join("dev");
            std::fs::create_dir_all(dev.join("nerd")).unwrap();
            std::fs::create_dir_all(dev.join("near")).unwrap();

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/ne");
            w.key(key(KeyCode::Tab));
            assert_eq!(w.value, "~/dev/ne", "already the common prefix of both: nothing to extend");

            w.key(key(KeyCode::Tab));
            let first = w.value.clone();
            assert!(first == "~/dev/near" || first == "~/dev/nerd", "cycling lands on a candidate: {first}");
            w.key(key(KeyCode::Tab));
            let second = w.value.clone();
            assert_ne!(first, second, "a second Tab cycles to the other match");

            w.value = "~/dev/ner".to_string();
            w.tab_cycle = None;
            w.recompute();
            w.key(key(KeyCode::Tab));
            assert_eq!(w.value, "~/dev/nerd/", "a single match completes and adds the trailing slash");
        });
    }

    #[test]
    fn the_hint_appears_only_when_the_typed_path_cannot_lead_anywhere() {
        with_temp_home(|home| {
            std::fs::create_dir_all(home.join("dev/nerd")).unwrap();

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/ner");
            assert_eq!(w.hint, None, "a prefix match exists");

            w.value = "~/dev/".to_string();
            w.recompute();
            assert_eq!(w.hint, None, "an existing folder with nothing typed after it");

            w.value = "~/dev/zzz".to_string();
            w.recompute();
            assert_eq!(w.hint.as_deref(), Some(NO_SUCH_FOLDER), "no folder starts or is named that");

            w.value = "~/nope/zzz".to_string();
            w.recompute();
            assert_eq!(w.hint.as_deref(), Some(NO_SUCH_FOLDER), "the parent itself does not exist");
        });
    }

    #[test]
    fn a_click_descends_and_enter_opens_the_selection() {
        with_temp_home(|home| {
            std::fs::create_dir_all(home.join("dev/nerd")).unwrap();
            std::fs::create_dir_all(home.join("dev/nerd/src")).unwrap();

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/nerd");
            let screen = Rect::new(0, 0, 80, 24);
            let mut buf = Buffer::empty(screen);
            let theme = Theme::dark();
            let rows = w.draw(&mut buf, screen, &theme);
            assert_eq!(rows.len(), 1, "one match: nerd");

            let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: rows[0].x, row: rows[0].y, modifiers: KeyModifiers::NONE };
            assert_eq!(w.mouse(click, screen, &rows), Outcome::None, "a single click descends");
            assert_eq!(w.value, "~/dev/nerd/");
            assert!(w.rows.iter().any(|r| r.display.ends_with("/src")), "now listing inside it");

            w.step(1);
            let want = w.rows[w.selected.unwrap()].path.clone();
            assert_eq!(w.key(key(KeyCode::Enter)), Outcome::Open(want), "Enter opens the selected row");
        });
    }

    #[test]
    fn esc_cancels_and_a_double_click_opens() {
        with_temp_home(|home| {
            // A leaf: descending into it empties the list, which must not stop the second click.
            std::fs::create_dir_all(home.join("dev/nerd")).unwrap();
            let mut w = SpacePicker::new(home.to_path_buf());
            assert_eq!(w.key(key(KeyCode::Esc)), Outcome::Cancelled);

            type_str(&mut w, "dev/nerd");
            let screen = Rect::new(0, 0, 80, 24);
            let mut buf = Buffer::empty(screen);
            let rows = w.draw(&mut buf, screen, &Theme::dark());
            assert_eq!(rows.len(), 1);
            let want = w.rows[0].path.clone();
            let at = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: rows[0].x, row: rows[0].y, modifiers: KeyModifiers::NONE };

            assert_eq!(w.mouse(at, screen, &rows), Outcome::None, "the first click descends");
            assert!(w.rows.is_empty(), "the leaf folder has no subfolders to show");
            assert_eq!(w.mouse(at, screen, &rows), Outcome::Open(want), "the second click still opens what was clicked");
        });
    }

    #[test]
    fn the_branch_header_appears_only_with_a_git_row() {
        with_temp_home(|home| {
            std::fs::create_dir_all(home.join("dev/plain")).unwrap();
            init_git(&home.join("dev/repo"), "feat/x");

            let mut w = SpacePicker::new(home.to_path_buf());
            type_str(&mut w, "dev/plain");
            let screen = Rect::new(0, 0, 80, 24);
            let mut buf = Buffer::empty(screen);
            let theme = Theme::dark();
            w.draw(&mut buf, screen, &theme);
            // top border, field row, the hint's reserved row, then the section divider
            let mut divider_y = w.rect(screen).y + 3;
            let line: String = (0..screen.width).map(|x| buf[(x, divider_y)].symbol().to_string()).collect();
            assert!(!line.contains("branch"), "no git row in this list: {line:?}");

            w.value = "~/dev/".to_string();
            w.recompute();
            let mut buf = Buffer::empty(screen);
            w.draw(&mut buf, screen, &theme);
            divider_y = w.rect(screen).y + 3;
            let line: String = (0..screen.width).map(|x| buf[(x, divider_y)].symbol().to_string()).collect();
            assert!(line.contains("branch"), "a git row is listed: {line:?}");
        });
    }
}
