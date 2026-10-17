//! Sidebar: a directory tree with a preview of the selected file below.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

use crate::render::Theme;

mod keys;
mod model;
mod view;

pub use keys::*;
pub use model::*;

/// A folder with more children than this shows this many and a row offering the rest: every
/// row costs a stat and a folder row a read, and the tree rebuilds whole on every expand.
const HUGE_ROWS: usize = 200;

/// Huge: never crawled into by the filter nor folded into a chain, and never opened on their own.
/// A click or Enter opens one level, which is a handful of rows.
const SKIP: [&str; 3] = [".git", "node_modules", "target"];

/// Bytes sniffed from the start for a NUL byte, to call a file text or binary.
const SNIFF: usize = 8 * 1024;

/// Kitty image id; one panel shows one image at a time.
const IMG_ID: u32 = 1;

const DIR_PEEK: usize = 20;

/// Where the filter and grep walk stops: depth, matches, time; the screen says which.
const DEEP_DEPTH: u16 = 6;
const FIND_MAX: usize = 1000;
const FIND_TIME: std::time::Duration = std::time::Duration::from_millis(100);
const GREP_TIME: std::time::Duration = std::time::Duration::from_secs(1);

const GREP_MAX: usize = 500;
const GREP_FILE_MAX: u64 = 512 * 1024;
const GREP_LINE: usize = 100;

use crossterm::event::KeyCode::{self, *};

/// Where the two halves landed, for the mouse and for the width the seam drags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pair {
    /// The whole frame, title row and borders included: what the mouse is routed by.
    pub frame: Rect,
    pub tree: Rect,
    pub preview: Option<Rect>,
    /// The column between the halves; None when they are stacked instead.
    pub seam: Option<u16>,
}

/// The panel only emits actions; the app performs them (open, insert, delete, ...).
pub enum Outcome {
    None,
    Close,
    /// Open in the editor; the line comes from a content-search hit, if any.
    OpenInEditor(PathBuf, Option<u32>),
    InsertPath(PathBuf),
    OpenSystem(PathBuf),
    CopyPath(PathBuf),
    Cd(PathBuf),
    /// The selected directory becomes a workspace of its own.
    NewSpace(PathBuf),
    /// Ask for a name; a trailing `/` means a directory.
    AskCreate(PathBuf),
    AskRename(PathBuf),
    AskDelete(PathBuf),
    /// Toggle the tree and preview taking the whole screen.
    Zoom,
    /// Toggle wrapping long lines in the preview.
    Wrap,
    /// The next of the views the preview's plugin offers.
    View,
    /// Close the tree (the header's × button).
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Tree,
    Preview,
}

/// Which question the panel is answering: where a file is, or what has changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Files,
    Changes,
}

/// What the rows hold: tree, filter hits, or grep hits — same Row shape, different origin/label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Tree,
    Filter,
    Grep,
}

/// Flat list, not a tree: easier to draw and hit-test with the mouse; depth is a number for indent.
#[derive(Debug, Clone)]
struct Row {
    path: PathBuf,
    /// What we draw: the name in the tree, the relative path in hits.
    label: String,
    depth: u16,
    is_dir: bool,
    size: u64,
    /// Expanded state, drawn as ▸/▾.
    open: bool,
    /// A symlink - we draw a → after the name.
    link: bool,
    /// Directory failed to read (no permission); drawn as ⚠ instead of panicking.
    err: bool,
    /// Line number from a content search hit.
    line: Option<u32>,
    /// The `../` row: a way out of the root, not an entry in it.
    up: bool,
    /// The last row of a folder cut at `HUGE_ROWS`: how many are not shown. `path` is the folder.
    more: usize,
}

/// The marked paths as a tree, for `changes`: directories by name, then the files in them.
#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<String>,
}

/// A single directory entry along with its already-resolved type.
struct Ent {
    is_dir: bool,
    link: bool,
    name: String,
    path: PathBuf,
    size: u64,
    /// Last write, when the filesystem knows one: what the preview sorts a directory by.
    changed: Option<std::time::SystemTime>,
}

pub struct Tree {
    /// The space's folder as the app gave it, before canonicalizing: telling a new one apart costs no syscall.
    given: PathBuf,
    /// Tree root: the filesystem root, or the directory zoomed into with `=`.
    root: PathBuf,
    /// The space's working directory, canonical; also the frame's label and what tells a
    /// space switch apart from a same-directory redraw.
    space: PathBuf,
    /// Path components leading to `space`: always shown even if hidden, otherwise
    /// `~/.config/app` could never be expanded.
    on_path: HashSet<PathBuf>,
    /// Where to search from with the filter and grep - captured at keypress.
    scope: PathBuf,
    rows: Vec<Row>,
    /// Expanded directories. Survive a tree reload via `r`.
    expanded: HashSet<PathBuf>,
    /// Folders past `HUGE_ROWS` the user asked to see whole.
    shown_all: HashSet<PathBuf>,
    /// What an empty root says about itself, read when the rows were.
    empty: String,
    selected: usize,
    /// First drawn row: the list is longer than the panel.
    top: usize,
    mode: Mode,
    query: String,
    /// Why the last filter or grep stopped early, for the line under the query.
    cut: Option<Cut>,
    /// The last filter that saw every match: (scope, hidden, query, rows). A longer query narrows it.
    complete: Option<(PathBuf, bool, String, Vec<Row>)>,
    /// A query is being typed: printable keys go into the string, not into commands.
    typing: bool,
    hidden: bool,
    focus: Focus,
    preview: Option<crate::preview::Preview>,
    pscroll: usize,
    /// Columns the preview's text is scrolled to the left; nothing while it wraps.
    phscroll: usize,
    /// Long lines wrap in the preview; set from the config each frame.
    pub wrap: bool,
    /// Which line of the file the preview is scrolled to - search hits have their own.
    preview_line: Option<u32>,
    /// Set by the UI before drawing: the preview column is switched on.
    pub preview_on: bool,
    /// Set by the UI before drawing, from the settings and the client terminal.
    pub image_view: crate::preview::ImageView,
    /// Git marks from the app: absolute path -> M/N/A/D.
    marks: HashMap<PathBuf, char>,
    /// Badges by exact path, set by the app before each frame.
    pub badges: std::sync::Arc<crate::slots::TreeBadges>,
    /// `changes` as one row per file rather than the tree the files sit in.
    changes_flat: bool,
    /// Keys go to the tree: only then do its frame light up and its hints show.
    pub active: bool,
    /// Row and time of the last click, for the double click that opens a file.
    last_click: Option<(usize, std::time::Instant)>,
    view: View,
    /// Moves on every reload: the same file may read differently now.
    reloads: u64,
    /// What the preview holds, and what was last asked for it; the asking and loading happen outside the draw.
    shown: Option<Wanted>,
    asked: Option<Wanted>,
    /// What the shown image was last asked to have made.
    need_asked: Option<crate::preview::Need>,
    /// The preview just changed: the next frame scrolls it to its line.
    fresh: bool,
}

/// What the preview should hold: the selected row, as its diff in `changes`.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub path: PathBuf,
    pub diff: bool,
    reloads: u64,
}

/// What the last frame drew where and still owes the terminal: the draw writes it, keys and mouse read it.
#[derive(Default)]
pub struct TreeView {
    /// Terminal bytes for the image: Kitty draws it itself, bypassing ratatui's buffer,
    /// so it's handed over once and cleared on file change.
    pub(super) pending_img: Vec<u8>,
    pub(super) sent_img: Option<(PathBuf, Rect)>,
    /// Set by `draw_preview`, reset by `take_image_bytes`: a frame without the preview clears the image.
    pub(super) preview_drawn: bool,
    /// Row rects from the last frame with their index into `rows`, since `mouse()`
    /// runs separately from `draw()`.
    pub(super) hits: Vec<(Rect, usize)>,
    /// Header buttons: full screen and close. Zero rects when not drawn.
    pub(super) btn_zoom: Rect,
    pub(super) btn_close: Rect,
    /// A selection inside the preview, in screen cells; it lives until the next click.
    pub(super) psel: Option<crate::select::Span>,
    /// Set when the mouse is released on a real span: the text is taken off the next frame,
    /// which is the only place the drawn cells exist.
    pub(super) psel_copy: bool,
    /// A drag in progress. The selection outlives it on screen, so it cannot stand for this.
    pub(super) pdrag: bool,
    pub(super) pending_copy: Option<String>,
    /// The title's words, for the clicks that switch the view and the shape of `changes`.
    pub(super) btn_files: Rect,
    pub(super) btn_changes: Rect,
    pub(super) btn_shape: Rect,
    /// The two halves from the last frame - the mouse uses them to tell what to scroll.
    pub(super) tree_area: Rect,
    pub(super) prev_area: Rect,
    /// Rows the list had room for in the last frame.
    pub(super) list_h: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    /// A private directory per test; removed by hand at the end so a panicked test
    /// doesn't break the next one.
    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "nerd-tree-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        // On macOS /var is a symlink to /private/var, and the tree canonicalizes paths.
        canon(&p)
    }

    fn rm(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    fn theme() -> Theme {
        Theme::dark()
    }

    /// A tree with the view state the app keeps beside it.
    struct T {
        t: Tree,
        v: TreeView,
    }

    impl T {
        fn key(&mut self, k: KeyEvent) -> Outcome {
            self.t.key(&self.v, k)
        }

        fn mouse(&mut self, ev: MouseEvent, tree_area: Rect, preview_area: Option<Rect>) -> Outcome {
            self.t.mouse(&mut self.v, ev, tree_area, preview_area)
        }

        /// Loads what the app would have asked its workers for and follows the last frame, then draws.
        fn draw_pair(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, tree_w: u16) -> Pair {
            self.t.settle(&mut self.v);
            self.t.follow(&self.v);
            self.t.draw_pair(&mut self.v, buf, area, theme, tree_w)
        }

        fn selecting(&self) -> bool {
            self.v.selecting()
        }

        fn clear_selection(&mut self) {
            self.v.clear_selection()
        }

        fn take_copy(&mut self) -> Option<String> {
            self.v.take_copy()
        }

        fn take_image_bytes(&mut self) -> Vec<u8> {
            self.v.take_image_bytes()
        }

        fn forget_image(&mut self) {
            self.v.forget_image()
        }
    }

    impl std::ops::Deref for T {
        type Target = Tree;
        fn deref(&self) -> &Tree {
            &self.t
        }
    }

    impl std::ops::DerefMut for T {
        fn deref_mut(&mut self) -> &mut Tree {
            &mut self.t
        }
    }

    /// A tree of the temp directory: a new tree is already rooted at the directory it is given.
    fn at(d: &Path) -> T {
        let t = Tree::new(d.to_path_buf());
        assert_eq!(t.root, d, "rooted at the working directory");
        T { t, v: TreeView::default() }
    }

    /// The entries, without the `../` row the tests are not about.
    fn ents(t: &Tree) -> Vec<&Row> {
        t.rows.iter().filter(|r| !r.up).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// An action: the letters answer to `option` now, since a bare one types into the filter.
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn ktype(t: &mut T, s: &str) {
        for c in s.chars() {
            t.key(key(KeyCode::Char(c)));
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn drag(column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE }
    }

    fn release(column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE }
    }

    /// Row labels in draw order.
    fn names(t: &Tree) -> Vec<String> {
        ents(t).iter().map(|r| r.label.clone()).collect()
    }

    /// The halves stacked: asking for a tree as wide as the panel leaves no room for a column.
    fn draw(t: &mut T, w: u16, h: u16) -> (Buffer, Rect) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        t.draw_pair(&mut buf, area, &theme(), w);
        (buf, area)
    }

    /// Both halves side by side, as a wide enough panel draws them.
    fn draw_wide(t: &mut T, w: u16, h: u16, tree_w: u16) -> (Buffer, Pair) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let pair = t.draw_pair(&mut buf, area, &theme(), tree_w);
        (buf, pair)
    }

    /// A row's text starting at column 1 (column 0 is the frame's │).
    fn line_at(buf: &Buffer, y: u16, w: u16) -> String {
        (1..w).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn dirs_come_first_then_files_case_insensitively() {
        let d = tmp("order");
        fs::create_dir(d.join("zeta")).unwrap();
        fs::create_dir(d.join("Alpha")).unwrap();
        fs::write(d.join("b.txt"), "b").unwrap();
        fs::write(d.join("A.txt"), "a").unwrap();

        let t = at(&d);
        assert_eq!(names(&t), ["Alpha", "zeta", "A.txt", "b.txt"]);
        rm(&d);
    }

    #[test]
    fn children_appear_only_after_expanding_and_vanish_on_collapse() {
        let d = tmp("expand");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/inner.txt"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["sub"], "nested entries aren't read until expanded");

        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["sub", "inner.txt"]);
        assert!(ents(&t)[0].open);

        t.key(key(KeyCode::Left));
        assert_eq!(names(&t), ["sub"]);
        assert!(!ents(&t)[0].open);
        rm(&d);
    }

    #[test]
    fn a_huge_folder_shows_its_first_rows_and_offers_the_rest() {
        let d = tmp("huge");
        let big = d.join("big");
        fs::create_dir(&big).unwrap();
        for i in 0..HUGE_ROWS + 50 {
            fs::write(big.join(format!("f{i:04}.txt")), "x").unwrap();
        }
        let mut t = at(&d);
        t.key(key(KeyCode::Enter));
        let rows = names(&t);
        assert_eq!(rows.len(), 1 + HUGE_ROWS + 1, "the folder, its first rows and one more");
        assert_eq!(rows[HUGE_ROWS], "f0199.txt", "the first in the tree's order, not any 200");
        assert_eq!(rows.last().unwrap(), "… 50 more, show all");

        let (buf, _) = draw(&mut t, 40, 3 * HUGE_ROWS as u16);
        let y = (0..buf.area.height).find(|&y| line_at(&buf, y, 40).contains("more, show all")).expect("the more row is drawn");
        let more = line_at(&buf, y, 40);
        assert!(!more.contains("0B"), "no size on it: {more:?}");
        let x = 1 + more.find('…').map(|b| more[..b].chars().count()).unwrap() as u16;
        assert_eq!(buf[(x, y)].fg, theme().muted, "muted: an offer, not a file");

        t.selected = t.rows.len() - 1;
        assert!(t.sel().is_none(), "no edit, rename or delete reaches the folder through its more row");
        t.key(key(KeyCode::Enter));
        let rows = names(&t);
        assert_eq!(rows.len(), 1 + HUGE_ROWS + 50, "Enter on it shows the whole folder");
        assert_eq!(t.rows[t.selected].label, "f0200.txt", "the cursor lands on the first row it hid");
        rm(&d);
    }

    #[test]
    fn a_huge_dir_opens_by_hand_one_level_and_is_never_crawled() {
        let d = tmp("skip");
        fs::create_dir_all(d.join("target/debug")).unwrap();
        fs::write(d.join("target/debug/huge.bin"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["target"], "not folded into target/debug: that would walk it");
        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["target", "debug"], "Enter opens it: a ▸ that does nothing reads as broken");
        rm(&d);
    }

    #[test]
    fn a_letter_types_and_the_actions_answer_to_option() {
        let d = tmp("typing");
        fs::write(d.join("daemon.rs"), "x").unwrap();
        fs::write(d.join("tree.rs"), "x").unwrap();
        let mut t = at(&d);

        // `r` used to reload and `e` to edit; both type now
        ktype(&mut t, "re");
        assert_eq!(t.query, "re");
        assert_eq!(names(&t), ["tree.rs"], "and the list is down to what matches");

        t.key(key(KeyCode::Esc));
        assert_eq!(t.query, "");
        assert_eq!(names(&t), ["daemon.rs", "tree.rs"]);

        // the actions are where the strip says they are
        t.key(key(KeyCode::End));
        let file = d.join("tree.rs");
        assert!(matches!(t.key(alt(KeyCode::Char('e'))), Outcome::OpenInEditor(p, None) if p == file));
        assert!(matches!(t.key(alt(KeyCode::Char('y'))), Outcome::CopyPath(p) if p == file));
        assert!(matches!(t.key(key(KeyCode::Char('e'))), Outcome::None), "bare, it types instead");
        assert_eq!(t.query, "e");
        rm(&d);
    }

    #[test]
    fn a_command_works_on_a_russian_layout_and_a_query_keeps_its_letters() {
        let d = tmp("layout");
        fs::write(d.join("visible.txt"), "v").unwrap();
        fs::write(d.join(".secret"), "s").unwrap();

        let mut t = at(&d);
        // р is what the physical H key prints on a Russian keyboard
        t.key(alt(KeyCode::Char('р')));
        assert_eq!(names(&t), [".secret", "visible.txt"], "a command is read off the key");
        // and a bare letter is what the layout printed, straight into the filter
        t.key(key(KeyCode::Char('ы')));
        assert_eq!(t.query, "ы", "what is typed into a filter stays as typed");
        rm(&d);
    }

    #[test]
    fn h_toggles_hidden_entries() {
        let d = tmp("hidden");
        fs::write(d.join("visible.txt"), "v").unwrap();
        fs::write(d.join(".secret"), "s").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["visible.txt"]);
        t.key(alt(KeyCode::Char('h')));
        assert_eq!(names(&t), [".secret", "visible.txt"]);
        t.key(alt(KeyCode::Char('h')));
        assert_eq!(names(&t), ["visible.txt"]);
        rm(&d);
    }

    #[test]
    fn typing_filters_names_and_esc_clears_then_closes() {
        let d = tmp("filter");
        fs::write(d.join("alpha.rs"), "a").unwrap();
        fs::write(d.join("beta.rs"), "b").unwrap();
        fs::write(d.join("ALPHABET.md"), "c").unwrap();

        let mut t = at(&d);
        // the first character typed starts the filter; nothing has to be pressed first
        ktype(&mut t, "al");
        assert_eq!(t.query, "al");
        // case-insensitive sort: "alpha.rs" < "alphabet.md"
        assert_eq!(names(&t), ["alpha.rs", "ALPHABET.md"], "case doesn't matter");

        // first Esc clears the filter, second closes the panel
        assert!(matches!(t.key(key(KeyCode::Esc)), Outcome::None));
        assert_eq!(names(&t).len(), 3);
        assert!(matches!(t.key(key(KeyCode::Esc)), Outcome::Close));
        rm(&d);
    }

    #[test]
    fn the_filter_finds_a_file_behind_a_huge_folder() {
        let d = tmp("huge-before");
        fs::create_dir_all(d.join("a_assets")).unwrap();
        fs::create_dir(d.join("src")).unwrap();
        for i in 0..25_000 {
            fs::File::create(d.join(format!("a_assets/img{i:05}.png"))).unwrap();
        }
        fs::write(d.join("src/x_target.rs"), "x").unwrap();
        let mut t = at(&d);
        ktype(&mut t, "x_target");
        assert_eq!(names(&t), ["src/x_target.rs"]);
        assert_eq!(t.cut, None);
        rm(&d);
    }

    #[test]
    fn the_filter_lists_files_not_pipes() {
        let d = tmp("fifo");
        fs::File::create(d.join("pipe.txt")).unwrap();
        nix::unistd::mkfifo(&d.join("pipe.fifo"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        let mut t = at(&d);
        ktype(&mut t, "pipe");
        assert_eq!(names(&t), vec!["pipe.txt".to_string()]);
        rm(&d);
    }

    #[test]
    fn a_capped_filter_says_so_and_a_longer_query_looks_again() {
        crate::session::tests::with_temp_state_dir("tree-cap", || {
            let d = tmp("cap");
            for i in 0..=FIND_MAX {
                fs::File::create(d.join(format!("f{i:05}"))).unwrap();
            }
            let mut t = at(&d);
            ktype(&mut t, "f");
            assert_eq!((names(&t).len(), t.cut), (FIND_MAX, Some(Cut::Matches(FIND_MAX))));
            let (buf, area) = draw(&mut t, 40, 24);
            let note = line_at(&buf, 2, area.width);
            assert!(note.trim_start().starts_with(&format!("{FIND_MAX} shown, more not listed ")), "{note:?}");
            let log = fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert_eq!(log.matches(&format!("tree filter: {FIND_MAX} shown, more not listed in {}", d.display())).count(), 1, "{log}");

            // Made after the capped scan: only a new scan can find it, never narrowing the old rows.
            fs::File::create(d.join("fx")).unwrap();
            ktype(&mut t, "x");
            assert_eq!((names(&t), t.cut), (vec!["fx".to_string()], None));
            assert!(t.complete.is_some(), "a result that saw everything narrows the next key");
            rm(&d);
        });
    }

    #[test]
    fn the_filter_covers_the_root_and_never_leaves_it() {
        let d = tmp("deep");
        fs::create_dir_all(d.join("src/render")).unwrap();
        fs::create_dir(d.join("sibling")).unwrap();
        fs::create_dir(d.join("target")).unwrap();
        fs::write(d.join("src/render/needle.rs"), "x").unwrap();
        fs::write(d.join("sibling/needle.rs"), "x").unwrap();
        fs::write(d.join("target/needle.rs"), "x").unwrap();
        fs::write(d.join("other.txt"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["sibling", "src/render", "target", "other.txt"], "directories are collapsed");

        // the cursor is on the first row, and the filter still covers the whole root
        ktype(&mut t, "needle");
        assert_eq!(t.scope, d);
        assert_eq!(names(&t), ["sibling/needle.rs", "src/render/needle.rs"], "target is skipped");

        t.key(key(KeyCode::Esc));
        assert_eq!(names(&t), ["sibling", "src/render", "target", "other.txt"], "Esc brought the tree back");

        // and the same from the last row: where the cursor rests is no business of the search
        t.key(key(KeyCode::End));
        ktype(&mut t, "needle");
        assert_eq!(t.scope, d);
        assert_eq!(names(&t), ["sibling/needle.rs", "src/render/needle.rs"]);
        rm(&d);
    }

    #[test]
    fn a_new_tree_is_the_workspace_itself_with_a_way_out_on_top() {
        let d = tmp("rooted");
        fs::create_dir(d.join("inner")).unwrap();
        fs::write(d.join("f.txt"), "x").unwrap();

        let t = Tree::new(d.clone());
        assert_eq!(t.root, d, "the workspace is the root");
        assert_eq!(names(&t), ["inner", "f.txt"], "its own entries, at no indent");
        assert!(ents(&t).iter().all(|r| r.depth == 0));

        assert!(t.rows[0].up, "the first row leads out");
        assert_eq!(t.rows[0].label, "../");
        assert_eq!(t.rows[0].path, d.parent().unwrap(), "and it points at the parent");
        assert_eq!(t.selected, 1, "the keys land on the first entry, not on the way out");
        assert_eq!(t.top, 0);
        rm(&d);
    }

    #[test]
    fn the_way_out_takes_the_root_with_it() {
        let d = tmp("wayout");
        let inner = d.join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("f.txt"), "x").unwrap();

        let mut t = at(&inner);
        assert_eq!(names(&t), ["f.txt"]);

        t.key(key(KeyCode::Home));
        assert!(t.on_up(), "Home lands on the way out");
        assert!(t.sel().is_none(), "and no action takes it for an entry");
        t.key(key(KeyCode::Enter));
        assert_eq!(t.root, d, "enter on ../ made the parent the root");
        assert_eq!(t.selected_path().unwrap(), inner, "and landed on where we came from");
        rm(&d);
    }

    #[test]
    fn a_chain_of_single_child_directories_is_one_row() {
        let d = tmp("compact");
        fs::create_dir_all(d.join("docs/planning/plans")).unwrap();
        fs::write(d.join("docs/planning/plans/one.md"), "x").unwrap();
        fs::create_dir_all(d.join("src/render")).unwrap();
        fs::write(d.join("src/main.rs"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["docs/planning/plans", "src"], "only the single-child chain folds");

        // expanding the row expands the deepest directory of the chain
        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["docs/planning/plans", "one.md", "src"]);
        assert_eq!(ents(&t)[1].path, d.join("docs/planning/plans/one.md"));
        rm(&d);
    }

    #[test]
    fn eq_zooms_in_and_minus_zooms_out_until_the_filesystem_root() {
        let d = tmp("zoom");
        fs::create_dir(d.join("inner")).unwrap();
        fs::write(d.join("inner/f.txt"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["inner"]);
        t.key(alt(KeyCode::Char('=')));
        assert_eq!(t.root, d.join("inner"));
        assert_eq!(names(&t), ["f.txt"]);

        t.key(key(KeyCode::Backspace));
        assert_eq!(t.root, d, "going up returned the parent");
        assert_eq!(t.selected_path().unwrap(), d.join("inner"), "and landed on where we came from");

        for _ in 0..40 {
            t.key(key(KeyCode::Backspace));
        }
        assert_eq!(t.root, Path::new("/"), "nowhere above the filesystem root");
        assert!(!t.rows.iter().any(|r| r.up), "and no way out of it");
        rm(&d);
    }

    #[test]
    fn a_space_switch_resets_the_zoom_but_the_same_cwd_keeps_it() {
        let d = tmp("space");
        let other = tmp("space-other");
        fs::write(other.join("elsewhere.md"), "e").unwrap();

        let mut t = at(&d);
        assert_eq!(t.root, d, "zoomed in by hand");
        t.set_root(d.clone());
        assert_eq!(t.root, d, "the same directory doesn't reset the zoom");

        t.set_root(other.clone());
        assert_eq!(t.root, other, "switching spaces re-rooted the tree");
        assert_eq!(t.selected_path().unwrap(), other.join("elsewhere.md"));
        rm(&d);
        rm(&other);
    }

    #[test]
    fn a_hidden_directory_we_came_from_is_shown_but_other_hidden_files_are_not() {
        let d = tmp("hiddenpath");
        let deep = d.join(".config/app");
        fs::create_dir_all(&deep).unwrap();
        fs::write(d.join(".other"), "x").unwrap();

        let mut t = at(&deep);
        t.key(key(KeyCode::Backspace));
        t.key(key(KeyCode::Backspace));
        assert_eq!(t.root, d);

        assert_eq!(names(&t), [".config/app"], "visible, and folded with its only child");
        assert_eq!(t.selected_path().unwrap(), deep, "the row that contains where we came from");
        let paths: Vec<&Path> = t.rows.iter().map(|r| r.path.as_path()).collect();
        assert!(!paths.contains(&d.join(".other").as_path()), "other hidden entries stay hidden");
        rm(&d);
    }

    #[test]
    fn grep_lists_matching_lines_with_path_and_number() {
        let d = tmp("grep");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/a.txt"), "первая\nиголка тут\nтретья\n").unwrap();
        fs::write(d.join("b.txt"), "ничего\n").unwrap();
        fs::write(d.join("c.bin"), [b'x', 0, b'\n', b'x']).unwrap();

        let mut t = at(&d);
        t.key(key(KeyCode::End)); // from a file, scope is the whole directory
        t.key(alt(KeyCode::Char('g')));
        ktype(&mut t, "иголка");
        assert!(names(&t).contains(&"sub".to_string()), "before Enter the tree stays put");

        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["sub/a.txt:2: иголка тут"]);
        assert_eq!(ents(&t)[0].line, Some(2));
        assert_eq!(ents(&t)[0].path, d.join("sub/a.txt"));

        // Enter on a hit opens the file at its line
        let want = d.join("sub/a.txt");
        assert!(matches!(
            t.key(key(KeyCode::Enter)),
            Outcome::OpenInEditor(p, Some(2)) if p == want
        ));

        // targeted the found line; nowhere to scroll since the file fits the panel. The second
        // frame is the first with the preview's room measured.
        draw(&mut t, 40, 24);
        draw(&mut t, 40, 24);
        assert_eq!(t.preview_line, Some(2));
        assert_eq!(t.pscroll, 0);

        t.key(key(KeyCode::Esc));
        assert_eq!(names(&t), ["sub", "b.txt", "c.bin"]);
        rm(&d);
    }

    #[test]
    fn the_action_keys_yield_their_outcomes() {
        let d = tmp("actions");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("f.txt"), "x").unwrap();

        let mut t = at(&d);
        let (dir, file) = (d.join("sub"), d.join("f.txt"));

        // on a directory: open with the system, insert, copy, cd, ask
        assert!(matches!(t.key(alt(KeyCode::Char('o'))), Outcome::OpenSystem(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('i'))), Outcome::InsertPath(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('y'))), Outcome::CopyPath(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('d'))), Outcome::Cd(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('p'))), Outcome::NewSpace(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('a'))), Outcome::AskCreate(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('R'))), Outcome::AskRename(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('D'))), Outcome::AskDelete(p) if p == dir));
        assert!(matches!(t.key(alt(KeyCode::Char('e'))), Outcome::None), "a directory isn't for the editor");

        t.key(key(KeyCode::Down));
        assert!(matches!(t.key(alt(KeyCode::Char('e'))), Outcome::OpenInEditor(p, None) if p == file));
        assert!(matches!(t.key(key(KeyCode::Enter)), Outcome::OpenInEditor(p, None) if p == file));
        // cd from a file leads to its directory
        assert!(matches!(t.key(alt(KeyCode::Char('d'))), Outcome::Cd(p) if p == d));
        rm(&d);
    }

    #[test]
    fn one_frame_holds_both_halves_with_a_seam_between_them() {
        let d = tmp("split");
        fs::write(d.join("f.txt"), "one\ntwo\n").unwrap();
        let mut t = at(&d);

        let (buf, pair) = draw_wide(&mut t, 60, 20, 20);
        let prev = pair.preview.expect("wide enough for two halves");
        let seam = pair.seam.expect("and for the seam between them");

        assert_eq!(pair.tree.x, 2, "the tree is the left half, one column of padding in");
        assert_eq!(pair.tree.right() + 1, seam, "a column of padding, then the seam");
        assert_eq!(prev.x, seam + 2, "and the preview starts after it");
        assert!(prev.right() <= 58, "which leaves the frame's own border alone");
        assert_eq!(buf[(seam, 1)].symbol(), "│", "the seam is drawn down the body");
        assert_eq!(buf[(seam, 0)].symbol(), "─", "and leaves the frame's own line whole");
        assert_eq!(buf[(seam, 0)].fg, theme().accent, "which lights up as one piece");

        let title = line_at(&buf, 0, 59);
        assert!(title.contains("files"), "one frame, one title: {title:?}");
        assert!(title.contains("⊡ · ×"), "both buttons on it, as one pair: {title:?}");
        assert_eq!(t.v.btn_zoom.y, 0);
        assert_eq!(t.v.btn_close.y, 0);

        // one strip under both halves, on the last rows inside the frame
        let strip = line_at(&buf, 17, 59);
        assert!(strip.contains("do  ⌥+E edit"), "strip: {strip:?}");

        // the strip has the pair's width now: entries a single column never had room for
        let (wide, _) = draw_wide(&mut t, 120, 20, 34);
        let strip = line_at(&wide, 17, 119);
        assert!(strip.contains("⌥+shift+D delete") && strip.contains("⌥+H hidden"), "strip: {strip:?}");
        rm(&d);
    }

    #[test]
    fn a_directory_preview_counts_and_puts_the_newest_first() {
        let d = tmp("dirprev");
        let sub = d.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("old.txt"), "x").unwrap();
        fs::write(sub.join("new.txt"), "xx").unwrap();
        fs::create_dir(sub.join("inner")).unwrap();
        let two_hours = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        fs::File::options().write(true).open(sub.join("old.txt")).unwrap().set_modified(two_hours).unwrap();

        let mut t = at(&d);
        t.key(key(KeyCode::End));
        assert!(t.sel().is_some_and(|r| r.is_dir), "the directory is selected");

        let (buf, pair) = draw_wide(&mut t, 80, 16, 24);
        let prev = pair.preview.expect("two halves");
        // the path/size line is the preview's own header; the directory's own starts under it
        let head = line_at(&buf, prev.y + 1, 79);
        assert!(head.contains("2 files · 1 folders"), "head: {head:?}");
        assert!(line_at(&buf, prev.y + 3, 79).contains("changed last"));

        let rows: Vec<String> = (4..7).map(|k| line_at(&buf, prev.y + k, 79)).collect();
        assert!(rows[0].contains("new.txt") || rows[0].contains("inner/"), "fresh first: {rows:?}");
        assert!(rows[2].contains("old.txt"), "the oldest goes last: {rows:?}");
        assert!(rows[2].contains("2h"), "with its age beside it: {rows:?}");
        rm(&d);
    }

    #[test]
    fn the_frame_buttons_answer_a_click_on_the_title_row() {
        let d = tmp("buttons");
        fs::write(d.join("f.txt"), "x").unwrap();
        let mut t = at(&d);

        let (_, pair) = draw_wide(&mut t, 60, 16, 20);
        let (close, zoom) = (t.v.btn_close, t.v.btn_zoom);
        assert_eq!(close.y, pair.frame.y, "the buttons sit on the title row");
        assert!(!pair.tree.contains(close.as_position()), "which is outside either half");
        assert!(pair.preview.is_some_and(|p| !p.contains(zoom.as_position())));

        assert!(t.v.clickable(close.x + 1, close.y) && t.v.clickable(zoom.x, zoom.y) && t.v.clickable(t.v.btn_files.x, t.v.btn_files.y));
        let row = t.v.hits[0].0;
        assert!(t.v.clickable(row.x + 1, row.y), "a file's row");
        assert!(!t.v.clickable(pair.tree.x + 1, pair.tree.bottom() - 1), "the empty space under the rows");

        // so the mouse has to be routed by the frame, not by the halves
        let hide = t.mouse(click(close.x + 1, close.y), pair.frame, pair.preview);
        assert!(matches!(hide, Outcome::Hide), "× closes the panel");
        let full = t.mouse(click(zoom.x + 1, zoom.y), pair.frame, pair.preview);
        assert!(matches!(full, Outcome::Zoom), "□ takes the whole screen");
        rm(&d);
    }

    #[test]
    fn the_previews_text_is_dragged_over_and_copied() {
        let d = tmp("psel");
        fs::write(d.join("a.txt"), "hello world\nsecond line\n").unwrap();
        let mut t = at(&d);

        let (_, pair) = draw_wide(&mut t, 70, 16, 24);
        let prev = pair.preview.expect("two halves");
        // the header takes the first row, the gutter the first two columns of the next
        let (x, y) = (prev.x + 2, prev.y + 1);

        t.mouse(click(x, y), pair.tree, Some(prev));
        t.mouse(drag(x + 4, y), pair.tree, Some(prev));
        assert!(t.selecting(), "a drag in the preview is ours until it is released");
        t.mouse(release(x + 4, y), pair.tree, Some(prev));
        assert!(
            !t.selecting(),
            "a finished selection stays on screen but claims no later event, or no pane could be selected in again"
        );

        let (buf, _) = draw_wide(&mut t, 70, 16, 24);
        assert_eq!(buf[(x, y)].bg, theme().accent, "the dragged cells are marked");
        assert_eq!(buf[(x + 5, y)].bg, theme().ground, "and nothing beyond them");
        let text = t.take_copy().expect("the release copied what was under it");
        assert_eq!(text, "hello", "text: {text:?}");
        assert!(t.take_copy().is_none(), "taken once");

        // a click elsewhere in the preview drops it
        t.mouse(click(x, y + 1), pair.tree, Some(prev));
        t.mouse(release(x, y + 1), pair.tree, Some(prev));
        let (buf, _) = draw_wide(&mut t, 70, 16, 24);
        assert_eq!(buf[(x, y)].bg, theme().ground, "the old selection is gone");

        // and so does a press that lands anywhere else, which the app hands down
        t.mouse(click(x, y), pair.tree, Some(prev));
        t.mouse(drag(x + 4, y), pair.tree, Some(prev));
        t.mouse(release(x + 4, y), pair.tree, Some(prev));
        t.clear_selection();
        let (buf, _) = draw_wide(&mut t, 70, 16, 24);
        assert_eq!(buf[(x, y)].bg, theme().ground, "cleared from outside too");

        // a drag that starts in the gutter still takes the text alone
        t.mouse(click(prev.x, y), pair.tree, Some(prev));
        t.mouse(drag(x + 10, y + 1), pair.tree, Some(prev));
        t.mouse(release(x + 10, y + 1), pair.tree, Some(prev));
        let (buf, _) = draw_wide(&mut t, 70, 16, 24);
        assert_eq!(buf[(prev.x, y)].bg, theme().ground, "the line number is not marked");
        assert_eq!(buf[(x, y)].bg, theme().accent, "the text beside it is");
        let text = t.take_copy().expect("copied");
        assert_eq!(text, "hello world\nsecond line", "no numbers in it: {text:?}");
        rm(&d);
    }

    #[test]
    fn the_preview_scrolls_sideways_by_key_and_wheel_and_wrap_is_asked_for() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let d = tmp("sideways");
        fs::write(d.join("a.txt"), format!("{}\n", "x".repeat(200))).unwrap();
        let mut t = at(&d);
        let (_, pair) = draw_wide(&mut t, 70, 16, 20);
        let prev = pair.preview.expect("two halves");
        t.key(key(KeyCode::Tab));
        t.key(key(KeyCode::Right));
        t.key(key(KeyCode::Right));
        assert_eq!(t.phscroll, 2 * SIDE_STEP, "the arrows move the text sideways");
        t.key(key(KeyCode::Left));
        assert_eq!(t.phscroll, SIDE_STEP);

        let wheel = |kind, modifiers| MouseEvent { kind, column: prev.x + 2, row: prev.y + 3, modifiers };
        t.mouse(wheel(MouseEventKind::ScrollDown, KeyModifiers::SHIFT), pair.tree, Some(prev));
        assert_eq!((t.phscroll, t.pscroll), (2 * SIDE_STEP, 0), "shift turns the wheel sideways");
        t.mouse(wheel(MouseEventKind::ScrollLeft, KeyModifiers::NONE), pair.tree, Some(prev));
        assert_eq!(t.phscroll, SIDE_STEP, "and a trackpad's own sideways scroll works as it is");

        assert!(matches!(t.key(key(KeyCode::Char('w'))), Outcome::Wrap), "w asks the app to flip the setting");
        assert!(matches!(t.key(key(KeyCode::Char('v'))), Outcome::View), "v asks the app for the next view");
        t.wrap = true;
        draw_wide(&mut t, 70, 16, 20);
        assert_eq!(t.phscroll, 0, "nothing to scroll sideways while lines wrap");
        rm(&d);
    }

    #[test]
    fn the_preview_half_carries_a_header_and_a_footer() {
        let d = tmp("panel");
        fs::write(d.join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        let mut t = at(&d);

        let (buf, pair) = draw_wide(&mut t, 70, 16, 20);
        let prev = pair.preview.expect("two halves");
        let head = line_at(&buf, prev.y, 69);
        assert!(head.contains("a.rs") && head.contains("rust"), "header: {head:?}");
        let foot = line_at(&buf, prev.bottom() - 1, 69);
        assert!(foot.contains("line 1 of"), "the footer counts lines: {foot:?}");

        // the keys move, and the strip under both halves names the preview's own
        t.key(key(KeyCode::Tab));
        let (buf, _) = draw_wide(&mut t, 70, 16, 20);
        let strip = format!("{}\n{}", line_at(&buf, 13, 69), line_at(&buf, 14, 69));
        assert!(strip.contains("Tab tree") && strip.contains("w wrap"), "strip: {strip:?}");
        rm(&d);
    }

    #[test]
    fn a_click_in_the_preview_rect_gives_it_focus_and_the_wheel_scrolls_it() {
        let d = tmp("mouse2");
        let body: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        fs::write(d.join("long.txt"), body).unwrap();
        let mut t = at(&d);

        let (_, pair) = draw_wide(&mut t, 60, 20, 20);
        let prev = pair.preview.expect("two halves");

        let tr = pair.tree;
        t.mouse(click(prev.x + 2, prev.y + 3), tr, Some(prev));
        assert_eq!(t.focus, Focus::Preview, "a click in the preview half gave it the keys");

        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: prev.x + 2,
            row: prev.y + 3,
            modifiers: KeyModifiers::NONE,
        };
        let before = t.selected;
        t.mouse(wheel, tr, Some(prev));
        assert_eq!(t.pscroll, 3);
        assert_eq!(t.selected, before, "the tree didn't move");
        rm(&d);
    }

    #[test]
    fn the_hint_line_shows_while_the_tree_has_focus() {
        let d = tmp("hint");
        fs::write(d.join("f.txt"), "x").unwrap();
        let mut t = at(&d);

        // the strip takes the last rows inside the frame, under both halves
        let (buf, _) = draw(&mut t, 60, 20);
        let both = format!("{}{}", line_at(&buf, 17, 58), line_at(&buf, 18, 58));
        assert!(both.contains("⌥+E edit"), "the hint sits under the pair");
        // and what the first row could not hold moves to the second, where it used to vanish
        assert!(both.contains("go  a-z filter"), "the second group is named: {both:?}");
        let (wide, _) = draw(&mut t, 140, 20);
        let both = format!("{}{}", line_at(&wide, 17, 138), line_at(&wide, 18, 138));
        assert!(
            both.contains("do") && both.contains("go") && both.contains("move"),
            "with the room for it, every group: {both:?}"
        );

        t.key(key(KeyCode::Tab));
        let (buf, _) = draw(&mut t, 60, 20);
        let line = format!("{}{}", line_at(&buf, 17, 58), line_at(&buf, 18, 58));
        assert!(!line.contains("e edit"), "the tree's own keys are gone: {line:?}");
        assert!(line.contains("z full screen") || line.contains("esc shell"), "the preview says what it answers: {line:?}");
        rm(&d);
    }

    #[test]
    fn hints_are_whole_entries_and_only_while_keys_go_to_the_tree() {
        let d = tmp("hintwidth");
        fs::write(d.join("f.txt"), "x").unwrap();
        let mut t = at(&d);
        t.preview_on = false;

        let (buf, _) = draw(&mut t, 32, 20);
        let line = format!("{}  {}", line_at(&buf, 17, 32), line_at(&buf, 18, 32));
        assert!(line.contains("⌥+E edit"), "{line:?}");
        // every entry that made it is whole: nothing ends in the middle of a word
        for entry in line.split("  ").map(|e| e.trim_end_matches('│').trim()).filter(|e| !e.is_empty()) {
            let whole = ["move", "go", "do"].contains(&entry)
                || COMMAND.iter().any(|r| {
                    entry == r.label || entry == format!("{} {}", r.label, r.word).trim()
                });
            assert!(whole, "cut entry {entry:?} in {line:?}");
        }
        assert_eq!(buf[(1, 0)].fg, theme().border_focus, "the frame is lit");

        t.active = false;
        let (buf, _) = draw(&mut t, 32, 20);
        assert!(!line_at(&buf, 18, 32).contains("edit"), "no hints while keys go elsewhere");
        assert!(!line_at(&buf, 17, 32).contains("edit"), "neither row of it");
        assert_eq!(buf[(1, 0)].fg, theme().border, "and no lit frame");
        rm(&d);
    }

    #[test]
    fn select_path_expands_the_parents() {
        let d = tmp("selpath");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/c.txt"), "x").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["a/b"], "a chain with one child each is one row");
        t.select_path(&d.join("a/b/c.txt"));
        assert_eq!(names(&t), ["a/b", "c.txt"]);
        assert_eq!(t.selected_path().unwrap(), d.join("a/b/c.txt"));
        rm(&d);
    }

    #[test]
    fn create_rename_and_delete_do_the_work_and_refuse_the_bad_cases() {
        let d = tmp("ops");
        let t = at(&d);

        // create
        let f = create(&d, "new.txt").unwrap();
        assert!(f.is_file());
        let sub = create(&d, "sub/").unwrap();
        assert!(sub.is_dir());
        assert!(create(&d, "new.txt").is_err(), "a second time - already there");
        assert!(create(&d, "a/b").is_err(), "separator in the name");
        assert!(create(&d, "").is_err());
        assert!(create(&d, "..").is_err());

        // rename
        let moved = rename(&f, "renamed.txt").unwrap();
        assert_eq!(moved, d.join("renamed.txt"));
        assert!(moved.is_file() && !f.exists());
        assert!(rename(&moved, "../escape").is_err(), "separator in the name");
        assert!(rename(&moved, "sub").is_err(), "name is taken");

        // delete
        assert!(t.delete(&moved).is_ok());
        assert!(!moved.exists());
        fs::write(sub.join("inside"), "x").unwrap();
        assert!(t.delete(&sub).is_ok(), "the directory goes away whole");
        assert!(!sub.exists());
        assert!(t.delete(&d).is_err(), "we don't delete the root");

        let outside = tmp("ops-outside");
        let victim = outside.join("keep.txt");
        fs::write(&victim, "x").unwrap();
        assert!(t.delete(&victim).is_err(), "we don't delete outside the root");
        assert!(victim.exists());
        rm(&d);
        rm(&outside);
    }

    #[test]
    fn the_cursor_takes_the_accent_only_while_the_tree_has_the_keys() {
        let d = tmp("cursor");
        fs::write(d.join("a.txt"), "x").unwrap();
        let mut t = at(&d);
        let th = theme();

        // row 1 is `../`, the cursor starts on the first entry below it
        let (buf, _) = draw(&mut t, 40, 20);
        assert_eq!(buf[(2, 2)].bg, th.accent, "the tree has the keys: its row is the accent");
        assert_eq!(buf[(2, 2)].fg, th.ground, "and its text is the ground under it");

        t.key(key(KeyCode::Tab));
        assert!(t.in_preview(), "the keys moved to the preview");
        let (buf, _) = draw(&mut t, 40, 20);
        assert_eq!((buf[(2, 2)].bg, buf[(2, 2)].fg), (th.slab, th.text), "the keys are elsewhere: the away cursor");

        t.show_rows();
        t.active = false;
        let (buf, _) = draw(&mut t, 40, 20);
        assert_eq!(buf[(2, 2)].bg, th.slab, "a pane holds the keys: the away cursor as well");
        rm(&d);
    }

    #[test]
    fn the_changes_view_lists_what_git_marked_and_the_title_switches_back() {
        let d = tmp("changes");
        fs::create_dir(d.join("src")).unwrap();
        fs::write(d.join("src/app.rs"), "x").unwrap();
        fs::write(d.join("README.md"), "x").unwrap();

        let mut t = at(&d);
        t.set_git_marks(HashMap::from([(d.join("src/app.rs"), 'M'), (d.join("README.md"), 'N')]));
        t.set_view(View::Changes);
        assert_eq!(names(&t), ["src", "app.rs", "README.md"], "the tree the changed files sit in");

        let (buf, pair) = draw_wide(&mut t, 60, 16, 24);
        let title = line_at(&buf, 0, 59);
        assert!(title.contains("files") && title.contains("changes") && title.contains("as a list") && !title.contains("as a tree"), "title: {title:?}");
        assert_eq!(buf[(t.v.btn_changes.x, 0)].fg, theme().accent, "the view in hand takes the accent");

        // one word names what a click does to the shape, and it flips as the shape does
        let shape = t.v.btn_shape;
        t.mouse(click(shape.x, shape.y), pair.frame, pair.preview);
        assert_eq!(names(&t), ["README.md", "src/app.rs"], "one row per file, path order");
        let (buf, _) = draw_wide(&mut t, 60, 16, 24);
        let title = line_at(&buf, 0, 59);
        assert!(title.contains("as a tree") && !title.contains("as a list"), "title: {title:?}");

        // a path longer than the panel keeps its end: the name is what identifies the row
        let deep = d.join("packages/uikit/src/components/TimeStepper/research/rev-1/proto.tsx");
        t.set_git_marks(HashMap::from([(deep, 'N')]));
        let (buf, _) = draw_wide(&mut t, 60, 16, 24);
        let row = line_at(&buf, 1, 26);
        assert!(row.starts_with(" N  ") && row.contains('…') && row.ends_with("proto.tsx"), "{row:?}");

        // the other view answers a click too, and the tree is a tree again
        t.set_git_marks(HashMap::from([(d.join("src/app.rs"), 'M'), (d.join("README.md"), 'N')]));
        let (_, pair) = draw_wide(&mut t, 60, 16, 24);
        let files = t.v.btn_files;
        t.mouse(click(files.x, files.y), pair.frame, pair.preview);
        assert_eq!(t.view(), View::Files);
        assert_eq!(names(&t), ["src", "app.rs", "README.md"], "back to the tree");

        // and an unchanged repository says so rather than looking broken
        t.set_git_marks(HashMap::new());
        t.set_view(View::Changes);
        let (buf, _) = draw_wide(&mut t, 60, 16, 24);
        assert!(line_at(&buf, 1, 59).contains("nothing changed"));
        rm(&d);
    }

    #[test]
    fn the_title_bar_never_cuts_a_word_however_narrow_the_panel() {
        let d = tmp("title-words");
        let mut t = at(&d);
        t.set_view(View::Changes);
        for w in 14..=48 {
            let (buf, _) = draw(&mut t, w, 12);
            let title = line_at(&buf, 0, w);
            let rest = title.replace("as a list", "").replace("changes", "").replace("files", "");
            assert!(!rest.chars().any(|c| c.is_ascii_alphabetic()), "width {w} cut a word: {title:?}");
        }
        rm(&d);
    }

    #[test]
    fn the_changes_tree_holds_a_deleted_file_and_the_state_leads_the_row() {
        let d = tmp("gone");
        fs::create_dir_all(d.join("packages/uikit/src/components")).unwrap();
        fs::write(d.join("packages/uikit/src/components/index.ts"), "x").unwrap();

        let mut t = at(&d);
        let here = d.join("packages/uikit/src/components");
        // deleted: nothing on disk leads to it, the path is all there is
        t.set_git_marks(HashMap::from([(here.join("index.ts"), 'M'), (here.join("gone.ts"), 'D')]));
        t.set_view(View::Changes);
        assert_eq!(
            names(&t),
            ["packages/uikit/src/components", "gone.ts", "index.ts"],
            "the folded chain, then what changed inside it"
        );

        // the cursor takes the accent and drops the tint, so it is parked on the last row
        t.key(key(KeyCode::Down));
        t.key(key(KeyCode::Down));
        let (buf, _) = draw_wide(&mut t, 80, 16, 40);
        // the state leads each row as a column, so the names line up under each other
        assert_eq!(line_at(&buf, 1, 38).trim_end(), " M ▾ packages/uikit/src/components");
        assert_eq!(line_at(&buf, 2, 38).trim_end(), " D     gone.ts", "and no size: the file is gone");
        assert_eq!(line_at(&buf, 3, 38).trim_end(), " M     index.ts");
        assert_eq!(buf[(2, 2)].fg, theme().bad, "the letter takes the state's color");
        assert_eq!(buf[(8, 2)].fg, theme().bad, "and so does the name of a changed file");
        assert_eq!(buf[(6, 1)].fg, theme().text, "a directory keeps the plain text: the mark is its contents'");
        rm(&d);
    }

    #[test]
    fn git_marks_colour_files_and_bubble_up_to_dirs() {
        let d = tmp("marks");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src/changed.rs"), "x").unwrap();
        fs::write(d.join("plain.txt"), "x").unwrap();

        let mut t = at(&d);
        t.set_git_marks(HashMap::from([(d.join("src/changed.rs"), 'M')]));
        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["src", "changed.rs", "plain.txt"]);

        assert_eq!(mark_of(&t.marks, ents(&t)[0]), Some('M'), "the directory inherits the mark");
        assert_eq!(mark_of(&t.marks, ents(&t)[1]), Some('M'));
        assert_eq!(mark_of(&t.marks, ents(&t)[2]), None, "an untouched file has no mark");

        t.key(key(KeyCode::End));
        let (buf, _) = draw(&mut t, 40, 20);
        assert!(line_at(&buf, 2, 20).starts_with(" ▾ src M"), "{:?}", line_at(&buf, 2, 20));
        assert_eq!(buf[(8, 2)].fg, theme().warn, "M is yellow");
        rm(&d);
    }

    /// Hand-assembled 1x1 PNG (signature, IHDR, IDAT, IEND, real CRCs).
    /// Duplicated from preview.rs's tests: `#[cfg(test)]` isn't visible across modules.
    fn png_1x1() -> Vec<u8> {
        let mut v = Vec::from(*b"\x89PNG\r\n\x1a\n");
        v.extend([0, 0, 0, 13]);
        v.extend(*b"IHDR");
        v.extend([0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        v.extend([0x90, 0x77, 0x53, 0xde]);
        v.extend([0, 0, 0, 12]);
        v.extend(*b"IDAT");
        v.extend([0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00]);
        v.extend([0x18, 0xdd, 0x8d, 0xb0]);
        v.extend([0, 0, 0, 0]);
        v.extend(*b"IEND");
        v.extend([0xae, 0x42, 0x60, 0x82]);
        v
    }

    #[test]
    fn a_stale_result_is_dropped_and_a_new_preview_asks_for_its_png_again() {
        use crate::preview::Need;
        let d = tmp("need-again");
        let jpg = d.join("a.jpg");
        // SOI, then a start-of-frame: 3x2, enough for the size from the header.
        fs::write(&jpg, [0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 0, 2, 0, 3, 0]).unwrap();
        let T { t: mut tree, .. } = at(&d);
        let mut v = TreeView::default();
        let old = tree.wanted().unwrap();
        assert!(tree.set_preview(&mut v, &old, load(&jpg, false)));
        assert_eq!(tree.next_need(), Some(Need::Png(jpg.clone())));
        assert_eq!(tree.next_need(), None, "asked once");

        tree.reload();
        let new = tree.wanted().unwrap();
        assert_ne!(old, new, "a reload wants the file read again");
        // One turn takes the PNG made for the old preview, a load for the old selection, then the new load.
        assert!(tree.give_png(&jpg, None));
        assert!(!tree.set_preview(&mut v, &old, load(&jpg, false)), "a result for what is no longer wanted is dropped");
        assert!(tree.set_preview(&mut v, &new, load(&jpg, false)));
        assert_eq!(tree.next_need(), Some(Need::Png(jpg.clone())), "the new preview asks again");
        rm(&d);
    }

    #[test]
    fn a_png_goes_to_the_terminal_once_and_is_cleared_when_we_leave_it() {
        let d = tmp("img");
        fs::write(d.join("a.png"), png_1x1()).unwrap();
        fs::write(d.join("b.txt"), "text\n").unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["a.png", "b.txt"]);

        draw(&mut t, 40, 24);
        let first = t.take_image_bytes();
        assert!(first.starts_with(&crate::preview::clear_image_bytes(IMG_ID)), "clears the previous one first");
        assert!(String::from_utf8_lossy(&first).contains("a=T,f=100,i=1"), "png transfer");

        draw(&mut t, 40, 24);
        assert!(t.take_image_bytes().is_empty(), "second frame of the same image - nothing");

        // moved to text: image needs clearing
        t.key(key(KeyCode::Down));
        draw(&mut t, 40, 24);
        assert_eq!(t.take_image_bytes(), crate::preview::clear_image_bytes(IMG_ID));
        draw(&mut t, 40, 24);
        assert!(t.take_image_bytes().is_empty(), "nothing left to send while on text");
        rm(&d);
    }

    #[test]
    fn leaving_pixels_or_the_preview_clears_the_image_once() {
        use crate::preview::{clear_image_bytes, ImageView};
        let d = tmp("img-views");
        fs::write(d.join("a.png"), png_1x1()).unwrap();
        let mut t = at(&d);

        draw(&mut t, 40, 24);
        assert!(String::from_utf8_lossy(&t.take_image_bytes()).contains("a=T"), "pixels by default");

        t.image_view = ImageView::Blocks { hint: crate::preview::BlockHint::Terminal };
        draw(&mut t, 40, 24);
        assert_eq!(t.take_image_bytes(), clear_image_bytes(IMG_ID), "switching to blocks deletes the image");
        draw(&mut t, 40, 24);
        assert!(t.take_image_bytes().is_empty(), "blocks send no Kitty bytes");

        t.image_view = ImageView::Pixels;
        draw(&mut t, 40, 24);
        assert!(String::from_utf8_lossy(&t.take_image_bytes()).contains("a=T"), "back to pixels sends it again");

        // Preview off: the frame holds the tree alone, and the image must go.
        t.preview_on = false;
        let area = Rect::new(0, 0, 40, 24);
        let mut buf = Buffer::empty(area);
        t.draw_pair(&mut buf, area, &theme(), 36);
        assert_eq!(t.take_image_bytes(), clear_image_bytes(IMG_ID));
        t.draw_pair(&mut buf, area, &theme(), 36);
        assert!(t.take_image_bytes().is_empty(), "cleared once");
        assert_eq!(t.v.btn_zoom, Rect::ZERO, "no full-screen button without a preview");
        t.key(key(KeyCode::Tab));
        assert_eq!(t.focus, Focus::Tree, "Tab has no preview to go to");

        // A new client has no image: it is sent again.
        t.key(key(KeyCode::Esc));
        t.preview_on = true;
        draw(&mut t, 40, 24);
        assert!(!t.take_image_bytes().is_empty());
        t.forget_image();
        draw(&mut t, 40, 24);
        assert!(String::from_utf8_lossy(&t.take_image_bytes()).contains("a=T"), "resent after attach");
        rm(&d);
    }

    #[test]
    fn unreadable_dir_gets_a_warning_marker_not_a_panic() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("perm");
        let locked = d.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("inside"), "x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // running as root bypasses the permission check; nothing to test then
        if fs::read_dir(&locked).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            rm(&d);
            return;
        }

        let mut t = at(&d);
        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["locked"]);
        assert!(ents(&t)[0].err, "the row is marked with ⚠");

        let (buf, _) = draw(&mut t, 30, 12);
        assert!(line_at(&buf, 2, 30).contains('⚠'), "⚠ is drawn on the directory's row");
        assert!(line_at(&buf, 2, 30).contains("no access"), "and says it in words: {:?}", line_at(&buf, 2, 30));

        // before it is ever expanded: the cursor landing on it is enough
        let mut t = at(&d);
        assert!(!ents(&t)[0].err);
        let (buf, pair) = draw_wide(&mut t, 90, 12, 40);
        assert!(ents(&t)[0].err, "the preview tried it, so the row knows");
        let prev = pair.preview.expect("two halves");
        let said: String = (prev.y..prev.bottom()).map(|y| line_at(&buf, y, 90)).collect();
        assert!(said.contains("can't open this folder: you have no") && said.contains("permission to read it"), "{said}");
        let (buf, _) = draw_wide(&mut t, 90, 12, 40);
        assert!(line_at(&buf, 2, 45).contains("no access"), "from the next frame on: {:?}", line_at(&buf, 2, 45));

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        rm(&d);
    }

    #[test]
    fn a_folder_macos_keeps_private_says_where_to_allow_it() {
        let private = crate::preview::why_unreadable(&std::io::Error::from_raw_os_error(1));
        assert!(private.contains("Privacy & Security") && private.contains("Full Disk Access"), "{private}");
        assert_eq!(crate::preview::why_unreadable(&std::io::Error::from_raw_os_error(13)), "you have no permission to read it");
        assert!(crate::preview::why_unreadable(&std::io::Error::from_raw_os_error(2)).contains("No such file"), "anything else as the OS says it");
    }

    #[test]
    fn symlinked_dir_expands_and_symlinked_file_stays_a_file() {
        use std::os::unix::fs::symlink;
        let d = tmp("symdir");
        let real = d.join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("inner.txt"), "x").unwrap();
        fs::write(d.join("plain.txt"), "p").unwrap();
        symlink(&real, d.join("link")).unwrap();
        symlink(d.join("plain.txt"), d.join("linkfile")).unwrap();

        let mut t = at(&d);
        // a symlink to a directory sorts with directories, one to a file with files
        assert_eq!(names(&t), ["link", "real", "linkfile", "plain.txt"]);
        assert!(ents(&t)[0].is_dir && ents(&t)[0].link);
        assert!(!ents(&t)[2].is_dir && ents(&t)[2].link);

        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["link", "inner.txt", "real", "linkfile", "plain.txt"]);

        // → is drawn after the name, dimmed
        t.key(key(KeyCode::End));
        let (buf, _) = draw(&mut t, 30, 16);
        let row = line_at(&buf, 2, 12);
        assert!(row.starts_with(" ▾ link →"), "row: {row:?}");
        assert_eq!(buf[(9, 2)].fg, theme().faint);
        rm(&d);
    }

    #[test]
    fn a_symlink_loop_is_listed_but_never_expands() {
        use std::os::unix::fs::symlink;
        let d = tmp("symloop");
        fs::write(d.join("a.txt"), "a").unwrap();
        symlink(&d, d.join("loop")).unwrap();

        let mut t = at(&d);
        assert_eq!(names(&t), ["loop", "a.txt"]);
        t.key(key(KeyCode::Enter));
        assert_eq!(names(&t), ["loop", "a.txt"], "we don't walk into a symlink to an ancestor");
        assert!(!ents(&t)[0].open);
        rm(&d);
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let d = tmp("tiny");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("f.txt"), "hello").unwrap();

        let mut t = at(&d);
        for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 3), (3, 4), (5, 5), (1, 20), (20, 1)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            t.draw_pair(&mut buf, area, &theme(), w);
        }
        // a buffer smaller than the requested area
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        t.draw_pair(&mut buf, Rect::new(0, 0, 40, 30), &theme(), 20);
        rm(&d);
    }

    #[test]
    fn click_selects_a_row_and_expands_a_dir() {
        let d = tmp("click");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/inner.txt"), "x").unwrap();
        fs::write(d.join("top.txt"), "t").unwrap();

        let mut t = at(&d);
        draw(&mut t, 30, 16);
        assert_eq!(names(&t), ["sub", "top.txt"]);
        let tr = t.v.tree_area;

        // row 0 is `../`, the entries follow it
        let (rect, k) = t.v.hits[2];
        assert_eq!(k, 2);
        t.mouse(click(rect.x + 1, rect.y), tr, None);
        assert_eq!(t.selected, 2);
        assert_eq!(t.selected_path().unwrap(), d.join("top.txt"));

        // clicking a directory expands it too
        let (rect, _) = t.v.hits[1];
        t.mouse(click(rect.x + 1, rect.y), tr, None);
        assert_eq!(t.selected, 1);
        assert_eq!(names(&t), ["sub", "inner.txt", "top.txt"]);

        // and clicking the way out takes the root with it
        draw(&mut t, 30, 16);
        let (rect, _) = t.v.hits[0];
        t.mouse(click(rect.x + 1, rect.y), tr, None);
        assert_eq!(t.root, d.parent().unwrap());
        rm(&d);
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let d = tmp("ends");
        for i in 0..5 {
            fs::write(d.join(format!("f{i}.txt")), "x").unwrap();
        }
        let mut t = at(&d);
        t.key(key(KeyCode::End));
        assert_eq!(t.selected, 5, "five files, and `../` above them");
        t.key(key(KeyCode::Home));
        assert_eq!(t.selected, 0, "Home is the way out, one press from leaving");
        rm(&d);
    }

    #[test]
    fn tab_moves_focus_and_wheel_scrolls_the_half_under_the_pointer() {
        let d = tmp("focus");
        fs::write(d.join("a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        fs::write(d.join("b.txt"), "x").unwrap();

        let mut t = at(&d);
        draw(&mut t, 30, 20);
        assert_eq!(t.focus, Focus::Tree);
        t.key(key(KeyCode::Tab));
        assert_eq!(t.focus, Focus::Preview);
        t.key(key(KeyCode::Down));
        assert_eq!(t.pscroll, 1);
        t.key(key(KeyCode::Tab));
        assert_eq!(t.focus, Focus::Tree);

        // the wheel over the bottom half scrolls the preview, not the tree
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: t.v.prev_area.x,
            row: t.v.prev_area.y,
            modifiers: KeyModifiers::NONE,
        };
        let (before, tr, pr) = (t.selected, t.v.tree_area, t.v.prev_area);
        t.mouse(wheel, tr, Some(pr));
        assert_eq!(t.selected, before);
        assert_eq!(t.pscroll, 4);
        rm(&d);
    }

    #[test]
    fn reload_picks_up_new_files() {
        let d = tmp("reload");
        fs::write(d.join("one.txt"), "1").unwrap();
        let mut t = at(&d);
        assert_eq!(names(&t), ["one.txt"]);

        fs::write(d.join("two.txt"), "2").unwrap();
        assert_eq!(names(&t), ["one.txt"], "without r the tree doesn't refresh");
        t.key(alt(KeyCode::Char('r')));
        assert_eq!(names(&t), ["one.txt", "two.txt"]);
        rm(&d);
    }
}
