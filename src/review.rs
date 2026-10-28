//! Review changes: what an agent's pane changed in its repository, since the default branch or not yet committed,
//! with comments on lines that go back to the agent in one message.

use std::cell::Cell;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::git::{Changed, ReviewBase};
use crate::layout::PaneId;
use crate::messages::{Drawn, Line};
use crate::render::Theme;

/// A diff is shown up to this many lines.
const DIFF_LINES: usize = 5000;

/// A comment on one line of one file.
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub file: String,
    /// The line's number on its side: the new file's, or the old one's for a removed line.
    pub line: u32,
    pub old: bool,
    /// The line's text: it anchors the comment when the diff moves, and the agent is shown it.
    pub quote: String,
    pub text: String,
    /// The quoted line is no longer in the diff.
    pub outdated: bool,
    /// Made since the default branch, not on uncommitted changes: a line and its side mean that diff.
    pub since: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    File(usize),
    /// The folder of this file: a click folds or unfolds it.
    Fold(usize),
    /// The line between the list and the diff: a drag moves it.
    Divider,
    Line(usize),
    Edit(usize),
    Delete(usize),
    /// true: since the default branch.
    Since(bool),
    Send,
    Discard,
    Refresh,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
    /// Ask git for the files again.
    Load,
    /// Ask git for the selected file's diff.
    Diff,
    /// Ask for the text of a comment on this line.
    Comment(Comment),
    Edit(usize),
    Delete(usize),
    Send,
    Discard,
}

#[derive(Clone, Debug, PartialEq)]
struct DiffLine {
    text: String,
    /// '+', '-', ' ', or '@' for a hunk's head.
    kind: char,
    old: Option<u32>,
    new: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub pane: PaneId,
    /// The pane's name, which a message needs; None for an unnamed pane.
    pub who: Option<String>,
    pub root: PathBuf,
    /// None while loading.
    pub base: Option<ReviewBase>,
    pub since: bool,
    pub files: Option<Vec<Changed>>,
    pub at: usize,
    diff: Option<Vec<DiffLine>>,
    more: usize,
    pick: Option<usize>,
    fscroll: usize,
    dscroll: usize,
    /// The keys moved: the chosen file and line are kept in view; the wheel lets go of them.
    follow: bool,
    /// The folders folded in the list.
    fold: Vec<String>,
    /// The keys go to the diff, not the list.
    in_diff: bool,
    /// The list's cursor is on the folder of this file, not on the chosen file.
    on_folder: Option<usize>,
    /// The list's width as the divider was dragged; None: as wide as its longest name.
    pub width: Option<u16>,
    drag: bool,
    /// The list's left edge, as the last frame drew it, for a drag of the divider.
    left: Cell<u16>,
    /// The left column's right edge and the rows, as the last frame drew them, for the wheel.
    split: Cell<u16>,
    rows: Cell<usize>,
    /// Each column's first row drawn and its last possible one, so a notch of the wheel always moves.
    fview: Cell<(usize, usize)>,
    dview: Cell<(usize, usize)>,
    /// The send waiting for git to read the commented files again; a new window or another [s] is a new one.
    pub sending: Option<u64>,
}

impl Review {
    pub fn new(pane: PaneId, who: Option<String>, root: PathBuf) -> Self {
        let view = Cell::new((0, 0));
        Review { pane, who, root, base: None, since: true, files: None, at: 0, diff: None, more: 0, pick: None, fscroll: 0, dscroll: 0, follow: false, fold: Vec::new(), in_diff: false, on_folder: None, width: None, drag: false, left: Cell::new(0), split: Cell::new(0), rows: Cell::new(0), fview: view.clone(), dview: view, sending: None }
    }

    /// The commit compared with: where this branch left the default one, or HEAD.
    pub fn base_sha(&self) -> Option<String> {
        let b = self.base.as_ref()?;
        Some(if self.since { b.since.clone().unwrap_or_else(|| b.head.clone()) } else { b.head.clone() })
    }

    /// The two diffs' names, as the header shows them.
    fn mode(&self, since: bool) -> String {
        if !since {
            return "uncommitted".into();
        }
        format!("since {}", self.base.as_ref().and_then(|b| b.branch.clone()).unwrap_or_else(|| "the default branch".into()))
    }

    pub fn path(&self) -> Option<&str> {
        self.files.as_ref()?.get(self.at).map(|c| c.path.as_str())
    }

    /// Git's answer for this pane and mode; `since` falls back to uncommitted when there is nothing since.
    pub fn take_files(&mut self, since: bool, base: ReviewBase, mut files: Vec<Changed>) -> bool {
        if since != self.since {
            return false;
        }
        // the list's order: folders first, each with its files, then the root's files
        files.sort_by(|a, b| {
            let (da, na) = folder(&a.path);
            let (db, nb) = folder(&b.path);
            (da.is_empty(), da.to_lowercase(), da, na.to_lowercase(), na).cmp(&(db.is_empty(), db.to_lowercase(), db, nb.to_lowercase(), nb))
        });
        let keep = self.path().map(String::from);
        self.since = since && base.since.is_some();
        self.base = Some(base);
        self.at = keep.and_then(|k| files.iter().position(|c| c.path == k)).unwrap_or(0);
        self.files = Some(files);
        self.on_folder = None;
        self.diff = None;
        true
    }

    /// The diff of `path`, if it is still the one shown; re-anchors this file's comments on it.
    pub fn take_diff(&mut self, since: bool, path: &str, text: &str, comments: &mut [Comment]) -> bool {
        if since != self.since || self.path() != Some(path) {
            return false;
        }
        let (lines, more) = parse(text);
        reanchor(comments, path, since, &lines);
        self.diff = Some(lines);
        self.more = more;
        self.pick = None;
        self.dscroll = 0;
        true
    }

    fn select(&mut self, i: usize) -> Out {
        let Some(c) = self.files.as_ref().and_then(|f| f.get(i)) else { return Out::None };
        let dir = folder(&c.path).0.to_string();
        self.fold.retain(|f| *f != dir);
        if i == self.at && self.diff.is_some() {
            return Out::None;
        }
        self.at = i;
        self.diff = None;
        Out::Diff
    }

    fn comment_on(&mut self, i: usize) -> Out {
        let (Some(path), Some(l)) = (self.path(), self.diff.as_ref().and_then(|d| d.get(i))) else { return Out::None };
        let (line, old) = match (l.kind, l.old, l.new) {
            ('-', Some(o), _) => (o, true),
            (_, _, Some(n)) => (n, false),
            _ => return Out::None,
        };
        let c = Comment { file: path.to_string(), line, old, quote: l.text.clone(), text: String::new(), outdated: false, since: self.since };
        self.pick = Some(i);
        Out::Comment(c)
    }

    /// The keys move in the list or in the diff, as a tree and a list do: Tab goes from one to the other.
    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Out {
        use crossterm::event::KeyCode::*;
        let k = crate::keys::normalize(k);
        self.follow = true;
        match k.code {
            Esc => Out::Close,
            Char('s') => Out::Send,
            Char('x') => Out::Discard,
            Char('r') => Out::Load,
            Tab | BackTab => {
                self.in_diff = !self.in_diff;
                self.first_line();
                Out::None
            }
            code if self.in_diff => self.diff_key(code),
            code => self.list_key(code),
        }
    }

    /// Picks the diff's first numbered line when none is picked.
    fn first_line(&mut self) {
        if self.in_diff && self.pick.is_none() {
            self.pick = self.diff.as_ref().and_then(|d| d.iter().position(|l| l.old.or(l.new).is_some()));
        }
    }

    fn diff_key(&mut self, code: crossterm::event::KeyCode) -> Out {
        use crossterm::event::KeyCode::*;
        let lines = self.diff.as_ref().map_or(0, Vec::len);
        let step = |at: Option<usize>, by: isize| at.map_or(0, |a| a.saturating_add_signed(by)).min(lines.saturating_sub(1));
        let page = self.rows.get().max(1) as isize;
        let by = match code {
            Left => {
                self.in_diff = false;
                return Out::None;
            }
            Enter => return self.pick.map_or(Out::None, |i| self.comment_on(i)),
            Down if self.pick.is_none() => {
                self.first_line();
                return Out::None;
            }
            Up => -1,
            Down => 1,
            PageUp => -page,
            PageDown => page,
            Home => isize::MIN / 2,
            End => isize::MAX / 2,
            _ => return Out::None,
        };
        self.pick = Some(step(self.pick, by));
        Out::None
    }

    fn list_key(&mut self, code: crossterm::event::KeyCode) -> Out {
        use crossterm::event::KeyCode::*;
        let list = self.list();
        let Some(last) = list.len().checked_sub(1) else { return Out::None };
        let here = list.iter().position(|r| match (*r, self.on_folder) {
            (Row::Folder { first, .. }, Some(f)) => first == f,
            (Row::File(i), None) => i == self.at,
            _ => false,
        });
        let here = here.unwrap_or(0);
        let page = self.rows.get().saturating_sub(2).max(1);
        let to = match code {
            Up => here.saturating_sub(1),
            Down => (here + 1).min(last),
            PageUp => here.saturating_sub(page),
            PageDown => (here + page).min(last),
            Home => 0,
            End => last,
            Right | Enter => match list[here] {
                Row::Folder { first, .. } if self.folded(first) => return self.toggle(first),
                Row::Folder { first, .. } if code == Enter => return self.toggle(first),
                Row::Folder { .. } => (here + 1).min(last),
                Row::File(_) => {
                    self.in_diff = true;
                    self.first_line();
                    return Out::None;
                }
            },
            Left => match list[here] {
                Row::Folder { first, .. } if !self.folded(first) => return self.toggle(first),
                Row::Folder { .. } => here,
                // a file goes up to its folder's row
                Row::File(_) => list[..here].iter().rposition(|r| matches!(r, Row::Folder { .. })).filter(|_| self.path().is_some_and(|p| p.contains('/'))).unwrap_or(here),
            },
            _ => return Out::None,
        };
        match list[to] {
            Row::Folder { first, .. } => {
                self.on_folder = Some(first);
                Out::None
            }
            Row::File(i) => {
                self.on_folder = None;
                self.select(i)
            }
        }
    }

    fn folded(&self, first: usize) -> bool {
        self.files.as_ref().and_then(|f| f.get(first)).is_some_and(|c| self.fold.iter().any(|f| f == folder(&c.path).0))
    }

    /// Folds or unfolds the folder of file `first`.
    fn toggle(&mut self, first: usize) -> Out {
        if let Some(c) = self.files.as_ref().and_then(|f| f.get(first)) {
            let dir = folder(&c.path).0.to_string();
            if self.fold.contains(&dir) {
                self.fold.retain(|f| *f != dir);
            } else {
                self.fold.push(dir);
            }
        }
        Out::None
    }

    /// The divider is being dragged.
    pub fn dragging(&self) -> bool {
        self.drag
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        use crossterm::event::{MouseButton, MouseEventKind::*};
        let (x, y) = (ev.column, ev.row);
        match ev.kind {
            Down(MouseButton::Left) if d.close.contains((x, y).into()) || !d.frame.contains((x, y).into()) => Out::Close,
            Down(MouseButton::Left) if on_divider(d, x, y) => {
                self.drag = true;
                Out::None
            }
            Drag(MouseButton::Left) if self.drag => {
                self.width = Some(x.saturating_sub(self.left.get()));
                Out::None
            }
            Up(_) if self.drag => {
                self.drag = false;
                Out::None
            }
            Down(MouseButton::Left) => match d.at(x, y) {
                Some(Click::File(i)) => {
                    (self.in_diff, self.on_folder) = (false, None);
                    self.select(i)
                }
                Some(Click::Fold(i)) => {
                    (self.in_diff, self.on_folder) = (false, Some(i));
                    self.toggle(i)
                }
                Some(Click::Line(i)) => {
                    self.in_diff = true;
                    self.comment_on(i)
                }
                Some(Click::Edit(i)) => Out::Edit(i),
                Some(Click::Delete(i)) => Out::Delete(i),
                Some(Click::Since(s)) if s != self.since => {
                    self.since = s;
                    self.files = None;
                    self.diff = None;
                    self.pick = None;
                    self.fscroll = 0;
                    Out::Load
                }
                Some(Click::Send) => Out::Send,
                Some(Click::Discard) => Out::Discard,
                Some(Click::Refresh) => Out::Load,
                _ => Out::None,
            },
            ScrollUp | ScrollDown => {
                let by: isize = if ev.kind == ScrollUp { -3 } else { 3 };
                self.follow = false;
                if x < self.split.get() {
                    let (from, last) = self.fview.get();
                    self.fscroll = from.saturating_add_signed(by).min(last);
                } else {
                    let (from, last) = self.dview.get();
                    self.dscroll = from.saturating_add_signed(by).min(last);
                }
                Out::None
            }
            _ => Out::None,
        }
    }

    /// The window over the whole screen: the header, the files beside the diff, and the buttons.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, comments: &[Comment]) -> Drawn<Click> {
        let g = Style::default().bg(theme.ground);
        // the top row is left to the logo, an image the terminal draws over any text
        let screen = Rect { y: screen.y + 1, height: screen.height.saturating_sub(1), ..screen };
        let inner = screen.inner(crate::chrome::DIALOG_FRAME);
        let (w, room) = (inner.width as usize, inner.height.saturating_sub(2) as usize);
        let mut lines: Vec<Line<Click>> = Vec::with_capacity(room);

        let mut head: Line<Click> = Vec::new();
        let who = self.who.clone().unwrap_or_else(|| "unnamed pane".into());
        head.push((format!("{who} · {} · ", crate::paths::shown(&self.root)), g.fg(theme.muted), None));
        let can_since = self.base.as_ref().is_none_or(|b| b.since.is_some());
        for (on, word) in [(true, self.mode(true)), (false, self.mode(false))] {
            if on {
                head.push((String::new(), g, None));
            } else {
                head.push((" · ".into(), g.fg(theme.border), None));
            }
            let style = if on == self.since && (on || !self.since || can_since) {
                g.fg(theme.text).add_modifier(Modifier::BOLD)
            } else if on && !can_since {
                g.fg(theme.faint)
            } else {
                g.fg(theme.muted)
            };
            let hit = (on != self.since && (!on || can_since)).then_some(Click::Since(on));
            head.push((word, style, hit));
        }
        lines.push(head);
        let body = room.saturating_sub(2);
        let auto = self.files.as_ref().map_or(12, |f| Columns::of(f).need(f)).clamp(12, (w / 2).max(12));
        let lw = self.width.map_or(auto, usize::from).clamp(12, w.saturating_sub(20).max(12));
        self.left.set(inner.x);
        self.split.set(inner.x + lw as u16);
        self.rows.set(body);
        let left = self.file_rows(body, lw, comments, theme);
        let right = self.diff_rows(body, w.saturating_sub(lw + 1), comments, theme);
        for (l, r) in left.into_iter().zip(right) {
            let mut row = l;
            row.push(("│".into(), g.fg(if self.drag { theme.accent } else { theme.border }), Some(Click::Divider)));
            row.extend(r);
            lines.push(row);
        }
        let mut foot: Line<Click> = Vec::new();
        let mine = comments.len();
        let note = match (&self.who, mine) {
            (_, 0) => "a click on a line comments on it".to_string(),
            (None, n) => format!("{n} comment{}; name the pane to send them: Rename pane… in its menu", plural(n)),
            (Some(_), n) => format!("{n} comment{} in {} file{}", plural(n), files_in(comments), plural(files_in(comments))),
        };
        foot.push((note, g.fg(theme.faint), None));
        lines.push(foot);
        lines.truncate(room);

        let mut buttons = vec![("[r] refresh".to_string(), Click::Refresh)];
        if mine > 0 {
            if let Some(who) = &self.who {
                buttons.push((format!("[s] send {mine} comment{} to {who}", plural(mine)), Click::Send));
            }
            buttons.push(("[x] discard".to_string(), Click::Discard));
        }
        crate::messages::draw(buf, screen, "Review changes", theme, &lines, &crate::chrome::Scroll::default(), &buttons, "[Esc] close")
    }

    /// The list's rows: each folder, then its files unless it is folded; the root's files last.
    fn list(&self) -> Vec<Row> {
        let mut out = Vec::new();
        let Some(files) = &self.files else { return out };
        for (i, c) in files.iter().enumerate() {
            let dir = folder(&c.path).0;
            if !dir.is_empty() && (i == 0 || folder(&files[i - 1].path).0 != dir) {
                let all = files.iter().filter(|f| folder(&f.path).0 == dir);
                let (added, removed) = all.fold((0, 0), |(a, r), f| (a + f.added as u64, r + f.removed as u64));
                out.push(Row::Folder { first: i, added, removed });
            }
            if dir.is_empty() || !self.fold.iter().any(|f| f == dir) {
                out.push(Row::File(i));
            }
        }
        out
    }

    fn file_rows(&self, rows: usize, w: usize, comments: &[Comment], theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let pad = |s: String| fit(&s, w);
        let Some(files) = &self.files else {
            return (0..rows).map(|i| vec![(pad(if i == 0 { "loading…".into() } else { String::new() }), g.fg(theme.faint), None)]).collect();
        };
        if files.is_empty() {
            return (0..rows).map(|i| vec![(pad(if i == 0 { "nothing changed".into() } else { String::new() }), g.fg(theme.faint), None)]).collect();
        }
        let cols = Columns::of(files);
        let n = files.len();
        let mut out = vec![cols.row(format!("{n} file{}", plural(n)), (cols.added, cols.removed), w, g.fg(theme.text).add_modifier(Modifier::BOLD), None, theme)];
        out.push(vec![(pad(String::new()), g, None)]);
        let rows = rows.saturating_sub(2);
        let list = self.list();
        let last = list.len().saturating_sub(rows);
        let mut from = self.fscroll.min(last);
        let cursor = |r: &Row| match (*r, self.on_folder) {
            (Row::Folder { first, .. }, Some(f)) => first == f,
            (Row::File(i), None) => i == self.at,
            _ => false,
        };
        if self.follow && let Some(at) = list.iter().position(cursor) {
            from = from.min(at).max((at + 1).saturating_sub(rows));
        }
        self.fview.set((from, last));
        for r in 0..rows {
            let mut line = match list.get(from + r) {
                None => vec![(pad(String::new()), g, None)],
                Some(&Row::Folder { first, added, removed }) => {
                    let dir = folder(&files[first].path).0;
                    let sign = if self.fold.iter().any(|f| f == dir) { "▸ " } else { "▾ " };
                    cols.row(format!("{sign}{dir}"), (added, removed), w, g.fg(theme.muted), Some(Click::Fold(first)), theme)
                }
                Some(&Row::File(i)) => {
                    let c = &files[i];
                    let (dir, name) = folder(&c.path);
                    let lead = format!("{}{}", if dir.is_empty() { "" } else { "  " }, if comments.iter().any(|m| m.file == c.path) { "• " } else { "  " });
                    let style = if i == self.at { g.fg(theme.accent).add_modifier(Modifier::BOLD) } else { g.fg(theme.text) };
                    cols.row(format!("{lead}{name}"), (c.added as u64, c.removed as u64), w, style, Some(Click::File(i)), theme)
                }
            };
            // the keys' cursor, while they go to the list
            if !self.in_diff && list.get(from + r).is_some_and(cursor) {
                line.iter_mut().for_each(|s| s.1 = s.1.bg(theme.slab));
            }
            out.push(line);
        }
        out
    }

    /// Over the diff: the chosen file's folder, its name and counts, and a blank row.
    fn diff_rows(&self, rows: usize, w: usize, comments: &[Comment], theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let Some(c) = self.files.as_ref().and_then(|f| f.get(self.at)) else { return self.diff_body(rows, w, comments, theme) };
        let (dir, name) = folder(&c.path);
        let mut counts = Vec::new();
        if c.added > 0 {
            counts.push((format!("  +{}", thousands(c.added as u64)), g.fg(theme.ok), None));
        }
        if c.removed > 0 {
            counts.push((format!("  −{}", thousands(c.removed as u64)), g.fg(theme.bad), None));
        }
        let room = w.saturating_sub(1 + counts.iter().map(|(s, _, _)| cells(s)).sum::<usize>());
        let mut named: Line<Click> = vec![(format!(" {}", cut_middle(name, room)), g.fg(theme.text).add_modifier(Modifier::BOLD), None)];
        named.extend(counts);
        let used: usize = named.iter().map(|(s, _, _)| cells(s)).sum();
        named.push((" ".repeat(w.saturating_sub(used)), g, None));
        let shown_dir = if dir.is_empty() { String::new() } else { format!(" {}", cut_middle(&format!("{dir}/"), w.saturating_sub(1))) };
        let mut out = vec![vec![(fit(&shown_dir, w), g.fg(theme.muted), None)], named, vec![(" ".repeat(w), g, None)]];
        out.extend(self.diff_body(rows.saturating_sub(3), w, comments, theme));
        out.truncate(rows);
        out
    }

    fn diff_body(&self, rows: usize, w: usize, comments: &[Comment], theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let blank = || -> Line<Click> { vec![(" ".repeat(w), g, None)] };
        let Some(diff) = &self.diff else {
            let mut v: Vec<Line<Click>> = (0..rows).map(|_| blank()).collect();
            if self.path().is_some() && let Some(first) = v.first_mut() {
                *first = vec![(" loading…".into(), g.fg(theme.faint), None)];
            }
            return v;
        };
        let path = self.path().unwrap_or_default();
        // The diff with each comment under its line; first the outdated ones and those on the other diff.
        let mut shown: Vec<Line<Click>> = Vec::new();
        let mut picked = None;
        for (ci, c) in comments.iter().enumerate().filter(|(_, c)| c.file == path && (c.outdated || c.since != self.since)) {
            let why = if c.since != self.since { self.mode(c.since) } else { "outdated".into() };
            shown.push(comment_row(ci, &format!("{why}, \"{}\": {}", c.quote.trim(), c.text), w, theme));
        }
        for (i, l) in diff.iter().enumerate() {
            let num = match l.kind {
                '@' => String::new(),
                '-' => l.old.map_or(String::new(), |n| n.to_string()),
                _ => l.new.map_or(String::new(), |n| n.to_string()),
            };
            let colour = match l.kind {
                '+' => theme.ok,
                '-' => theme.bad,
                '@' => theme.muted,
                _ => theme.text,
            };
            let mut style = g.fg(colour);
            if self.pick == Some(i) && self.in_diff {
                style = style.bg(theme.slab);
            }
            let sign = if l.kind == '@' { ' ' } else { l.kind };
            let text = format!("{num:>5} {sign} {}", l.text.replace('\t', "    "));
            // only a numbered line takes a comment
            let hit = l.old.or(l.new).is_some().then_some(Click::Line(i));
            if self.pick == Some(i) {
                picked = Some(shown.len());
            }
            shown.push(vec![(fit(&text, w), style, hit)]);
            for (ci, c) in comments.iter().enumerate().filter(|(_, c)| c.file == path && !c.outdated && c.since == self.since && anchored(c, l)) {
                shown.push(comment_row(ci, &c.text, w, theme));
            }
        }
        if self.more > 0 {
            shown.push(vec![(format!("      … {} more lines", self.more), g.fg(theme.faint), None)]);
        }
        let last = shown.len().saturating_sub(rows);
        let mut from = self.dscroll.min(last);
        if let Some(at) = picked.filter(|_| self.follow) {
            from = from.min(at).max((at + 1).saturating_sub(rows));
        }
        self.dview.set((from, last));
        let mut out: Vec<Line<Click>> = shown.into_iter().skip(from).take(rows).collect();
        out.resize_with(rows, blank);
        out
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn files_in(comments: &[Comment]) -> usize {
    let mut f: Vec<&str> = comments.iter().map(|c| c.file.as_str()).collect();
    f.sort_unstable();
    f.dedup();
    f.len()
}

fn cells(s: &str) -> usize {
    crate::slots::cells(s)
}

/// `s` in exactly `w` cells: cut with "…", or padded.
fn fit(s: &str, w: usize) -> String {
    let s = crate::chrome::cut_cells(s, w);
    let n = cells(&s);
    s + &" ".repeat(w.saturating_sub(n))
}

/// (x, y) on the divider; the cells beside it are a file and a line.
pub fn on_divider(d: &Drawn<Click>, x: u16, y: u16) -> bool {
    d.hits.iter().any(|(r, h)| *h == Click::Divider && r.contains((x, y).into()))
}

/// `s` in at most `max` cells, its middle cut so a file's name and extension stay.
fn cut_middle(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let room = max - 1;
    let (mut head, mut tail) = (String::new(), Vec::new());
    let (mut hw, mut tw) = (0, 0);
    for c in s.chars() {
        let n = cells(c.encode_utf8(&mut [0; 4]));
        if hw + n > room / 2 {
            break;
        }
        hw += n;
        head.push(c);
    }
    for c in s.chars().rev() {
        let n = cells(c.encode_utf8(&mut [0; 4]));
        if tw + n > room - hw {
            break;
        }
        tw += n;
        tail.push(c);
    }
    format!("{head}…{}", tail.into_iter().rev().collect::<String>())
}

/// A path's folder ("" at the root) and its name.
fn folder(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Row {
    /// A folder, by its first file, with its files' sums.
    Folder { first: usize, added: u64, removed: u64 },
    File(usize),
}

/// The list's two count columns, right-aligned, as wide as the totals; none for a count that is zero everywhere.
struct Columns {
    added: u64,
    removed: u64,
}

impl Columns {
    fn of(files: &[Changed]) -> Self {
        let (added, removed) = files.iter().fold((0, 0), |(a, r), f| (a + f.added as u64, r + f.removed as u64));
        Columns { added, removed }
    }

    fn widths(&self) -> (usize, usize) {
        let w = |n: u64| if n == 0 { 0 } else { 1 + thousands(n).len() };
        (w(self.added), w(self.removed))
    }

    /// The cells the counts take, a space before each column.
    fn counts(&self) -> usize {
        let (a, r) = self.widths();
        (if a > 0 { 1 + a } else { 0 }) + if r > 0 { 1 + r } else { 0 }
    }

    /// The width the longest name or folder wants, counts included.
    fn need(&self, files: &[Changed]) -> usize {
        let names = files.iter().map(|c| match folder(&c.path) {
            ("", name) => 2 + cells(name),
            (dir, name) => (4 + cells(name)).max(2 + cells(dir)),
        });
        names.max().unwrap_or(0).max(cells("1000 files")) + self.counts()
    }

    fn row(&self, text: String, (added, removed): (u64, u64), w: usize, style: Style, hit: Option<Click>, theme: &Theme) -> Line<Click> {
        let g = Style::default().bg(theme.ground);
        let (aw, rw) = self.widths();
        let shown = |sign: char, n: u64| if n == 0 { String::new() } else { format!("{sign}{}", thousands(n)) };
        let room = w.saturating_sub(self.counts());
        let mut line = vec![(fit(&cut_middle(&text, room), room), style, hit.clone())];
        if aw > 0 {
            line.push((format!(" {:>aw$}", shown('+', added)), g.fg(theme.ok), hit.clone()));
        }
        if rw > 0 {
            line.push((format!(" {:>rw$}", shown('−', removed)), g.fg(theme.bad), hit));
        }
        line
    }
}

fn comment_row(ci: usize, text: &str, w: usize, theme: &Theme) -> Line<Click> {
    let g = Style::default().bg(theme.ground);
    let tail = "   edit · delete";
    vec![
        (fit(&format!("        comment: {text}"), w.saturating_sub(cells(tail))), g.fg(theme.accent), None),
        ("   ".into(), g, None),
        ("edit".into(), g.fg(theme.muted), Some(Click::Edit(ci))),
        (" · ".into(), g.fg(theme.border), None),
        ("delete".into(), g.fg(theme.muted), Some(Click::Delete(ci))),
    ]
}

/// Moves the comments on `path` made on this diff (since the default branch or uncommitted) to their lines in it.
fn reanchor(comments: &mut [Comment], path: &str, since: bool, lines: &[DiffLine]) {
    for c in comments.iter_mut().filter(|c| c.file == path && c.since == since) {
        anchor(c, lines);
    }
}

/// The comments re-anchored on fresh diffs, given as (path, since, git's text), before they are sent.
pub fn refresh(comments: &mut [Comment], diffs: &[(String, bool, String)]) {
    for (path, since, text) in diffs {
        reanchor(comments, path, *since, &parse(text).0);
    }
}

fn anchored(c: &Comment, l: &DiffLine) -> bool {
    (if c.old { l.kind == '-' && l.old == Some(c.line) } else { l.kind != '-' && l.kind != '@' && l.new == Some(c.line) }) && l.text == c.quote
}

/// Moves a comment to its quoted line nearest where it was; outdated when the quote is gone from its side.
fn anchor(c: &mut Comment, lines: &[DiffLine]) {
    let side = |l: &DiffLine| if c.old { (l.kind == '-').then_some(l.old).flatten() } else { (l.kind != '-' && l.kind != '@').then_some(l.new).flatten() };
    let best = lines.iter().filter(|l| l.text == c.quote).filter_map(side).min_by_key(|n| n.abs_diff(c.line));
    match best {
        Some(n) => (c.line, c.outdated) = (n, false),
        None => c.outdated = true,
    }
}

/// A unified diff's lines from the first hunk, numbered on both sides; and how many past `DIFF_LINES` were left out.
fn parse(text: &str) -> (Vec<DiffLine>, usize) {
    let mut out = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut started = false;
    let mut more = 0;
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("@@ ") {
            let nums = |s: &str| s.split(',').next().and_then(|n| n.trim_start_matches(['-', '+']).parse::<u32>().ok()).unwrap_or(0);
            let mut it = h.split(' ');
            (old, new) = (nums(it.next().unwrap_or("")), nums(it.next().unwrap_or("")));
            started = true;
            if out.len() < DIFF_LINES {
                out.push(DiffLine { text: l.to_string(), kind: '@', old: None, new: None });
            } else {
                more += 1;
            }
            continue;
        }
        if !started {
            if !l.starts_with("diff ") && !l.starts_with("index ") && !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.is_empty() && !l.starts_with("new file") && !l.starts_with("deleted file") {
                out.push(DiffLine { text: l.to_string(), kind: ' ', old: None, new: None });
            }
            continue;
        }
        let (kind, rest) = match l.chars().next() {
            Some(c @ ('+' | '-' | ' ')) => (c, &l[1..]),
            _ => continue,
        };
        let (o, n) = match kind {
            '+' => (None, Some(new)),
            '-' => (Some(old), None),
            _ => (Some(old), Some(new)),
        };
        if kind != '+' {
            old += 1;
        }
        if kind != '-' {
            new += 1;
        }
        if out.len() < DIFF_LINES {
            out.push(DiffLine { text: rest.to_string(), kind, old: o, new: n });
        } else {
            more += 1;
        }
    }
    (out, more)
}

/// The message the agent gets: each comment with its file and line from the repository's root, and the line.
pub fn message(comments: &[Comment]) -> String {
    let mut out = format!("Review of your changes, {} comment{}:\n", comments.len(), plural(comments.len()));
    for c in comments {
        let side = if c.old { " (a removed line)" } else { "" };
        let outdated = if c.outdated { " (outdated: the line has changed since)" } else { "" };
        out += &format!("\n{}:{}{side}{outdated}\n> {}\n{}\n", c.file, c.line, c.quote.trim_end(), c.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@ fn x\n keep\n-gone\n+new one\n+new two\n tail\n";

    #[test]
    fn a_diff_is_numbered_on_both_sides() {
        let (l, more) = parse(DIFF);
        assert_eq!(more, 0);
        let row = |i: usize| (l[i].kind, l[i].old, l[i].new, l[i].text.as_str());
        assert_eq!(row(0).0, '@');
        assert_eq!(row(1), (' ', Some(1), Some(1), "keep"));
        assert_eq!(row(2), ('-', Some(2), None, "gone"));
        assert_eq!(row(3), ('+', None, Some(2), "new one"));
        assert_eq!(row(4), ('+', None, Some(3), "new two"));
        assert_eq!(row(5), (' ', Some(3), Some(4), "tail"));
    }

    #[test]
    fn a_click_on_a_line_asks_for_a_comment_on_its_side() {
        let mut r = Review::new(1, Some("rev".into()), PathBuf::from("/r"));
        let base = ReviewBase { branch: Some("main".into()), since: Some("abc".into()), head: "def".into() };
        r.take_files(true, base, vec![Changed { path: "a.rs".into(), added: 2, removed: 1 }]);
        assert!(r.take_diff(true, "a.rs", DIFF, &mut []));
        assert_eq!(r.comment_on(3), Out::Comment(Comment { file: "a.rs".into(), line: 2, old: false, quote: "new one".into(), text: String::new(), outdated: false, since: true }));
        assert!(matches!(r.comment_on(2), Out::Comment(Comment { line: 2, old: true, .. })), "a removed line, by its old number");
        assert_eq!(r.comment_on(0), Out::None, "not a hunk's head");
        assert!(!r.take_diff(false, "a.rs", DIFF, &mut []), "an answer for the other mode is dropped");
    }

    #[test]
    fn a_comment_follows_its_line_and_is_outdated_when_it_is_gone() {
        let (lines, _) = parse(DIFF);
        let mut c = Comment { file: "a.rs".into(), line: 7, old: false, quote: "new two".into(), text: "why".into(), outdated: false, since: true };
        anchor(&mut c, &lines);
        assert_eq!((c.line, c.outdated), (3, false), "moved to where the line is now");
        c.quote = "not there".into();
        anchor(&mut c, &lines);
        assert!(c.outdated);
    }

    #[test]
    fn the_message_names_each_line_from_the_root_and_quotes_it() {
        let c = |file: &str, line, old, quote: &str, text: &str, outdated| Comment { file: file.into(), line, old, quote: quote.into(), text: text.into(), outdated, since: true };
        let m = message(&[c("src/app/model.rs", 138, false, "pub fn work(&self) -> Option<String> {", "почему не кэшировать ветку?", false), c("a.rs", 2, true, "gone", "why remove it?", true)]);
        assert_eq!(
            m,
            "Review of your changes, 2 comments:\n\nsrc/app/model.rs:138\n> pub fn work(&self) -> Option<String> {\nпочему не кэшировать ветку?\n\na.rs:2 (a removed line) (outdated: the line has changed since)\n> gone\nwhy remove it?\n"
        );
    }

    #[test]
    fn the_window_draws_files_beside_the_diff_with_comments_under_their_lines() {
        let theme = Theme::dark();
        let mut r = Review::new(1, Some("rev".into()), PathBuf::from("/r"));
        let base = ReviewBase { branch: Some("main".into()), since: None, head: "def".into() };
        r.take_files(true, base, vec![Changed { path: "a.rs".into(), added: 2, removed: 1 }, Changed { path: "b.rs".into(), added: 1, removed: 0 }]);
        assert!(!r.since, "nothing since main: uncommitted");
        let mut comments = vec![Comment { file: "a.rs".into(), line: 2, old: false, quote: "new one".into(), text: "why?".into(), outdated: false, since: false }];
        r.take_diff(false, "a.rs", DIFF, &mut comments);
        let screen = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(screen);
        let d = r.draw(&mut buf, screen, &theme, &comments);
        let text: Vec<String> = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect()).collect();
        let all = text.join("\n");
        assert!(all.contains("Review changes") && all.contains("rev · /r · since main · uncommitted"), "{all}");
        assert!(all.contains("• a.rs") && all.contains("+2 −1│") && all.contains("    2 + new one") && all.contains("comment: why?"), "{all}");
        assert!(all.contains("[s] send 1 comment to rev") && all.contains("[Esc] close"), "{all}");
        assert!(!d.hits.iter().any(|(_, h)| *h == Click::Since(true)), "since main is greyed: nothing since it");
        assert!(d.hits.iter().any(|(_, h)| *h == Click::File(1)) && d.hits.iter().any(|(_, h)| *h == Click::Edit(0)) && d.hits.iter().any(|(_, h)| *h == Click::Send));
        // an unnamed pane takes comments, and says how to send them
        r.who = None;
        let mut buf = Buffer::empty(screen);
        let d = r.draw(&mut buf, screen, &theme, &comments);
        let all: String = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
        assert!(all.contains("name the pane to send them") && !d.hits.iter().any(|(_, h)| *h == Click::Send), "{all}");
    }

    fn mouse(kind: crossterm::event::MouseEventKind, (x, y): (u16, u16)) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE }
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    /// A window over `files`, the first one's diff 200 numbered lines long, drawn on 100×20.
    fn long(files: usize) -> (Review, Buffer, Rect) {
        let mut r = Review::new(1, Some("rev".into()), PathBuf::from("/r"));
        let base = ReviewBase { branch: Some("main".into()), since: Some("abc".into()), head: "def".into() };
        r.take_files(true, base, (0..files).map(|i| Changed { path: format!("f{i:02}.rs"), added: 1, removed: 0 }).collect());
        let text = format!("@@ -1,200 +1,200 @@\n{}", (1..=200).map(|i| format!(" line {i}\n")).collect::<String>());
        assert!(r.take_diff(true, "f00.rs", &text, &mut []));
        let screen = Rect::new(0, 0, 100, 20);
        (r, Buffer::empty(screen), screen)
    }

    fn first_line(r: &Review, buf: &mut Buffer, screen: Rect) -> (Drawn<Click>, usize) {
        let d = r.draw(buf, screen, &Theme::dark(), &[]);
        let first = d.hits.iter().find_map(|(_, h)| if let Click::Line(i) = h { Some(*i) } else { None }).unwrap();
        (d, first)
    }

    #[test]
    fn the_wheel_scrolls_past_a_commented_line_and_every_notch_moves() {
        use crossterm::event::MouseEventKind::{ScrollDown, ScrollUp};
        let (mut r, mut buf, screen) = long(1);
        let (d, _) = first_line(&r, &mut buf, screen);
        let at = d.hits.iter().find(|(_, h)| *h == Click::Line(3)).unwrap().0;
        assert!(matches!(r.mouse(mouse(crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), (at.x, at.y)), &d), Out::Comment(_)));
        for _ in 0..10 {
            r.mouse(mouse(ScrollDown, (60, 10)), &d);
            first_line(&r, &mut buf, screen);
        }
        assert_eq!(first_line(&r, &mut buf, screen).1, 30, "ten notches of three lines, the commented line left behind");
        for _ in 0..100 {
            r.mouse(mouse(ScrollDown, (60, 10)), &d);
            first_line(&r, &mut buf, screen);
        }
        let bottom = first_line(&r, &mut buf, screen).1;
        r.mouse(mouse(ScrollUp, (60, 10)), &d);
        assert_eq!(first_line(&r, &mut buf, screen).1, bottom - 3, "no notch is spent past the end");
    }

    #[test]
    fn keys_pick_lines_and_files_and_keep_them_in_view() {
        use crossterm::event::KeyCode::{BackTab, Down, Enter, PageDown, Tab, Up};
        let (mut r, mut buf, screen) = long(40);
        assert_eq!(r.key(key(Tab)), Out::None, "Tab goes to the diff");
        assert!(matches!(r.key(key(Enter)), Out::Comment(Comment { line: 1, .. })), "the first line is picked");
        r.key(key(PageDown));
        r.key(key(PageDown));
        let (d, first) = first_line(&r, &mut buf, screen);
        let picked = r.pick.unwrap();
        assert!(first <= picked && d.hits.iter().any(|(_, h)| *h == Click::Line(picked)), "the picked line is drawn");
        r.key(key(BackTab));
        for _ in 0..30 {
            assert_eq!(r.key(key(Down)), Out::Diff);
        }
        assert_eq!(r.path(), Some("f30.rs"));
        let d = r.draw(&mut buf, screen, &Theme::dark(), &[]);
        assert!(d.hits.iter().any(|(_, h)| *h == Click::File(30)), "the keys keep the chosen file in view");
        r.key(key(Up));
        assert_eq!(r.path(), Some("f29.rs"));
    }

    fn shown(r: &Review, comments: &[Comment]) -> (Drawn<Click>, Vec<String>) {
        let screen = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(screen);
        let d = r.draw(&mut buf, screen, &Theme::dark(), comments);
        (d, (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect()).collect())
    }

    #[test]
    fn files_are_listed_under_their_folders_with_their_counts_in_columns() {
        let mut r = Review::new(1, Some("rev".into()), PathBuf::from("/r"));
        let files = [("src/b.rs", 1, 0), ("README.md", 3, 0), ("app.rs", 1, 0), ("src/a.rs", 2, 1), ("docs/guide/x.md", 25183, 0)];
        let base = ReviewBase { branch: Some("main".into()), since: Some("abc".into()), head: "def".into() };
        r.take_files(true, base, files.iter().map(|&(p, added, removed)| Changed { path: p.into(), added, removed }).collect());
        assert_eq!(r.path(), Some("docs/guide/x.md"), "folders first, the root's files last");
        let comments = [Comment { file: "src/a.rs".into(), line: 1, old: false, quote: "x".into(), text: "y".into(), outdated: false, since: true }];
        let (d, rows) = shown(&r, &comments);
        let all = rows.join("\n");
        let list: Vec<&str> = rows.iter().map(|l| l.split('│').nth(1).unwrap_or("").trim_end()).filter(|l| l.contains('+')).collect();
        assert_eq!(list.len(), 8, "{all}");
        for (row, want) in list.iter().zip(["5 files", "▾ docs/guide", "    x.md", "▾ src", "  • a.rs", "    b.rs", "  app.rs", "  README.md"]) {
            assert!(row.contains(want), "{row:?} names {want:?}\n{all}");
        }
        assert!(list[0].ends_with("+25,190 −1") && list[1].ends_with("+25,183") && list[3].ends_with("+3 −1"), "{all}");
        let right = |l: &str| l.split('│').nth(1).and_then(|s| { let c: Vec<char> = s.chars().collect(); let at = c.iter().position(|&x| x == '+')?; Some(at + 1 + c[at + 1..].iter().take_while(|x| x.is_ascii_digit() || **x == ',').count()) });
        let edges: Vec<_> = rows.iter().filter_map(|l| right(l)).collect();
        assert!(edges.windows(2).all(|w| w[0] == w[1]), "the counts stand in one column\n{all}");
        assert!(!list.iter().any(|l| l.contains("−0")), "a zero is not printed\n{all}");
        assert!(all.contains("│ docs/guide/") && all.contains("│ x.md  +25,183"), "the chosen file's folder and name over its diff\n{all}");
        // a click folds a folder; choosing a file in it unfolds it
        let at = d.hits.iter().find(|(_, h)| matches!(h, Click::Fold(i) if r.files.as_ref().unwrap()[*i].path.starts_with("src/"))).unwrap().0;
        let down = crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left);
        assert_eq!(r.mouse(mouse(down, (at.x, at.y)), &d), Out::None);
        let all = shown(&r, &comments).1.join("\n");
        assert!(all.contains("▸ src") && !all.contains("a.rs"), "{all}");
        // the keys walk the list as a tree: Right unfolds, Left goes up to the folder and folds it
        use crossterm::event::KeyCode::{Left, Right, Up};
        assert_eq!(r.key(key(Right)), Out::None);
        let all = shown(&r, &comments).1.join("\n");
        assert!(all.contains("▾ src") && all.contains("• a.rs"), "{all}");
        assert_eq!(r.key(key(crossterm::event::KeyCode::Down)), Out::Diff);
        assert_eq!(r.path(), Some("src/a.rs"));
        r.key(key(Left));
        r.key(key(Left));
        assert!(shown(&r, &comments).1.join("\n").contains("▸ src"));
        assert_eq!(r.key(key(Up)), Out::Diff);
        assert_eq!(r.path(), Some("docs/guide/x.md"));
        assert_eq!(r.key(key(Right)), Out::None);
        assert!(r.in_diff, "Right on a file goes to its diff");
    }

    #[test]
    fn a_drag_moves_the_divider_and_the_list_keeps_its_width() {
        use crossterm::event::{MouseButton::Left, MouseEventKind::{Down, Drag, Up}};
        let (mut r, mut buf, screen) = long(3);
        let d = r.draw(&mut buf, screen, &Theme::dark(), &[]);
        let at = d.hits.iter().find(|(_, h)| *h == Click::Divider).expect("the divider is drawn").0;
        assert_eq!(r.mouse(mouse(Down(Left), (at.x, at.y)), &d), Out::None);
        assert!(r.dragging());
        r.mouse(mouse(Drag(Left), (at.x + 20, at.y)), &d);
        r.mouse(mouse(Up(Left), (at.x + 20, at.y)), &d);
        assert!(!r.dragging());
        let d = r.draw(&mut buf, screen, &Theme::dark(), &[]);
        assert_eq!(d.hits.iter().find(|(_, h)| *h == Click::Divider).unwrap().0.x, at.x + 20);
        r.mouse(mouse(Down(Left), (at.x + 20, at.y)), &d);
        r.mouse(mouse(Drag(Left), (99, at.y)), &d);
        let d = r.draw(&mut buf, screen, &Theme::dark(), &[]);
        assert!(d.hits.iter().find(|(_, h)| *h == Click::Divider).unwrap().0.x < 80, "the diff keeps room");
    }

    #[test]
    fn a_long_name_is_cut_in_the_middle_and_keeps_its_end() {
        let cut = cut_middle("astrotheme-planetary-dominants.txt", 20);
        assert!(cut.contains('…') && cut.ends_with("ants.txt") && cut.starts_with("astro") && cells(&cut) == 20, "{cut}");
        assert_eq!(cut_middle("short.rs", 20), "short.rs");
        let wide = cut_middle("文档说明文件很长的名字.md", 9);
        assert!(cells(&wide) <= 9 && wide.ends_with(".md"), "{wide}");
        assert_eq!(thousands(25183), "25,183");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn the_other_diff_asks_git_again_and_shows_its_comments_apart() {
        let (mut r, mut buf, screen) = long(1);
        let d = r.draw(&mut buf, screen, &Theme::dark(), &[]);
        let at = d.hits.iter().find(|(_, h)| *h == Click::Since(false)).expect("uncommitted is a click target").0;
        assert_eq!(r.mouse(mouse(crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), (at.x, at.y)), &d), Out::Load);
        assert!(!r.since && r.files.is_none());
        let base = ReviewBase { branch: Some("main".into()), since: Some("abc".into()), head: "def".into() };
        assert!(!r.take_files(true, base.clone(), Vec::new()), "a late answer for since main is dropped");
        r.take_files(false, base, vec![Changed { path: "a.rs".into(), added: 2, removed: 1 }]);
        let mut comments = vec![Comment { file: "a.rs".into(), line: 2, old: true, quote: "gone".into(), text: "keep it".into(), outdated: false, since: true }];
        r.take_diff(false, "a.rs", "@@ -1 +1 @@\n-other\n+x\n", &mut comments);
        assert!(!comments[0].outdated, "only a diff of its own kind moves or outdates a comment");
        let mut buf = Buffer::empty(screen);
        r.draw(&mut buf, screen, &Theme::dark(), &comments);
        let all: String = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
        assert!(all.contains("comment: since main, \"gone\": keep it"), "{all}");
        refresh(&mut comments, &[("a.rs".into(), true, "@@ -1 +1 @@\n-other\n+x\n".into())]);
        assert!(comments[0].outdated, "before a send, its own diff no longer has the line");
    }

    #[test]
    fn a_hunk_head_picked_by_the_keys_stays_in_view() {
        use crossterm::event::KeyCode::Down;
        let mut r = Review::new(1, None, PathBuf::from("/r"));
        r.take_files(true, ReviewBase { branch: None, since: None, head: "def".into() }, vec![Changed { path: "a.rs".into(), added: 0, removed: 0 }]);
        let hunk = |at: usize| format!("@@ -{at},50 +{at},50 @@\n{}", (0..50).map(|i| format!(" line {}\n", at + i)).collect::<String>());
        r.take_diff(false, "a.rs", &(hunk(1) + &hunk(300)), &mut []);
        r.key(key(crossterm::event::KeyCode::Tab));
        for _ in 0..51 {
            r.key(key(Down));
        }
        let screen = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(screen);
        r.draw(&mut buf, screen, &Theme::dark(), &[]);
        let all: String = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
        assert!(all.contains("@@ -300,50"), "{all}");
    }

    #[test]
    fn only_a_numbered_line_is_a_click_target() {
        let mut r = Review::new(1, None, PathBuf::from("/r"));
        r.take_files(true, ReviewBase { branch: None, since: None, head: "def".into() }, vec![Changed { path: "b.bin".into(), added: 0, removed: 0 }]);
        r.take_diff(false, "b.bin", "diff --git a/b.bin b/b.bin\nBinary files a/b.bin and b/b.bin differ\n", &mut []);
        let screen = Rect::new(0, 0, 100, 20);
        let d = r.draw(&mut Buffer::empty(screen), screen, &Theme::dark(), &[]);
        assert!(!d.hits.iter().any(|(_, h)| matches!(h, Click::Line(_))), "a note shows no hand");
    }

    #[test]
    fn a_wide_path_keeps_the_columns_in_place() {
        let mut r = Review::new(1, None, PathBuf::from("/r"));
        r.take_files(true, ReviewBase { branch: None, since: None, head: "def".into() }, vec![Changed { path: "文档/说明文件很长的名字.md".into(), added: 1, removed: 0 }]);
        let screen = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(screen);
        r.draw(&mut buf, screen, &Theme::dark(), &[]);
        let bar = |y: u16| (0..100).filter(|&x| buf[(x, y)].symbol() == "│").collect::<Vec<_>>();
        assert_eq!(bar(3), bar(4), "the divider stands in one column on every row");
    }
}
