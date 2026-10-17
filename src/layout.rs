//! Pane layout, a binary space partition (BSP) tree, like in i3/tmux; rects are recomputed from area in `panes()`, never stored, so a resize needs no separate sync.

use ratatui::layout::Rect;

pub type PaneId = usize;

/// Horizontal = a vertical split, panes sit side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dir {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    pub id: PaneId,
    pub rect: Rect,
}

/// A border for mouse dragging; `path` points to the Split node whose ratio it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Border {
    pub dir: Dir,
    pub rect: Rect,
    pub path: Vec<bool>,
}

/// `path` addresses a node from the root without pointers or an arena: false is the first child, true the second.
enum Node {
    Pane(PaneId),
    Split {
        dir: Dir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

pub struct Layout {
    root: Node,
    focused: PaneId,
    /// The pane expanded to fill the whole screen, if any.
    zoom: Option<PaneId>,
}

fn collect_ids(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => out.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids(first, out);
            collect_ids(second, out);
        }
    }
}

/// Accumulates the path to the leaf with the given id; on a false return, the caller must roll back what it pushed.
fn find_path(node: &Node, id: PaneId, path: &mut Vec<bool>) -> bool {
    match node {
        Node::Pane(p) => *p == id,
        Node::Split { first, second, .. } => {
            path.push(false);
            if find_path(first, id, path) {
                return true;
            }
            path.pop();
            path.push(true);
            if find_path(second, id, path) {
                return true;
            }
            path.pop();
            false
        }
    }
}

/// Walks down `path` to the node in place; iterative to keep the borrow checker happy.
fn node_at_mut<'a>(mut node: &'a mut Node, path: &[bool]) -> &'a mut Node {
    for &second_branch in path {
        node = match node {
            Node::Split { first, second, .. } => {
                if second_branch {
                    second.as_mut()
                } else {
                    first.as_mut()
                }
            }
            Node::Pane(_) => node,
        };
    }
    node
}

/// Splits an area into two with a gap between them; a gap larger than the area doesn't panic, one half just ends up zero-sized.
fn split_rect(dir: Dir, area: Rect, ratio: f32, gap: u16) -> (Rect, Rect) {
    match dir {
        Dir::Horizontal => {
            let usable = area.width.saturating_sub(gap);
            let first_w = ((usable as f32) * ratio).round() as u16;
            let first_w = first_w.min(usable);
            let second_w = usable - first_w;
            (
                Rect { x: area.x, y: area.y, width: first_w, height: area.height },
                Rect { x: area.x + first_w + gap, y: area.y, width: second_w, height: area.height },
            )
        }
        Dir::Vertical => {
            let usable = area.height.saturating_sub(gap);
            let first_h = ((usable as f32) * ratio).round() as u16;
            let first_h = first_h.min(usable);
            let second_h = usable - first_h;
            (
                Rect { x: area.x, y: area.y, width: area.width, height: first_h },
                Rect { x: area.x, y: area.y + first_h + gap, width: area.width, height: second_h },
            )
        }
    }
}

fn layout_rect(node: &Node, area: Rect, gap: u16, out: &mut Vec<PaneRect>) {
    match node {
        Node::Pane(id) => out.push(PaneRect { id: *id, rect: area }),
        Node::Split { dir, ratio, first, second } => {
            let (a, b) = split_rect(*dir, area, *ratio, gap);
            layout_rect(first, a, gap, out);
            layout_rect(second, b, gap, out);
        }
    }
}

fn collect_borders(node: &Node, area: Rect, gap: u16, path: &mut Vec<bool>, out: &mut Vec<Border>) {
    if let Node::Split { dir, ratio, first, second } = node {
        let (a, b) = split_rect(*dir, area, *ratio, gap);
        let rect = match dir {
            Dir::Horizontal => Rect { x: a.x + a.width, y: area.y, width: gap, height: area.height },
            Dir::Vertical => Rect { x: area.x, y: a.y + a.height, width: area.width, height: gap },
        };
        out.push(Border { dir: *dir, rect, path: path.clone() });
        path.push(false);
        collect_borders(first, a, gap, path, out);
        path.pop();
        path.push(true);
        collect_borders(second, b, gap, path, out);
        path.pop();
    }
}

fn first_pane_id(node: &Node) -> Option<PaneId> {
    match node {
        Node::Pane(id) => Some(*id),
        Node::Split { first, .. } => first_pane_id(first),
    }
}

/// The tree's shape for saving in a session; focus and zoom are transient UI state, not saved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Snapshot {
    Split { dir: Dir, ratio: f32, first: Box<Snapshot>, second: Box<Snapshot> },
    Pane(PaneId),
}

fn node_to_snapshot(node: &Node) -> Snapshot {
    match node {
        Node::Pane(id) => Snapshot::Pane(*id),
        Node::Split { dir, ratio, first, second } => Snapshot::Split {
            dir: *dir,
            ratio: *ratio,
            first: Box::new(node_to_snapshot(first)),
            second: Box::new(node_to_snapshot(second)),
        },
    }
}

fn snapshot_to_node(s: &Snapshot) -> Node {
    match s {
        Snapshot::Pane(id) => Node::Pane(*id),
        Snapshot::Split { dir, ratio, first, second } => Node::Split {
            dir: *dir,
            ratio: *ratio,
            first: Box::new(snapshot_to_node(first)),
            second: Box::new(snapshot_to_node(second)),
        },
    }
}

impl Layout {
    pub fn new(first: PaneId) -> Self {
        Self { root: Node::Pane(first), focused: first, zoom: None }
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    pub fn focus(&mut self, id: PaneId) -> bool {
        if find_path(&self.root, id, &mut Vec::new()) {
            self.focused = id;
            true
        } else {
            false
        }
    }

    pub fn pane_count(&self) -> usize {
        self.pane_ids().len()
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        collect_ids(&self.root, &mut out);
        out
    }

    /// Splits the focused pane; the new pane gets its id and focus.
    pub fn split(&mut self, dir: Dir, new_id: PaneId) {
        let mut path = Vec::new();
        if !find_path(&self.root, self.focused, &mut path) {
            return;
        }
        let node = node_at_mut(&mut self.root, &path);
        let old = std::mem::replace(node, Node::Pane(0));
        *node = Node::Split { dir, ratio: 0.5, first: Box::new(old), second: Box::new(Node::Pane(new_id)) };
        self.focused = new_id;
        // Otherwise the newly focused pane could stay behind a zoomed one, invisible.
        self.zoom = None;
    }

    /// Closes a pane; if one remains, it stays. Returns false if id isn't found.
    pub fn close(&mut self, id: PaneId) -> bool {
        let mut path = Vec::new();
        if !find_path(&self.root, id, &mut path) {
            return false;
        }
        if let Some(last) = path.pop() {
            // Collapses the parent into the remaining child: a Split never has a single child.
            let parent = node_at_mut(&mut self.root, &path);
            let old = std::mem::replace(parent, Node::Pane(0));
            if let Node::Split { first, second, .. } = old {
                *parent = if last { *first } else { *second };
            }
        }
        if self.zoom == Some(id) {
            self.zoom = None;
        }
        if self.focused == id {
            self.focused = self.pane_ids().first().copied().unwrap_or(id);
        }
        true
    }

    /// The leaf `old` becomes `new`; focus and zoom follow it. False if `old` isn't found.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        let mut path = Vec::new();
        if !find_path(&self.root, old, &mut path) {
            return false;
        }
        *node_at_mut(&mut self.root, &path) = Node::Pane(new);
        if self.focused == old {
            self.focused = new;
        }
        if self.zoom == Some(old) {
            self.zoom = Some(new);
        }
        true
    }

    /// Layout over an area, accounting for the gap between panes (in cells).
    pub fn panes(&self, area: Rect, gap: u16) -> Vec<PaneRect> {
        if area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        if let Some(id) = self.zoom {
            return vec![PaneRect { id, rect: area }];
        }
        let mut out = Vec::new();
        layout_rect(&self.root, area, gap, &mut out);
        out
    }

    /// All divider borders -- for the drag hit test.
    pub fn borders(&self, area: Rect, gap: u16) -> Vec<Border> {
        if area.width == 0 || area.height == 0 || self.zoom.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        collect_borders(&self.root, area, gap, &mut Vec::new(), &mut out);
        out
    }

    /// Changes a node's ratio along the path to a new value (0.05..0.95).
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        if let Node::Split { ratio: r, .. } = node_at_mut(&mut self.root, path) {
            *r = ratio.clamp(0.05, 0.95);
        }
    }

    /// Moves focus to the nearest pane by straight-line distance between centers; false if none that way.
    pub fn navigate(&mut self, area: Rect, dir: Nav) -> bool {
        let panes = self.panes(area, 0);
        let Some(current) = panes.iter().find(|p| p.id == self.focused) else {
            return false;
        };
        let cx = current.rect.x as f32 + current.rect.width as f32 / 2.0;
        let cy = current.rect.y as f32 + current.rect.height as f32 / 2.0;

        let mut best: Option<(f32, PaneId)> = None;
        for p in &panes {
            if p.id == self.focused {
                continue;
            }
            let px = p.rect.x as f32 + p.rect.width as f32 / 2.0;
            let py = p.rect.y as f32 + p.rect.height as f32 / 2.0;
            let (dx, dy) = (px - cx, py - cy);
            let in_dir = match dir {
                Nav::Left => dx < 0.0,
                Nav::Right => dx > 0.0,
                Nav::Up => dy < 0.0,
                Nav::Down => dy > 0.0,
            };
            if !in_dir {
                continue;
            }
            let dist = dx * dx + dy * dy;
            if best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, p.id));
            }
        }

        match best {
            Some((_, id)) => {
                self.focused = id;
                true
            }
            None => false,
        }
    }

    /// Zoom: one pane fills the screen, calling again undoes it.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoom = if self.zoom.is_some() { None } else { Some(self.focused) };
        self.zoom.is_some()
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some()
    }

    pub fn snapshot(&self) -> Snapshot {
        node_to_snapshot(&self.root)
    }

    pub fn from_snapshot(s: &Snapshot) -> Option<Self> {
        let root = snapshot_to_node(s);
        let focused = first_pane_id(&root)?;
        Some(Self { root, focused, zoom: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 0, y: 0, width: 100, height: 40 }
    }

    #[test]
    fn split_side_by_side() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        let panes = l.panes(area(), 0);
        assert_eq!(panes.len(), 2);
        let p1 = panes.iter().find(|p| p.id == 1).unwrap();
        let p2 = panes.iter().find(|p| p.id == 2).unwrap();
        assert_eq!(p1.rect.y, 0);
        assert_eq!(p2.rect.y, 0);
        assert_eq!(p1.rect.width + p2.rect.width, 100);
        assert!(p2.rect.x >= p1.rect.x + p1.rect.width);
    }

    #[test]
    fn gap_shrinks_rects() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        let no_gap: u16 = l.panes(area(), 0).iter().map(|p| p.rect.width).sum();
        let with_gap: u16 = l.panes(area(), 2).iter().map(|p| p.rect.width).sum();
        assert!(with_gap < no_gap);
    }

    #[test]
    fn close_collapses_parent() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        assert_eq!(l.pane_count(), 2);
        assert!(l.close(2));
        assert_eq!(l.pane_ids(), vec![1]);
        let panes = l.panes(area(), 0);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].rect, area());
    }

    #[test]
    fn close_missing_returns_false() {
        let mut l = Layout::new(1);
        assert!(!l.close(99));
    }

    #[test]
    fn set_ratio_moves_border() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        let borders = l.borders(area(), 1);
        assert_eq!(borders.len(), 1);

        l.set_ratio(&borders[0].path, 0.2);
        let panes = l.panes(area(), 1);
        let p1 = panes.iter().find(|p| p.id == 1).unwrap();
        assert!((p1.rect.width as f32) < 30.0);
    }

    #[test]
    fn navigate_focus() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        assert!(l.navigate(area(), Nav::Left));
        assert_eq!(l.focused(), 1);
        assert!(l.navigate(area(), Nav::Right));
        assert_eq!(l.focused(), 2);

        l.split(Dir::Vertical, 3);
        assert!(l.navigate(area(), Nav::Up));
        assert_eq!(l.focused(), 2);
        assert!(l.navigate(area(), Nav::Down));
        assert_eq!(l.focused(), 3);
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        l.split(Dir::Vertical, 3);
        l.set_ratio(&[], 0.3);

        let restored = Layout::from_snapshot(&l.snapshot()).unwrap();
        let mut before = l.pane_ids();
        let mut after = restored.pane_ids();
        before.sort();
        after.sort();
        assert_eq!(before, after);
        assert_eq!(l.panes(area(), 1).len(), restored.panes(area(), 1).len());
    }

    #[test]
    fn replace_keeps_the_place_and_the_focus() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        l.split(Dir::Vertical, 3);
        l.focus(2);
        l.toggle_zoom();
        let before = l.panes(area(), 1).iter().find(|p| p.id == 2).unwrap().rect;
        assert!(l.replace(2, 9));
        assert_eq!(l.focused(), 9);
        assert_eq!(l.panes(area(), 1), vec![PaneRect { id: 9, rect: before }], "zoom follows");
        l.toggle_zoom();
        assert_eq!(l.panes(area(), 1).iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 9, 3]);
        assert!(!l.replace(2, 10), "the old id is gone");
    }

    #[test]
    fn zoom_fills_area() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);

        assert!(l.toggle_zoom());
        assert!(l.is_zoomed());
        let panes = l.panes(area(), 1);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].rect, area());

        assert!(!l.toggle_zoom());
        assert!(!l.is_zoomed());
        assert_eq!(l.panes(area(), 1).len(), 2);
    }
}
