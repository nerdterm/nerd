use super::*;

/// Where a key belongs when the strip groups them: moving the selection, going somewhere,
/// or doing something to what is selected.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Move,
    Go,
    Do,
}

/// One key the tree answers: what it is called, what it does, and the code that reaches it.
/// The handler looks a key up here, and the strip and the keys screen print these same rows,
/// so the tree cannot answer a key it never mentions or promise one it does not answer.
pub struct Key {
    pub code: KeyCode,
    /// Held with `option`: a bare letter types into the filter instead.
    pub alt: bool,
    /// Empty when the row is a second code for the row above it, so the strip prints it once.
    pub label: &'static str,
    /// One word for the strip at the bottom, where there is no room for a sentence. Empty
    /// where the key needs none: nobody has to be told what an arrow does.
    pub word: &'static str,
    /// The sentence the keys screen prints.
    pub what: &'static str,
    pub group: Group,
    act: fn(&mut Tree, &TreeView, KeyCode) -> Outcome,
}

/// The keys the tree answers while it holds them and nothing is being typed.
pub const COMMAND: &[Key] = &[
    Key { code: Up, alt: false, label: "↑↓", word: "", what: "move the selection", group: Group::Move,
          act: |t, _, _| { t.step(-1); Outcome::None } },
    Key { code: Down, alt: false, label: "", word: "", what: "move the selection", group: Group::Move,
          act: |t, _, _| { t.step(1); Outcome::None } },
    Key { code: PageUp, alt: false, label: "PgUp PgDn", word: "", what: "a screen at a time", group: Group::Move,
          act: |t, v, _| { let p = v.page(); t.step(-p); Outcome::None } },
    Key { code: PageDown, alt: false, label: "", word: "", what: "a screen at a time", group: Group::Move,
          act: |t, v, _| { let p = v.page(); t.step(p); Outcome::None } },
    Key { code: Home, alt: false, label: "Home End", word: "", what: "the first and the last row", group: Group::Move,
          act: |t, _, _| { t.selected = 0; Outcome::None } },
    Key { code: End, alt: false, label: "", word: "", what: "the first and the last row", group: Group::Move,
          act: |t, _, _| { t.selected = t.rows.len().saturating_sub(1); Outcome::None } },
    // Typing is the fast way in, so the letters that used to act now live on `option`. The
    // filter is not dispatched from this table - nothing else can stand for "any character".
    Key { code: Null, alt: false, label: "a-z", word: "filter", what: "type to filter by name", group: Group::Go,
          act: |_, _, _| Outcome::None },
    Key { code: Enter, alt: false, label: "enter", word: "open", what: "open a directory, or the file in the editor", group: Group::Go,
          act: |t, _, _| if t.on_up() { t.go_up(); Outcome::None } else if t.on_more() { t.show_all(t.selected); Outcome::None } else { match t.sel() {
              Some(r) if r.is_dir => { t.toggle(t.selected); Outcome::None }
              Some(r) => Outcome::OpenInEditor(r.path, r.line),
              None => Outcome::None,
          } } },
    Key { code: Right, alt: false, label: "→", word: "expand", what: "expand a directory", group: Group::Go,
          act: |t, _, _| { if t.on_up() { t.go_up(); } else if t.on_more() { t.show_all(t.selected); } else if t.sel().is_some_and(|r| r.is_dir) { t.toggle(t.selected); } Outcome::None } },
    Key { code: Left, alt: false, label: "←", word: "up", what: "collapse, or go to the parent", group: Group::Go,
          act: |t, _, _| { match t.sel() {
              Some(r) if r.open => t.toggle(t.selected),
              _ => t.go_parent(),
          } Outcome::None } },
    Key { code: Char('='), alt: true, label: "⌥+=", word: "root", what: "make the selected directory the root", group: Group::Go,
          act: |t, _, _| { if let Some(r) = t.sel().filter(|r| r.is_dir) { t.goto_root(r.path); } Outcome::None } },
    Key { code: Char('d'), alt: true, label: "⌥+D", word: "cd", what: "go to this folder in the pane", group: Group::Go,
          act: |t, _, _| Outcome::Cd(t.sel_dir()) },
    Key { code: Char('p'), alt: true, label: "⌥+P", word: "space", what: "open this folder as a space", group: Group::Go,
          act: |t, _, _| Outcome::NewSpace(t.sel_dir()) },
    Key { code: Tab, alt: false, label: "Tab", word: "preview", what: "into the preview and back", group: Group::Go,
          act: |t, _, _| { if t.preview_on { t.focus = Focus::Preview; } Outcome::None } },
    Key { code: BackTab, alt: false, label: "", word: "preview", what: "into the preview and back", group: Group::Go,
          act: |t, _, _| { if t.preview_on { t.focus = Focus::Preview; } Outcome::None } },
    Key { code: Backspace, alt: false, label: "⌫", word: "up", what: "up one directory, once the filter is empty", group: Group::Go,
          act: |t, _, _| { t.go_up(); Outcome::None } },
    Key { code: Esc, alt: false, label: "esc", word: "shell", what: "leave a filter or a grep, then hand the keys back to the pane", group: Group::Go,
          act: |t, _, _| { if t.mode == Mode::Tree { return Outcome::Close } t.leave_query(); Outcome::None } },
    Key { code: Char('e'), alt: true, label: "⌥+E", word: "edit", what: "edit the file", group: Group::Do,
          act: |t, _, _| match t.sel().filter(|r| !r.is_dir) {
              Some(r) => Outcome::OpenInEditor(r.path, r.line),
              None => Outcome::None,
          } },
    Key { code: Char('i'), alt: true, label: "⌥+I", word: "insert", what: "insert its path into the pane", group: Group::Do,
          act: |t, _, _| match t.sel() { Some(r) => Outcome::InsertPath(r.path), None => Outcome::None } },
    Key { code: Char('j'), alt: true, label: "⌥+J", word: "open", what: "open in its app, or a folder in Finder", group: Group::Do,
          act: |t, _, _| match t.sel() { Some(r) => Outcome::OpenSystem(r.path), None => Outcome::None } },
    Key { code: Char('y'), alt: true, label: "⌥+Y", word: "copy", what: "copy its path", group: Group::Do,
          act: |t, _, _| match t.sel() { Some(r) => Outcome::CopyPath(r.path), None => Outcome::None } },
    Key { code: Char('a'), alt: true, label: "⌥+A", word: "new", what: "new file here", group: Group::Do,
          act: |t, _, _| Outcome::AskCreate(t.sel_dir()) },
    Key { code: Char('l'), alt: true, label: "⌥+L", word: "rename", what: "rename it", group: Group::Do,
          act: |t, _, _| match t.sel() { Some(r) => Outcome::AskRename(r.path), None => Outcome::None } },
    Key { code: Char('x'), alt: true, label: "⌥+X", word: "delete", what: "delete it", group: Group::Do,
          act: |t, _, _| match t.sel() { Some(r) => Outcome::AskDelete(r.path), None => Outcome::None } },
    Key { code: Char('g'), alt: true, label: "⌥+G", word: "grep", what: "search inside the files", group: Group::Do,
          act: |t, _, _| { t.start_grep(); Outcome::None } },
    Key { code: Char('h'), alt: true, label: "⌥+H", word: "hidden", what: "show hidden files", group: Group::Do,
          act: |t, _, _| { t.toggle_hidden(); Outcome::None } },
    Key { code: Char('r'), alt: true, label: "⌥+R", word: "reload", what: "reread from disk", group: Group::Do,
          act: |t, _, _| { t.reload(); Outcome::None } },
];

/// The keys that answer while the preview has the focus. It had no strip at all before.
/// Columns one sideways step moves the preview's text.
pub(super) const SIDE_STEP: usize = 4;

pub const PREVIEW: &[Key] = &[
    Key { code: Up, alt: false, label: "↑↓", word: "", what: "scroll the preview", group: Group::Move,
          act: |t, _, _| { t.pscroll = t.pscroll.saturating_sub(1); Outcome::None } },
    Key { code: Down, alt: false, label: "", word: "", what: "scroll the preview", group: Group::Move,
          act: |t, _, _| { t.pscroll += 1; Outcome::None } },
    Key { code: PageUp, alt: false, label: "PgUp PgDn", word: "", what: "a screen at a time", group: Group::Move,
          act: |t, v, _| { let p = v.prev_page(); t.pscroll = t.pscroll.saturating_sub(p); Outcome::None } },
    Key { code: PageDown, alt: false, label: "", word: "", what: "a screen at a time", group: Group::Move,
          act: |t, v, _| { let p = v.prev_page(); t.pscroll += p; Outcome::None } },
    Key { code: Home, alt: false, label: "Home", word: "", what: "back to the top and the left edge", group: Group::Move,
          act: |t, _, _| { t.pscroll = 0; t.phscroll = 0; Outcome::None } },
    Key { code: Left, alt: false, label: "←→", word: "", what: "sideways, while long lines do not wrap", group: Group::Move,
          act: |t, _, _| { t.phscroll = t.phscroll.saturating_sub(SIDE_STEP); Outcome::None } },
    Key { code: Right, alt: false, label: "", word: "", what: "sideways, while long lines do not wrap", group: Group::Move,
          act: |t, _, _| { if !t.wrap { t.phscroll += SIDE_STEP; } Outcome::None } },
    Key { code: Char('w'), alt: false, label: "w", word: "wrap", what: "wrap long lines, again to scroll them sideways", group: Group::Do,
          act: |_, _, _| Outcome::Wrap },
    Key { code: Tab, alt: false, label: "Tab", word: "tree", what: "back to the tree", group: Group::Go,
          act: |t, _, _| { t.focus = Focus::Tree; Outcome::None } },
    Key { code: BackTab, alt: false, label: "", word: "tree", what: "back to the tree", group: Group::Go,
          act: |t, _, _| { t.focus = Focus::Tree; Outcome::None } },
    Key { code: Esc, alt: false, label: "esc", word: "shell", what: "hand the keys back to the pane", group: Group::Go,
          act: |_, _, _| Outcome::Close },
    Key { code: Char('z'), alt: false, label: "z", word: "full screen", what: "the whole screen, again to leave", group: Group::Do,
          act: |_, _, _| Outcome::Zoom },
    Key { code: Char('v'), alt: false, label: "v", word: "view", what: "the next view a plugin offers of this file", group: Group::Do,
          act: |_, _, _| Outcome::View },
];

/// What a key is called on screen, for the menus; empty when the tables have no such key.
pub fn key_label(code: KeyCode, alt: bool) -> &'static str {
    [COMMAND, PREVIEW].iter().find_map(|t| find(t, code, alt)).map_or("", |k| k.label)
}

/// The row a key press lands on, if any.
fn find(table: &'static [Key], code: KeyCode, alt: bool) -> Option<&'static Key> {
    table.iter().find(|r| r.code == code && r.alt == alt)
}

impl Tree {
    /// Whether the keys are on the preview rather than on the rows.
    pub fn in_preview(&self) -> bool {
        self.focus == Focus::Preview
    }

    /// Moves them onto the preview, if there is one to move onto.
    pub fn show_preview(&mut self) {
        if self.preview_on {
            self.focus = Focus::Preview;
        }
    }

    pub fn show_rows(&mut self) {
        self.focus = Focus::Tree;
    }
}

impl Tree {
    /// The row under a point: its path and whether it is a directory.
    pub fn row_at(&self, v: &TreeView, x: u16, y: u16) -> Option<(PathBuf, bool)> {
        let at = (x, y).into();
        let &(_, k) = v.hits.iter().find(|(r, _)| r.contains(at))?;
        self.rows.get(k).map(|r| (r.path.clone(), r.is_dir))
    }

    /// Selects the row under a point, if any, and gives the tree focus.
    pub fn select_at(&mut self, v: &TreeView, x: u16, y: u16) {
        let at = (x, y).into();
        if let Some(&(_, k)) = v.hits.iter().find(|(r, _)| r.contains(at)) {
            self.selected = k;
            self.focus = Focus::Tree;
        }
    }

    pub fn key(&mut self, v: &TreeView, k: crossterm::event::KeyEvent) -> Outcome {
        use crossterm::event::KeyCode::*;
        use crossterm::event::KeyModifiers;

        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // A command is the letter on the key; a query is what the layout printed, so only the
        // command path is normalised. That is what lets a Russian keyboard do both.
        let cmd = crate::keys::normalize(k);

        if self.focus == Focus::Preview {
            return find(PREVIEW, cmd.code, alt).map_or(Outcome::None, |r| (r.act)(self, v, cmd.code));
        }

        // Any character starts the filter: finding the file is what the panel is for, and the
        // actions that used to own the letters answer to `option` now.
        if !self.typing
            && !alt
            && !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && let KeyCode::Char(c) = k.code
        {
            self.start_filter(&c.to_string());
            self.selected = 0;
            return Outcome::None;
        }

        // Printable keys go to the query, not commands (else "r" would reload);
        // filter searches per key, grep waits for Enter since it reads files.
        if self.typing {
            match k.code {
                Esc => self.leave_query(),
                Enter => {
                    self.typing = false;
                    if self.mode == Mode::Grep {
                        self.reload();
                    }
                }
                Backspace => {
                    self.query.pop();
                    if self.mode == Mode::Filter {
                        self.reload();
                        self.selected = 0;
                    }
                }
                Char(c) => {
                    self.query.push(c);
                    if self.mode == Mode::Filter {
                        self.reload();
                        // every keystroke is a new list: the keys belong on its first match
                        self.selected = 0;
                    }
                }
                Up => self.step(-1),
                Down => self.step(1),
                _ => {}
            }
            return Outcome::None;
        }

        find(COMMAND, cmd.code, alt).map_or(Outcome::None, |r| (r.act)(self, v, cmd.code))
    }

    /// A click selects a row (and expands a directory too), the wheel
    /// scrolls whichever half the pointer is over.
    pub fn mouse(
        &mut self,
        v: &mut TreeView,
        ev: crossterm::event::MouseEvent,
        tree_area: Rect,
        preview_area: Option<Rect>,
    ) -> Outcome {
        use crossterm::event::KeyModifiers;
        use crossterm::event::MouseEventKind::*;
        let at = (ev.column, ev.row).into();
        let on_preview = preview_area.is_some_and(|r| r.contains(at));
        if !on_preview && !tree_area.contains(at) {
            return Outcome::None;
        }

        match ev.kind {
            Down(_) => {
                if v.btn_files.contains(at) {
                    self.set_view(View::Files);
                    return Outcome::None;
                }
                if v.btn_changes.contains(at) {
                    self.set_view(View::Changes);
                    return Outcome::None;
                }
                if v.btn_shape.contains(at) {
                    self.toggle_changes_shape();
                    return Outcome::None;
                }
                if v.btn_zoom.contains(at) {
                    return Outcome::Zoom;
                }
                if v.btn_close.contains(at) {
                    return Outcome::Hide;
                }
                if let Some(&(_, k)) = v.hits.iter().find(|(r, _)| r.contains(at)) {
                    self.focus = Focus::Tree;
                    self.selected = k;
                    // Double click on a file opens it; a directory toggles on every click.
                    let again = self
                        .last_click
                        .is_some_and(|(row, at)| row == k && at.elapsed() <= std::time::Duration::from_millis(350));
                    self.last_click = Some((k, std::time::Instant::now()));
                    match self.rows.get(k) {
                        Some(r) if r.up => self.go_up(),
                        Some(r) if r.more > 0 => self.show_all(k),
                        Some(r) if r.is_dir => self.toggle(k),
                        Some(r) if again => return Outcome::OpenInEditor(r.path.clone(), r.line),
                        _ => {}
                    }
                } else if on_preview {
                    self.focus = Focus::Preview;
                    v.psel = Some(((ev.row, ev.column), (ev.row, ev.column)));
                    v.psel_copy = false;
                    v.pdrag = true;
                } else {
                    v.psel = None;
                }
            }
            Drag(_) => {
                if let Some(sel) = v.psel.as_mut() {
                    sel.1 = (ev.row, ev.column);
                }
            }
            Up(_) => {
                v.pdrag = false;
                // One cell is a click, not a selection; copying it would wipe the clipboard.
                if v.psel.is_some_and(|s| s.0 != s.1) {
                    v.psel_copy = true;
                } else {
                    v.psel = None;
                }
            }
            // shift turns a mouse wheel sideways; a trackpad sends the sideways scroll itself
            ScrollUp | ScrollLeft if on_preview && (ev.kind == ScrollLeft || ev.modifiers.contains(KeyModifiers::SHIFT)) => {
                v.psel = None;
                self.phscroll = self.phscroll.saturating_sub(SIDE_STEP);
            }
            ScrollDown | ScrollRight if on_preview && (ev.kind == ScrollRight || ev.modifiers.contains(KeyModifiers::SHIFT)) => {
                v.psel = None;
                if !self.wrap {
                    self.phscroll += SIDE_STEP;
                }
            }
            ScrollUp => {
                if on_preview {
                    v.psel = None;
                    self.pscroll = self.pscroll.saturating_sub(3);
                } else {
                    self.step(-3);
                }
            }
            ScrollDown => {
                if on_preview {
                    v.psel = None;
                    self.pscroll += 3;
                } else {
                    self.step(3);
                }
            }
            _ => {}
        }
        Outcome::None
    }
}

impl TreeView {
    /// Whether a selection is being dragged right now: the pointer may leave the panel mid-drag,
    /// and the release still belongs to us. A finished selection stays on screen and claims
    /// nothing — it used to, and then no pane could be selected in again.
    pub fn selecting(&self) -> bool {
        self.pdrag
    }

    /// Drops the selection in the preview: a press anywhere else ends it, as it does in a pane.
    pub fn clear_selection(&mut self) {
        self.psel = None;
        self.pdrag = false;
    }

    fn page(&self) -> isize {
        self.tree_area.height.max(1) as isize
    }

    fn prev_page(&self) -> usize {
        self.prev_area.height.max(1) as usize
    }
}
