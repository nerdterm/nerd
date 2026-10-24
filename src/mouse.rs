//! Mouse parsing: what the multiplexer keeps for itself, and what goes to the program in the pane.

use crate::layout::{Border, PaneId};
use crate::term::WheelRouting;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// How many lines one wheel click gives. Three is a habit from xterm.
const WHEEL_LINES: usize = 3;

/// Registry of clickable areas, rebuilt every frame since ratatui is immediate-mode and remembers no widgets.
#[derive(Default)]
pub struct Hits {
    pub tabs: Vec<(Rect, usize)>,
    pub new_tab: Option<Rect>,
    /// Status segments in the top row that do something when clicked.
    pub segments: Vec<(Rect, SegmentHit)>,
    pub sidebar_rows: Vec<(Rect, PaneId)>,
    /// Rows of the top block: spaces.
    pub sidebar_spaces: Vec<(Rect, usize)>,
    /// The ▸/▾ arrow on a worktree group's parent.
    pub sidebar_group_toggles: Vec<(Rect, usize)>,
    pub sidebar_new: Option<Rect>,
    pub sidebar_menu: Option<Rect>,
    /// « in the sidebar's bottom line, » in the collapsed rail's.
    pub sidebar_toggle: Option<Rect>,
    /// An agent's `◦N` subagent count.
    pub sidebar_subagents: Vec<(Rect, PaneId)>,
    /// A subagent's row: (rect, its agent's pane, its id).
    pub sidebar_subagent_rows: Vec<(Rect, PaneId, String)>,
    /// Border between sidebar blocks: dragging it resizes the height.
    /// Sidebar's right edge: dragging it resizes the width.
    pub sidebar_divider: Option<Rect>,
    /// File tree's left edge: dragging it resizes its width.
    pub tree_divider: Option<Rect>,
    /// Preview column's left edge, when it has its own column: dragging it resizes its width.
    pub preview_divider: Option<Rect>,
    pub panes: Vec<PaneHit>,
    pub borders: Vec<Border>,
    /// The notice card, and its ×.
    pub notice: Option<Rect>,
    pub notice_close: Option<Rect>,
}

pub struct PaneHit {
    pub id: PaneId,
    pub rect: Rect,        // the whole pane including its frame
    pub inner: Rect,       // the content
    pub wants_mouse: bool, // the program inside asked for mouse events
    pub mouse_mode: crate::term::MouseProtocolMode, // and which ones exactly
    pub wheel: WheelRouting,
}

/// What a click on a segment does.
#[derive(Debug, Clone, PartialEq)]
pub enum SegmentHit {
    /// A plugin's menu command: (plugin, command id).
    Command(String, String),
    /// nerd's battery: percent or time.
    Battery,
    /// nerd's clock: the time, or the date and the time.
    Clock,
}

impl Hits {
    pub fn clear(&mut self) {
        self.tabs.clear();
        self.new_tab = None;
        self.segments.clear();
        self.sidebar_rows.clear();
        self.sidebar_spaces.clear();
        self.sidebar_group_toggles.clear();
        self.sidebar_new = None;
        self.sidebar_menu = None;
        self.sidebar_toggle = None;
        self.sidebar_subagents.clear();
        self.sidebar_subagent_rows.clear();
        self.sidebar_divider = None;
        self.tree_divider = None;
        self.preview_divider = None;
        self.panes.clear();
        self.borders.clear();
        self.notice = None;
        self.notice_close = None;
    }
}

/// What to do with a mouse event.
#[derive(Debug)]
pub enum Action {
    None,
    /// The notice card's ×.
    NoticeClose,
    /// The notice card: its whole text in a window.
    NoticeOpen,
    SelectTab(usize),
    NewTab,
    Segment(SegmentHit),
    FocusPane(PaneId),
    /// Start dragging a divider; which one is remembered by `Router` itself until release.
    BeginDrag,
    /// Dragging a divider: new ratio for the node along the path.
    Drag { path: Vec<bool>, ratio: f32 },
    SidebarWidth(u16),
    /// The tree's left edge moved here; width is computed by the app, which knows the right edge.
    TreeEdge(u16),
    PreviewEdge(u16),
    SelectSpace(usize),
    /// A space dragged over another one's row takes its place.
    MoveSpace { from: usize, to: usize },
    /// The "new" button in the spaces block's footer.
    NewSpace,
    /// Collapse the sidebar to its rail, or expand it back.
    ToggleSidebar,
    /// Open or close an agent's subagent list in the sidebar.
    ToggleSubagents(PaneId),
    /// Open a subagent's run: its agent's pane and its id.
    OpenRun(PaneId, String),
    /// The "menu" button: open the general menu at this point.
    OpenMenu(u16, u16),
    /// Scrolling a pane's own scrollback.
    Scroll { id: PaneId, lines: isize },
    /// Bytes ready to send to the program; also carries focus to `id`, with no separate action for it.
    ToChild { id: PaneId, bytes: Vec<u8> },
    /// Always emitted on Ctrl+click, whether a link is there or not; the caller checks via `links::link_at`
    /// and replays the click as a normal one if not (see `route_pane`).
    OpenLink { id: PaneId, row: u16, col: u16 },
    /// Start a selection; also carries focus to `id`, no separate `FocusPane` for this click.
    SelectStart { id: PaneId, row: u16, col: u16 },
    /// Extend a selection; coordinates are already clamped to the pane's content.
    SelectExtend { id: PaneId, row: u16, col: u16 },
    SelectWord { id: PaneId, row: u16, col: u16 },
    /// Drag after a double click: the caller must snap both ends via `select::word_at`, the router can't see the screen.
    SelectExtendWord { id: PaneId, row: u16, col: u16 },
    SelectLine { id: PaneId, row: u16 },
    /// Drag after a triple click: boundaries are pushed to whole lines.
    SelectExtendLine { id: PaneId, row: u16 },
    ToggleGroup(usize),
    SelectEnd,
}

/// State of a drag between press and release.
#[derive(Default)]
pub struct DragState {
    /// The divider and its node's area, captured once on press since the pointer leaves the border
    /// but the ratio must keep being computed against that same node until release.
    border: Option<(Border, Rect)>,
    sidebar: bool,
    tree: bool,
    preview: bool,
    /// The space being dragged, by its current index.
    space: Option<usize>,
}

pub struct Router {
    pub drag: DragState,
    pub sidebar_width: u16,
    /// Scroll direction while a selection drags past a pane's edge: -1 up, +1 down, 0 nowhere.
    /// The caller polls this every tick, since no more mouse events arrive while parked past the edge.
    pub autoscroll: i8,
    /// The pane and its content area for a selection drag, captured on press for the same
    /// reason as `border`: the pointer leaves its bounds but coordinates must still clamp to it.
    select: Option<(PaneId, Rect)>,
    /// The last press in a pane and which click number it was, for counting double/triple clicks.
    last_click: Option<(PaneId, u16, u16, std::time::Instant, u8)>,
    /// Which click number started the current drag: it keeps dragging in that unit until release.
    drag_kind: u8,
    /// Whether there's a selection to drop on an outside click; public because a key can also clear it.
    pub has_selection: bool,
    /// The last press dropped the selection on its way; the app clears it and takes this back.
    pub dropped: bool,
}

/// Double-click threshold. 350ms, same as macOS's default.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(350);

impl Router {
    pub fn new() -> Self {
        Self {
            drag: DragState::default(),
            sidebar_width: 24,
            autoscroll: 0,
            select: None,
            last_click: None,
            drag_kind: 1,
            has_selection: false,
            dropped: false,
        }
    }

    /// The single parsing entry point; check order matters, UI elements before pane content, first match wins.
    pub fn route(&mut self, ev: MouseEvent, hits: &Hits) -> Action {
        // A press outside the text drops the selection and still does what it points at;
        // a press in another pane starts a new selection instead.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self.has_selection
            && !selectable_at(hits, ev.column, ev.row)
        {
            self.has_selection = false;
            self.dropped = true;
        }
        // Sidebar buttons and rows are checked first: they sit on top of an area panes would otherwise intercept.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            let (x, y) = (ev.column, ev.row);
            // The notice card lies over a pane: its clicks are its own.
            if hits.notice_close.is_some_and(|r| hit(r, x, y)) {
                return Action::NoticeClose;
            }
            if hits.notice.is_some_and(|r| hit(r, x, y)) {
                return Action::NoticeOpen;
            }
            if let Some(r) = hits.sidebar_menu
                && hit(r, x, y) {
                    // the button's own corner, not the cursor's: the same button must open it in one place
                    return Action::OpenMenu(r.x, r.y);
                }
            if let Some(r) = hits.sidebar_new
                && hit(r, x, y) {
                    return Action::NewSpace;
                }
            if let Some(r) = hits.sidebar_toggle
                && hit(r, x, y) {
                    return Action::ToggleSidebar;
                }
            if let Some((_, id)) = hits.sidebar_subagents.iter().find(|(r, _)| hit(*r, x, y)) {
                return Action::ToggleSubagents(*id);
            }
            if let Some((_, pane, id)) = hits.sidebar_subagent_rows.iter().find(|(r, ..)| hit(*r, x, y)) {
                return Action::OpenRun(*pane, id.clone());
            }
            if let Some((_, i)) = hits.sidebar_group_toggles.iter().find(|(r, _)| hit(*r, x, y)) {
                return Action::ToggleGroup(*i);
            }
            if let Some((_, i)) = hits.sidebar_spaces.iter().find(|(r, _)| hit(*r, x, y)) {
                self.drag.space = Some(*i);
                return Action::SelectSpace(*i);
            }
        }
        if let Some(from) = self.drag.space {
            match ev.kind {
                MouseEventKind::Drag(_) => {
                    let over = hits.sidebar_spaces.iter().find(|(r, _)| hit(*r, ev.column, ev.row));
                    return match over {
                        Some(&(_, to)) if to != from => {
                            self.drag.space = Some(to);
                            Action::MoveSpace { from, to }
                        }
                        _ => Action::None,
                    };
                }
                MouseEventKind::Up(_) => {
                    self.drag.space = None;
                    return Action::None;
                }
                // A release lost outside the window must not leave the next drag moving spaces.
                MouseEventKind::Down(_) => self.drag.space = None,
                _ => {}
            }
        }
        let (x, y) = (ev.column, ev.row);

        // A drag or selection in progress keeps events for itself: the pointer often leaves
        // the divider or the pane's edge, and hit-testing wouldn't find it there anymore.
        if let Some(a) = self.route_drag(ev, x, y) {
            return a;
        }
        if let Some(a) = self.route_select(ev) {
            return a;
        }

        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, s)) = hits.segments.iter().find(|(r, _)| hit(*r, x, y)) {
                return Action::Segment(s.clone());
            }
            for (r, i) in &hits.tabs {
                if hit(*r, x, y) {
                    return Action::SelectTab(*i);
                }
            }
            if hits.new_tab.is_some_and(|r| hit(r, x, y)) {
                return Action::NewTab;
            }
            if let Some(r) = hits.tree_divider
                && near(r, x, y)
            {
                self.drag.tree = true;
                return Action::None;
            }
            if let Some(r) = hits.preview_divider
                && near(r, x, y)
            {
                self.drag.preview = true;
                return Action::None;
            }
            if hits.sidebar_divider.is_some_and(|r| near(r, x, y)) {
                // A press only starts the drag; jumping to the click's position would ignore the set width.
                self.drag.sidebar = true;
                return Action::None;
            }
            for (r, id) in &hits.sidebar_rows {
                if hit(*r, x, y) {
                    return Action::FocusPane(*id);
                }
            }
            // Borders get an extra cell of slack: a strip one character wide is nearly impossible to hit.
            let border = hits
                .borders
                .iter()
                .find(|b| near(b.rect, x, y))
                .cloned();
            if let Some(b) = border {
                let area = node_area(hits, &b);
                self.drag.border = Some((b, area));
                return Action::BeginDrag;
            }
        }

        self.route_pane(ev, x, y, hits)
    }

    /// While a border, edge, or divider drag is in progress, events belong to the router, not the pointer's target.
    pub fn dragging(&self) -> bool {
        self.drag.border.is_some()
            || self.drag.sidebar
            || self.drag.tree
            || self.drag.preview
            || self.drag.space.is_some()
    }

    fn route_drag(&mut self, ev: MouseEvent, x: u16, y: u16) -> Option<Action> {
        if self.drag.border.is_none() && !self.drag.sidebar && !self.drag.tree && !self.drag.preview {
            return None;
        }
        match ev.kind {
            MouseEventKind::Up(_) => {
                self.drag = DragState::default();
                Some(Action::None)
            }
            MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                if let Some((b, area)) = &self.drag.border {
                    return Some(Action::Drag {
                        path: b.path.clone(),
                        ratio: ratio_at(b.rect, *area, x, y),
                    });
                }
                if self.drag.tree {
                    return Some(Action::TreeEdge(x));
                }
                if self.drag.preview {
                    return Some(Action::PreviewEdge(x));
                }
                let w = x.clamp(crate::config::SIDEBAR_WIDTH.0, crate::config::SIDEBAR_WIDTH.1);
                self.sidebar_width = w;
                Some(Action::SidebarWidth(w))
            }
            // Wheel and other events during a drag are swallowed so a stray click doesn't steal focus.
            _ => Some(Action::None),
        }
    }

    fn route_select(&mut self, ev: MouseEvent) -> Option<Action> {
        let (id, inner) = self.select?;
        match ev.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                self.autoscroll = if ev.row < inner.y {
                    -1
                } else if ev.row >= inner.bottom() {
                    1
                } else {
                    0
                };
                let (row, col) = pane_coords(inner, ev.column, ev.row);
                Some(match self.drag_kind {
                    2 => Action::SelectExtendWord { id, row, col },
                    3 => Action::SelectExtendLine { id, row },
                    _ => Action::SelectExtend { id, row, col },
                })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.select = None;
                self.drag_kind = 1;
                self.autoscroll = 0;
                Some(Action::SelectEnd)
            }
            _ => None,
        }
    }

    fn route_pane(&mut self, ev: MouseEvent, x: u16, y: u16, hits: &Hits) -> Action {
        let Some(p) = hits.panes.iter().find(|p| hit(p.rect, x, y)) else {
            return Action::None;
        };
        let (id, inner, wants_mouse, wheel) = (p.id, p.inner, p.wants_mouse, p.wheel);
        // Claude Code enables mode 1000 (presses/wheel only); a stream of plain motion
        // breaks its input parsing, printing stray coordinates into the prompt.
        let wants_mouse = wants_mouse && mode_allows(p.mouse_mode, ev.kind);

        if let Some(up) = wheel_up(ev.kind) {
            return match wheel {
                WheelRouting::ToChild => match encode_for_child(ev, inner, CHILD_ENCODING) {
                    Some(bytes) => Action::ToChild { id, bytes },
                    None => Action::None,
                },
                WheelRouting::Arrows { app_cursor } => Action::ToChild {
                    id,
                    bytes: arrows_for_wheel(up, WHEEL_LINES, app_cursor),
                },
                // Sign matches `Term::scroll_by`: minus into history, plus toward live.
                WheelRouting::Scrollback => Action::Scroll {
                    id,
                    lines: if up {
                        -(WHEEL_LINES as isize)
                    } else {
                        WHEEL_LINES as isize
                    },
                },
            };
        }

        // Ahead of both selection and the pane's program, or Ctrl+click on a link
        // in an interactive program would just go to its stdin.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && ev.modifiers.contains(KeyModifiers::CONTROL)
            && hit(inner, x, y)
        {
            let (row, col) = pane_coords(inner, x, y);
            return Action::OpenLink { id, row, col };
        }

        if wants_mouse
            && let Some(bytes) = encode_for_child(ev, inner, CHILD_ENCODING)
        {
            return Action::ToChild { id, bytes };
        }

        // The program didn't ask for the mouse, so the left button starts a selection here.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) && hit(inner, x, y) {
            let (row, col) = pane_coords(inner, x, y);
            let repeat = ev.modifiers.is_empty()
                && self.last_click.is_some_and(|(lid, lrow, lcol, at, _)| {
                    lid == id
                        && at.elapsed() <= DOUBLE_CLICK
                        && lrow.abs_diff(row) <= 1
                        && lcol.abs_diff(col) <= 1
                });
            let count = match self.last_click {
                Some((_, _, _, _, n)) if repeat && n < 3 => n + 1,
                _ => 1,
            };
            self.last_click = Some((id, row, col, std::time::Instant::now(), count));
            self.select = Some((id, inner));
            self.drag_kind = count;
            self.has_selection = true;
            self.autoscroll = 0;
            return match count {
                2 => Action::SelectWord { id, row, col },
                3 => Action::SelectLine { id, row },
                _ => Action::SelectStart { id, row, col },
            };
        }

        // Only a press gives focus: passing the pointer through shouldn't steal the keyboard.
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            Action::FocusPane(id)
        } else {
            Action::None
        }
    }
}

/// A point in the pane content's coordinates, clamped to its edges.
fn pane_coords(inner: Rect, x: u16, y: u16) -> (u16, u16) {
    let last = |start: u16, end: u16| end.saturating_sub(1).max(start);
    let col = x.clamp(inner.x, last(inner.x, inner.right())) - inner.x;
    let row = y.clamp(inner.y, last(inner.y, inner.bottom())) - inner.y;
    (row, col)
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// ceiling: SGR hardcoded for every pane instead of a per-pane field; add one to PaneHit if X10 is ever needed.
const CHILD_ENCODING: crate::term::MouseProtocolEncoding = crate::term::MouseProtocolEncoding::Sgr;

/// Encodes an event for the program inside a pane: coordinates are
/// translated into the pane's own system and made 1-based.
pub fn encode_for_child(
    ev: MouseEvent,
    inner: Rect,
    encoding: crate::term::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    // Outside the content -- on the frame or the title -- don't hand it to the program.
    if ev.column < inner.x
        || ev.row < inner.y
        || ev.column >= inner.right()
        || ev.row >= inner.bottom()
    {
        return None;
    }
    let col = ev.column - inner.x;
    let row = ev.row - inner.y;
    let mods = modifiers(ev.modifiers);

    match encoding {
        crate::term::MouseProtocolEncoding::Sgr => {
            let event = terminput::Event::Mouse(terminput::MouseEvent {
                kind: kind_for(ev.kind),
                column: col,
                row,
                modifiers: mods,
            });
            // terminput's Xterm encoding is exactly SGR (`ESC [ < ... M/m`),
            // with coordinates translated to 1-based internally.
            let mut buf = [0u8; 32];
            let n = event.encode(&mut buf, terminput::Encoding::Xterm).ok()?;
            Some(buf[..n].to_vec())
        }
        // terminput doesn't know the older encodings: there a coordinate
        // is a printable character, and the sequence has to be built by hand.
        enc => x10(button_code(ev.kind, mods), col, row, enc),
    }
}

/// What the program receives in each xterm mode: 1000 -- presses, releases and wheel,
/// 1002 -- plus drag with a button held, 1003 -- everything, including plain motion.
pub fn mode_allows(mode: crate::term::MouseProtocolMode, kind: MouseEventKind) -> bool {
    use crate::term::MouseProtocolMode as M;
    match kind {
        MouseEventKind::Down(_) => mode != M::None,
        MouseEventKind::Up(_)
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => matches!(mode, M::PressRelease | M::ButtonMotion | M::AnyMotion),
        MouseEventKind::Drag(_) => matches!(mode, M::ButtonMotion | M::AnyMotion),
        MouseEventKind::Moved => mode == M::AnyMotion,
    }
}

/// Wheel on the alternate screen without mouse tracking becomes arrow
/// keys, the way less and man are paged.
pub fn arrows_for_wheel(up: bool, lines: usize, app_cursor: bool) -> Vec<u8> {
    let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
    crate::keys::cursor_keys(seq.to_vec(), app_cursor).repeat(lines)
}

fn kind_for(kind: MouseEventKind) -> terminput::MouseEventKind {
    use terminput::MouseEventKind as T;
    use terminput::{MouseButton as TB, ScrollDirection as S};
    let button = |b: MouseButton| match b {
        MouseButton::Left => TB::Left,
        MouseButton::Middle => TB::Middle,
        MouseButton::Right => TB::Right,
    };
    match kind {
        MouseEventKind::Down(b) => T::Down(button(b)),
        MouseEventKind::Up(b) => T::Up(button(b)),
        MouseEventKind::Drag(b) => T::Drag(button(b)),
        MouseEventKind::Moved => T::Moved,
        MouseEventKind::ScrollUp => T::Scroll(S::Up),
        MouseEventKind::ScrollDown => T::Scroll(S::Down),
        MouseEventKind::ScrollLeft => T::Scroll(S::Left),
        MouseEventKind::ScrollRight => T::Scroll(S::Right),
    }
}

fn modifiers(m: crossterm::event::KeyModifiers) -> terminput::KeyModifiers {
    use crossterm::event::KeyModifiers as C;
    // The mouse protocol only knows three bits; the rest of the modifiers are lost.
    let mut out = terminput::KeyModifiers::empty();
    out.set(terminput::KeyModifiers::SHIFT, m.contains(C::SHIFT));
    out.set(terminput::KeyModifiers::ALT, m.contains(C::ALT));
    out.set(terminput::KeyModifiers::CTRL, m.contains(C::CONTROL));
    out
}

/// Button and modifiers packed into one number -- the table from xterm.
fn button_code(kind: MouseEventKind, mods: terminput::KeyModifiers) -> u8 {
    let base: u8 = match kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => 0,
        MouseEventKind::Down(MouseButton::Middle) | MouseEventKind::Up(MouseButton::Middle) => 1,
        MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Up(MouseButton::Right) => 2,
        MouseEventKind::Drag(MouseButton::Left) => 32,
        MouseEventKind::Drag(MouseButton::Middle) => 33,
        MouseEventKind::Drag(MouseButton::Right) => 34,
        MouseEventKind::Moved => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };
    let shift = u8::from(mods.intersects(terminput::KeyModifiers::SHIFT)) * 4;
    let alt = u8::from(mods.intersects(terminput::KeyModifiers::ALT)) * 8;
    let ctrl = u8::from(mods.intersects(terminput::KeyModifiers::CTRL)) * 16;
    base + shift + alt + ctrl
}

/// `ESC [ M Cb Cx Cy`. A coordinate is one printable character offset by
/// 32, so past column 223 (column 2015 in utf8) there's nothing to send.
fn x10(cb: u8, col: u16, row: u16, encoding: crate::term::MouseProtocolEncoding) -> Option<Vec<u8>> {
    let utf8 = matches!(encoding, crate::term::MouseProtocolEncoding::Utf8);
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + cb);
    for v in [col, row] {
        let c = 32u32 + v as u32 + 1;
        if utf8 {
            let ch = char::from_u32(c).filter(|_| c <= 2047)?;
            out.extend_from_slice(ch.encode_utf8(&mut [0u8; 4]).as_bytes());
        } else {
            out.push(u8::try_from(c).ok()?);
        }
    }
    Some(out)
}

fn wheel_up(kind: MouseEventKind) -> Option<bool> {
    match kind {
        MouseEventKind::ScrollUp => Some(true),
        MouseEventKind::ScrollDown => Some(false),
        _ => None,
    }
}

fn hit(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.right() && y < r.bottom()
}

/// A hit test with a one-cell margin around the rect.
pub fn near(r: Rect, x: u16, y: u16) -> bool {
    x.saturating_add(1) >= r.x && y.saturating_add(1) >= r.y && x <= r.right() && y <= r.bottom()
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: a.right().max(b.right()) - x,
        height: a.bottom().max(b.bottom()) - y,
    }
}

/// Whether two rects touch along an edge.
fn adjacent(a: Rect, b: Rect) -> bool {
    let x_touch = a.right() == b.x || b.right() == a.x;
    let y_touch = a.bottom() == b.y || b.bottom() == a.y;
    let x_over = a.x < b.right() && b.x < a.right();
    let y_over = a.y < b.bottom() && b.y < a.bottom();
    (x_touch && y_over) || (y_touch && x_over)
}

/// The area of the node a divider belongs to, never stored: assembled from dividers whose path
/// starts with its path, plus panes adjacent to any of them (every leaf of the node touches one).
/// A pane that wants the mouse itself still counts as "inside": its click goes to
/// the program in `route_pane`, rather than being lost by dropping the selection here.
fn selectable_at(hits: &Hits, x: u16, y: u16) -> bool {
    hits.panes.iter().any(|p| hit(p.inner, x, y))
}

fn node_area(hits: &Hits, border: &Border) -> Rect {
    let dividers: Vec<Rect> = hits
        .borders
        .iter()
        .filter(|b| b.path.starts_with(&border.path))
        .map(|b| b.rect)
        .chain(std::iter::once(border.rect))
        .collect();
    let mut area = dividers.iter().copied().fold(border.rect, union);
    for p in &hits.panes {
        if dividers.iter().any(|d| adjacent(*d, p.rect)) {
            area = union(area, p.rect);
        }
    }
    area
}

/// The node's share to the left (above) the pointer, clamped so the
/// neighboring pane can't collapse to zero.
fn ratio_at(divider: Rect, area: Rect, x: u16, y: u16) -> f32 {
    // ceiling: axis inferred from the divider's shape, not `Border::dir`; wrong only for a one-row node.
    let vertical = divider.width <= divider.height;
    let (pos, start, len) = if vertical {
        (x as f32, area.x as f32, area.width as f32)
    } else {
        (y as f32, area.y as f32, area.height as f32)
    };
    if len < 1.0 {
        return 0.5;
    }
    ((pos - start + 0.5) / len).clamp(0.05, 0.95)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Dir;

    fn ev(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn press(x: u16, y: u16) -> MouseEvent {
        ev(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn ctrl_press(x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..press(x, y)
        }
    }

    fn pane(id: PaneId, rect: Rect, wants_mouse: bool, wheel: WheelRouting) -> PaneHit {
        PaneHit {
            id,
            rect,
            inner: Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2),
            wants_mouse,
            mouse_mode: if wants_mouse { crate::term::MouseProtocolMode::AnyMotion } else { crate::term::MouseProtocolMode::None },
            wheel,
        }
    }

    #[test]
    fn child_gets_only_the_events_its_mode_covers() {
        use crate::term::MouseProtocolMode as M;
        let down = MouseEventKind::Down(MouseButton::Left);
        assert!(mode_allows(M::PressRelease, down));
        assert!(!mode_allows(M::None, down));
        assert!(mode_allows(M::PressRelease, MouseEventKind::ScrollUp));
        assert!(!mode_allows(M::PressRelease, MouseEventKind::Drag(MouseButton::Left)));
        assert!(!mode_allows(M::PressRelease, MouseEventKind::Moved), "Claude Code asks for 1000 -- we don't send it motion");
        assert!(mode_allows(M::ButtonMotion, MouseEventKind::Drag(MouseButton::Left)));
        assert!(!mode_allows(M::ButtonMotion, MouseEventKind::Moved));
        assert!(mode_allows(M::AnyMotion, MouseEventKind::Moved));
    }

    /// Frame: a tab strip on top, two panes side by side, divider at x=40.
    /// A router with the hits a frame would have left it.
    struct R {
        r: Router,
        hits: Hits,
    }

    impl R {
        fn new() -> Self {
            R { r: Router::new(), hits: Hits::default() }
        }

        fn route(&mut self, ev: MouseEvent) -> Action {
            self.r.route(ev, &self.hits)
        }
    }

    impl std::ops::Deref for R {
        type Target = Router;
        fn deref(&self) -> &Router {
            &self.r
        }
    }

    impl std::ops::DerefMut for R {
        fn deref_mut(&mut self) -> &mut Router {
            &mut self.r
        }
    }

    fn router(wants_mouse: bool, wheel: WheelRouting) -> R {
        let mut r = R::new();
        r.hits.tabs.push((Rect::new(0, 0, 10, 1), 0));
        r.hits.tabs.push((Rect::new(10, 0, 10, 1), 1));
        r.hits.new_tab = Some(Rect::new(20, 0, 3, 1));
        r.hits
            .panes
            .push(pane(1, Rect::new(0, 1, 40, 20), wants_mouse, wheel));
        r.hits
            .panes
            .push(pane(2, Rect::new(41, 1, 39, 20), wants_mouse, wheel));
        r.hits.borders.push(Border {
            dir: Dir::Horizontal,
            rect: Rect::new(40, 1, 1, 20),
            path: vec![],
        });
        r
    }

    #[test]
    fn tab_click_selects_tab() {
        let mut r = router(false, WheelRouting::Scrollback);
        assert!(matches!(r.route(press(12, 0)), Action::SelectTab(1)));
        assert!(matches!(r.route(press(21, 0)), Action::NewTab));
    }

    #[test]
    fn a_subagent_count_opens_its_list() {
        let mut r = R::new();
        r.hits.sidebar_spaces = vec![(Rect::new(1, 8, 28, 1), 0)];
        r.hits.sidebar_subagents = vec![(Rect::new(9, 8, 2, 1), 5)];
        assert!(matches!(r.route(press(9, 8)), Action::ToggleSubagents(5)), "the count wins over its row");
        assert!(matches!(r.route(press(3, 8)), Action::SelectSpace(0)), "the rest of the row selects the space");
        r.hits.sidebar_rows = vec![(Rect::new(1, 9, 28, 1), 5)];
        r.hits.sidebar_subagent_rows = vec![(Rect::new(1, 9, 28, 1), 5, "a1".into())];
        assert!(matches!(r.route(press(6, 9)), Action::OpenRun(5, ref id) if id == "a1"), "a subagent's row opens its run");
    }

    #[test]
    fn the_sidebar_toggle_is_a_button() {
        let mut r = R::new();
        r.hits.sidebar_toggle = Some(Rect::new(2, 20, 3, 1));
        assert!(matches!(r.route(press(3, 20)), Action::ToggleSidebar));
        assert!(!matches!(r.route(press(6, 20)), Action::ToggleSidebar), "only on the button");
    }

    #[test]
    fn a_space_dragged_over_others_takes_their_place() {
        let mut r = R::new();
        r.hits.sidebar_spaces = (0..3).map(|i| (Rect::new(0, 2 + 2 * i as u16, 20, 2), i)).collect();
        let drag = |y| ev(MouseEventKind::Drag(MouseButton::Left), 5, y);
        assert!(matches!(r.route(press(5, 2)), Action::SelectSpace(0)), "a press still selects");
        assert!(matches!(r.route(drag(3)), Action::None), "within its own row nothing moves");
        assert!(matches!(r.route(drag(6)), Action::MoveSpace { from: 0, to: 2 }));
        assert!(matches!(r.route(drag(4)), Action::MoveSpace { from: 2, to: 1 }), "it follows the pointer back");
        assert!(matches!(r.route(ev(MouseEventKind::Up(MouseButton::Left), 5, 4)), Action::None));
        assert!(matches!(r.route(drag(2)), Action::None), "released: nothing moves any more");
    }

    #[test]
    fn click_focuses_pane_that_did_not_ask_for_mouse() {
        let mut r = router(false, WheelRouting::Scrollback);
        match r.route(press(5, 5)) {
            Action::SelectStart { id, row, col } => {
                assert_eq!(id, 1);
                assert_eq!((row, col), (3, 4));
            }
            other => panic!("expected the start of a selection, got {other:?}"),
        }
        let mut r = router(false, WheelRouting::Scrollback);
        match r.route(press(0, 1)) {
            Action::FocusPane(id) => assert_eq!(id, 1),
            other => panic!("expected focus, got {other:?}"),
        }
    }

    #[test]
    fn drag_selects_and_release_finishes() {
        let mut r = router(false, WheelRouting::Scrollback);
        assert!(matches!(r.route(press(5, 5)), Action::SelectStart { .. }));

        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 100, 10)) {
            Action::SelectExtend { id, row, col } => {
                assert_eq!(id, 1);
                assert_eq!((row, col), (8, 37));
            }
            other => panic!("expected a selection drag, got {other:?}"),
        }
        assert_eq!(r.autoscroll, 0);

        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 5, 0)) {
            Action::SelectExtend { row, .. } => assert_eq!(row, 0),
            other => panic!("expected a selection drag, got {other:?}"),
        }
        assert_eq!(r.autoscroll, -1);

        r.route(ev(MouseEventKind::Drag(MouseButton::Left), 5, 100));
        assert_eq!(r.autoscroll, 1);

        assert!(matches!(
            r.route(ev(MouseEventKind::Up(MouseButton::Left), 5, 100)),
            Action::SelectEnd
        ));
        assert_eq!(r.autoscroll, 0);
    }

    #[test]
    fn second_click_in_a_row_selects_a_word() {
        let mut r = router(false, WheelRouting::Scrollback);
        assert!(matches!(r.route(press(5, 5)), Action::SelectStart { .. }));
        r.route(ev(MouseEventKind::Up(MouseButton::Left), 5, 5));
        match r.route(press(5, 5)) {
            Action::SelectWord { id, row, col } => {
                assert_eq!(id, 1);
                assert_eq!((row, col), (3, 4));
            }
            other => panic!("expected a word selection, got {other:?}"),
        }

        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 12, 5)) {
            Action::SelectExtendWord { row, col, .. } => assert_eq!((row, col), (3, 11)),
            other => panic!("expected a word-wise drag, got {other:?}"),
        }
        r.route(ev(MouseEventKind::Up(MouseButton::Left), 12, 5));

        match r.route(press(5, 5)) {
            Action::SelectLine { id, row } => {
                assert_eq!(id, 1);
                assert_eq!(row, 3);
            }
            other => panic!("expected a line selection, got {other:?}"),
        }
        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 12, 8)) {
            Action::SelectExtendLine { id, row } => {
                assert_eq!(id, 1);
                assert_eq!(row, 6);
            }
            other => panic!("expected a line-wise drag, got {other:?}"),
        }
        r.route(ev(MouseEventKind::Up(MouseButton::Left), 12, 8));

        assert!(matches!(r.route(press(5, 5)), Action::SelectStart { .. }));
        assert!(matches!(
            r.route(ev(MouseEventKind::Drag(MouseButton::Left), 12, 5)),
            Action::SelectExtend { .. }
        ));
    }

    #[test]
    fn pane_that_asked_for_mouse_is_not_selectable() {
        let mut r = router(true, WheelRouting::ToChild);
        assert!(matches!(r.route(press(5, 5)), Action::ToChild { .. }));
    }

    #[test]
    fn ctrl_click_opens_link_before_selection_or_child_forwarding() {
        let mut r = router(false, WheelRouting::Scrollback);
        match r.route(ctrl_press(5, 5)) {
            Action::OpenLink { id, row, col } => {
                assert_eq!(id, 1);
                assert_eq!((row, col), (3, 4));
            }
            other => panic!("expected a link to open, got {other:?}"),
        }

        let mut r = router(true, WheelRouting::ToChild);
        assert!(matches!(r.route(ctrl_press(5, 5)), Action::OpenLink { .. }));
    }

    #[test]
    fn click_outside_the_text_drops_the_selection() {
        let mut r = router(false, WheelRouting::Scrollback);
        r.route(press(5, 5));
        r.route(ev(MouseEventKind::Up(MouseButton::Left), 5, 5));

        // the first press already selects the tab, and drops the selection along the way
        assert!(matches!(r.route(press(12, 0)), Action::SelectTab(1)));
        assert!(r.dropped);

        r.route(press(5, 5));
        r.route(ev(MouseEventKind::Up(MouseButton::Left), 5, 5));
        match r.route(press(50, 5)) {
            Action::SelectStart { id, .. } => assert_eq!(id, 2),
            other => panic!("expected a new selection, got {other:?}"),
        }
    }

    #[test]
    fn same_click_goes_to_child_that_asked() {
        let mut r = router(true, WheelRouting::ToChild);
        match r.route(press(5, 5)) {
            Action::ToChild { id, bytes } => {
                assert_eq!(id, 1);
                assert!(!bytes.is_empty());
            }
            other => panic!("expected forwarding to the child, got {other:?}"),
        }
    }

    #[test]
    fn encoded_coordinates_are_pane_relative_and_one_based() {
        let inner = Rect::new(11, 6, 20, 10);
        let bytes = encode_for_child(press(13, 8), inner, crate::term::MouseProtocolEncoding::Sgr)
            .expect("click inside the content");
        assert_eq!(String::from_utf8_lossy(&bytes), "\x1b[<0;3;3M");

        assert!(encode_for_child(press(10, 6), inner, crate::term::MouseProtocolEncoding::Sgr).is_none());

        let bytes = encode_for_child(press(13, 8), inner, crate::term::MouseProtocolEncoding::Default)
            .expect("click inside the content");
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 32 + 3, 32 + 3]);
    }

    #[test]
    fn wheel_follows_three_way_rule() {
        let up = |x, y| ev(MouseEventKind::ScrollUp, x, y);

        let mut r = router(false, WheelRouting::Scrollback);
        match r.route(up(5, 5)) {
            Action::Scroll { id, lines } => {
                assert_eq!(id, 1);
                assert_eq!(lines, -(WHEEL_LINES as isize));
            }
            other => panic!("expected scrollback, got {other:?}"),
        }
        match r.route(ev(MouseEventKind::ScrollDown, 5, 5)) {
            Action::Scroll { lines, .. } => assert_eq!(lines, WHEEL_LINES as isize),
            other => panic!("expected scrollback, got {other:?}"),
        }

        let mut r = router(false, WheelRouting::Arrows { app_cursor: false });
        match r.route(up(5, 5)) {
            Action::ToChild { bytes, .. } => assert_eq!(bytes, b"\x1b[A\x1b[A\x1b[A"),
            other => panic!("expected arrow keys, got {other:?}"),
        }

        let mut r = router(false, WheelRouting::Arrows { app_cursor: true });
        match r.route(up(5, 5)) {
            Action::ToChild { bytes, .. } => assert_eq!(bytes, b"\x1bOA\x1bOA\x1bOA"),
            other => panic!("expected application arrow keys, got {other:?}"),
        }

        let mut r = router(true, WheelRouting::ToChild);
        match r.route(up(5, 5)) {
            Action::ToChild { bytes, .. } => {
                assert_eq!(String::from_utf8_lossy(&bytes), "\x1b[<64;5;4M")
            }
            other => panic!("expected forwarding to the child, got {other:?}"),
        }
    }

    #[test]
    fn drag_keeps_running_after_pointer_leaves_the_border() {
        let mut r = router(false, WheelRouting::Scrollback);
        assert!(matches!(r.route(press(40, 10)), Action::BeginDrag));

        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 20, 10)) {
            Action::Drag { path, ratio } => {
                assert!(path.is_empty());
                    assert!((ratio - 20.5 / 80.0).abs() < 1e-6, "{ratio}");
            }
            other => panic!("expected a drag, got {other:?}"),
        }

        assert!(matches!(
            r.route(ev(MouseEventKind::Up(MouseButton::Left), 20, 10)),
            Action::None
        ));
        assert!(matches!(r.route(press(5, 5)), Action::SelectStart { .. }));
    }

    #[test]
    fn ratio_is_clamped() {
        let mut r = router(false, WheelRouting::Scrollback);
        r.route(press(40, 10));
        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 0, 10)) {
            Action::Drag { ratio, .. } => assert_eq!(ratio, 0.05),
            other => panic!("expected a drag, got {other:?}"),
        }
        match r.route(ev(MouseEventKind::Drag(MouseButton::Left), 79, 10)) {
            Action::Drag { ratio, .. } => assert_eq!(ratio, 0.95),
            other => panic!("expected a drag, got {other:?}"),
        }
    }

    #[test]
    fn out_of_range_coordinates_are_quiet() {
        let mut r = router(true, WheelRouting::ToChild);
        assert!(matches!(r.route(press(u16::MAX, u16::MAX)), Action::None));

        let inner = Rect::new(0, 0, 300, 300);
        assert!(
            encode_for_child(press(250, 1), inner, crate::term::MouseProtocolEncoding::Default).is_none()
        );
        assert!(encode_for_child(press(250, 1), inner, crate::term::MouseProtocolEncoding::Utf8).is_some());
    }
}
