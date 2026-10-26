//! The sidebar: what needs you now, and a tree of spaces with what runs in them.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};

use crate::layout::PaneId;
use crate::render::Theme;
use crate::term::AgentState;

/// Worktrees of one repository share this key: the main checkout's `.git`.
pub type GroupKey = PathBuf;

/// A space and what runs in it.
pub struct SpaceNode {
    pub index: usize,
    pub name: String,
    pub branch: Option<String>,
    /// The loudest state of the space's agents.
    pub state: AgentState,
    pub active: bool,
    /// The keys are on this row.
    pub selected: bool,
    /// None for a folder outside a repository, which always stands alone.
    pub group: Option<GroupKey>,
    pub is_group_parent: bool,
    /// Meaningful only on the parent.
    pub collapsed: bool,
    /// Shown under the space only when there is more than one.
    pub panes: Vec<PaneNode>,
}

pub struct PaneNode {
    pub pane: PaneId,
    pub agent: bool,
    /// The label, else the agent, else the command.
    pub name: String,
    /// The tab's name when the space has more than one tab.
    pub tab: Option<String>,
    pub state: AgentState,
    pub focused: bool,
    /// The keys are on this row: the sidebar has them, and this is where they are.
    pub selected: bool,
    /// Subagents running or just finished.
    pub subagents: Vec<SubRow>,
    /// Their list is open: the human asked for it, and it stays until they close it.
    pub open: bool,
}

pub struct SubRow {
    pub id: String,
    pub kind: String,
    /// Its current tool, else its task.
    pub doing: String,
    pub state: crate::hook::SubState,
    /// Since it started.
    pub age: std::time::Duration,
    /// Nothing heard from it for a while.
    pub stale: bool,
    /// Its run is the section open over the pane.
    pub open: bool,
}

impl SubRow {
    /// Its last tool whether it runs, finished or failed; its task only while it has called none.
    /// A call with nothing to act on (the report a subagent hands back as its last step) says
    /// nothing about the work, so the last call that had a target speaks for it.
    pub fn of(s: &crate::hook::Sub, now: u64) -> Self {
        let last = s.calls.iter().rev().find(|c| !c.target.is_empty()).or(s.calls.last());
        let state = s.state(now);
        let unknown = state == crate::hook::SubState::Unknown;
        Self {
            id: s.id.clone(),
            kind: s.kind.clone(),
            doing: if unknown { "no report".into() } else { last.map_or_else(|| s.task.clone(), |c| c.tool.clone()) },
            state,
            // an unknown one's clock stops at its last word
            age: std::time::Duration::from_secs(if unknown { s.last } else { now }.saturating_sub(s.started)),
            stale: s.stale(now) && !unknown,
            open: false,
        }
    }
}

/// An agent that finished or asks for an answer.
pub struct Waiting {
    pub pane: PaneId,
    /// The pane's label, as the tree shows it; a number follows when the space has several alike.
    pub name: String,
    pub space: String,
    pub tab: Option<String>,
    pub state: AgentState,
    /// How long it has waited; None when nerd did not see it change (it waited since before a restart).
    pub waited: Option<std::time::Duration>,
    /// The tool a blocked agent asks about, when known.
    pub asks: Option<String>,
}

/// A message held for the owner, a route request, or a message to the owner.
pub struct MailRow {
    pub id: String,
    pub text: String,
    pub waited: std::time::Duration,
}

/// Agents that ask for an answer first, since they cannot go on without you; then finished ones.
/// Newest first within each; the pane breaks a tie, so equal waits keep one order.
pub fn sort_waiting(list: &mut [Waiting]) {
    list.sort_by_key(|w| (!matches!(w.state, AgentState::Blocked | AgentState::Failed), w.waited.unwrap_or(std::time::Duration::MAX), w.pane));
}

/// `12s`, `3m`, `1h`, `2d`: whole units, the largest that fits.
pub fn ago(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", (s / 86400).min(99)),
    }
}

#[derive(Default)]
pub struct Hits {
    pub spaces: Vec<(Rect, usize)>,
    /// The ▸/▾ cell of a group's main checkout: (rect, its space index).
    pub group_toggles: Vec<(Rect, usize)>,
    /// Waiting rows and pane rows alike: a click opens the pane.
    pub panes: Vec<(Rect, PaneId)>,
    pub new_space: Option<Rect>,
    /// « in the bottom line, » in the rail's: collapse and expand.
    pub toggle: Option<Rect>,
    /// An agent's `◦N`: a click opens or closes its subagents.
    pub subagent_toggles: Vec<(Rect, PaneId)>,
    /// A subagent's row: a click opens its run.
    pub subagent_rows: Vec<(Rect, PaneId, String)>,
    /// A mail row, by message id: a click opens the Messages window on it.
    pub mail: Vec<(Rect, String)>,
}

/// The collapsed sidebar's width: a border, a mark between two spaces, a border.
pub const RAIL_W: u16 = 5;

/// needs you's fixed height: what waits never pushes the tree down.
pub const NEEDS_ROWS: u16 = 3;

const COLLAPSED: &str = "\u{25b8}";
const EXPANDED: &str = "\u{25be}";
const TERMINAL: &str = "\u{203a}";

/// Two blocks: `needs you`, always the same height, and `spaces`. Rows move only when the human acts.
/// `keys`: the sidebar has the keyboard, so its frame and titles are lit.
pub fn draw(buf: &mut Buffer, area: Rect, waiting: &[Waiting], mail: &[MailRow], spaces: &[SpaceNode], keys: bool, theme: &Theme) -> Hits {
    let mut hits = Hits::default();
    if area.width < 12 || area.height < 4 {
        return hits;
    }
    let frame = if keys { theme.border_focus } else { theme.border };
    Block::bordered().border_style(Style::new().fg(frame)).render(area, buf);
    let inner = area.inner(Margin::new(1, 1));
    let rows = Rows { x: inner.x + 1, right: inner.right() - 1, bottom: inner.bottom(), full: inner };
    // air: a blank row under each block title and above the divider; tighter inside a group than between
    let mut y = inner.y;

    // needs you keeps its height whatever waits, so the tree below never moves because of it
    title(buf, area, area.y, "needs you", keys, theme);
    y += 1;
    let total = waiting.len() + mail.len();
    let shown = if total > NEEDS_ROWS as usize { NEEDS_ROWS as usize - 1 } else { total };
    for (n, m) in mail.iter().enumerate().take(shown.saturating_sub(waiting.len())) {
        let y = y + (waiting.len() + n) as u16;
        if y >= rows.bottom {
            break;
        }
        let age = ago(m.waited);
        // it waits on the owner's answer, as a blocked agent does
        rows.mark(buf, rows.x, y, AgentState::Blocked, theme);
        rows.text(buf, rows.x + 2, y, &m.text, age.len() as u16 + 1, Style::new().fg(theme.text));
        buf.set_string(rows.right - age.len() as u16, y, &age, Style::new().fg(theme.faint));
        hits.mail.push((rows.line(y), m.id.clone()));
    }
    for (n, w) in waiting.iter().take(shown).enumerate() {
        let y = y + n as u16;
        if y >= rows.bottom {
            break;
        }
        // the mark says what happened; the right side says how long it has waited
        let age = w.waited.map(ago).unwrap_or_default();
        // the question comes before the space and tab, which are what a narrow row cuts
        let mut label = w.name.clone();
        if let (AgentState::Blocked, Some(tool)) = (w.state, &w.asks) {
            label.push_str(&format!(" \u{b7} Allow {tool}?"));
        }
        label.push_str(&format!(" \u{b7} {}", w.space));
        if let Some(t) = &w.tab {
            label.push_str(&format!(" \u{b7} {t}"));
        }
        rows.mark(buf, rows.x, y, w.state, theme);
        rows.text(buf, rows.x + 2, y, &label, age.len() as u16 + 1, Style::new().fg(theme.text));
        buf.set_string(rows.right - age.len() as u16, y, &age, Style::new().fg(theme.faint));
        hits.panes.push((rows.line(y), w.pane));
    }
    let note = match total {
        0 => Some("all quiet".to_string()),
        n if n > shown => Some(format!("+{} more", n - shown)),
        _ => None,
    };
    if let Some(note) = note {
        let at = y + if total == 0 { 0 } else { shown as u16 };
        if at < rows.bottom {
            rows.text(buf, rows.x, at, &note, 0, Style::new().fg(theme.faint));
        }
    }
    let spaces_title = (y + NEEDS_ROWS + 1).min(rows.bottom);
    rule(buf, area, spaces_title, frame);
    y = spaces_title + 1;
    title(buf, area, spaces_title, "spaces", keys, theme);
    let plus = Rect::new(area.right() - 5, spaces_title, 3, 1);
    buf.set_string(plus.x, plus.y, " + ", Style::new().fg(theme.muted));
    hits.new_space = Some(plus);
    let toggle = Rect::new(area.x + 2, area.bottom() - 1, 3, 1);
    buf.set_string(toggle.x, toggle.y, " \u{ab} ", Style::new().fg(theme.muted));
    hits.toggle = Some(toggle);
    y += 1;

    for (i, kind) in group_order(spaces) {
        if y >= rows.bottom {
            break;
        }
        let sp = &spaces[i];
        let state = match (kind, &sp.group) {
            // a collapsed group speaks for all its members
            (Kind::Parent { collapsed: true }, Some(key)) => loudest(spaces, key),
            _ => sp.state,
        };
        let line = rows.line(y);
        let x = match kind {
            Kind::Plain => rows.x,
            Kind::Parent { collapsed } => {
                buf.set_string(rows.x, y, if collapsed { COLLAPSED } else { EXPANDED }, Style::new().fg(theme.faint));
                hits.group_toggles.push((Rect::new(rows.x, y, 1, 1), sp.index));
                rows.x + 2
            }
            Kind::Child { last } => {
                buf.set_string(rows.x + 2, y, if last { "\u{2514}\u{2500}" } else { "\u{251c}\u{2500}" }, Style::new().fg(theme.faint));
                rows.x + 5
            }
        };
        rows.mark(buf, x, y, state, theme);
        let branch = sp.branch.as_deref().unwrap_or("");
        let mut name_end = rows.text(buf, x + 2, y, &sp.name, 0, name_style(sp.active, theme));
        // a single pane folds into its space: its subagents' count sits on the space's line
        let solo = match sp.panes.as_slice() {
            [p] => Some(p),
            _ => None,
        };
        if let Some(p) = solo {
            name_end = rows.count(buf, name_end + 1, y, p, &mut hits, theme);
        }
        rows.right_label(buf, y, name_end + 2, branch, theme);
        if sp.selected {
            buf.set_style(line, theme.cursor());
        }
        hits.spaces.push((line, sp.index));
        y += 1;

        if let Some(p) = solo {
            if p.open && !p.subagents.is_empty() {
                y = rows.subagents(buf, x, y, p, None, &mut hits, theme);
                y += 1;
            }
            continue;
        }
        if sp.panes.is_empty() {
            continue;
        }
        // the space's panes hang from its mark: the branch glyph under the mark's centre
        for (n, p) in sp.panes.iter().enumerate() {
            if y >= rows.bottom {
                break;
            }
            let line = rows.line(y);
            let last = n + 1 == sp.panes.len();
            buf.set_string(x, y, if last { "\u{2514}" } else { "\u{251c}" }, Style::new().fg(theme.faint));
            if p.agent {
                rows.mark(buf, x + 2, y, p.state, theme);
            } else {
                buf.set_string(x + 2, y, TERMINAL, Style::new().fg(theme.faint));
            }
            let end = rows.text(buf, x + 4, y, &p.name, 0, name_style(p.focused, theme));
            let end = rows.count(buf, end + 1, y, p, &mut hits, theme);
            rows.right_label(buf, y, end + 2, p.tab.as_deref().unwrap_or(""), theme);
            if p.selected {
                buf.set_style(line, theme.cursor());
            }
            hits.panes.push((line, p.pane));
            y += 1;
            if p.open && !p.subagents.is_empty() {
                y = rows.subagents(buf, x + 2, y, p, (!last).then_some(x), &mut hits, theme);
            }
        }
        // an opened space is a group of its own: a blank row keeps it apart from the next
        y += 1;
    }
    hits
}

/// The collapsed sidebar: one mark per waiting agent and per space, each still a click away; » expands it.
pub fn draw_rail(buf: &mut Buffer, area: Rect, waiting: &[Waiting], mail: &[MailRow], spaces: &[SpaceNode], theme: &Theme) -> Hits {
    let mut hits = Hits::default();
    if area.width < RAIL_W || area.height < 4 {
        return hits;
    }
    Block::bordered().border_style(Style::new().fg(theme.border)).render(area, buf);
    let (x, bottom) = (area.x + 2, area.bottom() - 1);
    let line = |y| Rect::new(area.x + 1, y, area.width - 2, 1);
    let mut y = area.y + 1;
    for (n, w) in waiting.iter().take(NEEDS_ROWS as usize).enumerate() {
        let y = y + n as u16;
        if y >= bottom {
            break;
        }
        buf.set_string(x, y, w.state.mark().to_string(), Style::new().fg(state_color(w.state, theme)));
        hits.panes.push((line(y), w.pane));
    }
    for (n, m) in mail.iter().enumerate().take((NEEDS_ROWS as usize).saturating_sub(waiting.len())) {
        let y = y + (waiting.len() + n) as u16;
        if y >= bottom {
            break;
        }
        buf.set_string(x, y, AgentState::Blocked.mark().to_string(), Style::new().fg(state_color(AgentState::Blocked, theme)));
        hits.mail.push((line(y), m.id.clone()));
    }
    y = (y + NEEDS_ROWS).min(bottom);
    if y < bottom {
        rule(buf, area, y, theme.border);
        y += 1;
    }
    for sp in spaces {
        if y >= bottom {
            break;
        }
        // a mark cannot take the chosen weight, so the rail marks its space with the away slab
        if sp.active {
            buf.set_style(line(y), Style::new().bg(theme.slab));
        }
        buf.set_string(x, y, sp.state.mark().to_string(), Style::new().fg(state_color(sp.state, theme)));
        hits.spaces.push((line(y), sp.index));
        y += 1;
    }
    let toggle = Rect::new(area.x + 1, bottom, 3, 1);
    buf.set_string(toggle.x, toggle.y, " \u{bb} ", Style::new().fg(theme.muted));
    hits.toggle = Some(toggle);
    hits
}

/// The text area of the block and how to put things on its lines.
struct Rows {
    x: u16,
    right: u16,
    bottom: u16,
    full: Rect,
}

impl Rows {
    /// A full-width line, so a click left of the text still hits the entry.
    fn line(&self, y: u16) -> Rect {
        Rect::new(self.full.x, y, self.full.width, 1)
    }

    fn mark(&self, buf: &mut Buffer, x: u16, y: u16, state: AgentState, theme: &Theme) {
        if x < self.right {
            buf.set_string(x, y, state.mark().to_string(), Style::new().fg(state_color(state, theme)));
        }
    }

    /// Draws text up to `reserve` cells before the right edge; returns where it ended.
    fn text(&self, buf: &mut Buffer, x: u16, y: u16, text: &str, reserve: u16, style: Style) -> u16 {
        let room = self.right.saturating_sub(x + reserve);
        if room == 0 {
            return x;
        }
        buf.set_stringn(x, y, text, room as usize, style).0
    }

    /// `◦N` after a name when the pane runs subagents; returns where it ended.
    fn count(&self, buf: &mut Buffer, x: u16, y: u16, p: &PaneNode, hits: &mut Hits, theme: &Theme) -> u16 {
        if p.subagents.is_empty() || x >= self.right {
            return x.saturating_sub(1);
        }
        // Counts the running ones; with none running the rows still open from here, in faint.
        let running = p.subagents.iter().filter(|s| s.state == crate::hook::SubState::Running).count();
        let (n, color) = if running > 0 { (running, theme.warn) } else { (p.subagents.len(), theme.faint) };
        let label = format!("\u{25e6}{n}");
        let end = buf.set_stringn(x, y, &label, (self.right - x) as usize, Style::new().fg(color)).0;
        hits.subagent_toggles.push((Rect::new(x, y, end - x, 1), p.pane));
        end
    }

    /// An open subagent list under its agent: branch glyphs at `x`, a bar at `bar` while more panes follow.
    #[allow(clippy::too_many_arguments)]
    fn subagents(&self, buf: &mut Buffer, x: u16, mut y: u16, p: &PaneNode, bar: Option<u16>, hits: &mut Hits, theme: &Theme) -> u16 {
        use crate::hook::SubState;
        let faint = Style::new().fg(theme.faint);
        for (k, s) in p.subagents.iter().enumerate() {
            if y >= self.bottom {
                break;
            }
            if let Some(bx) = bar {
                buf.set_string(bx, y, "\u{2502}", faint);
            }
            buf.set_string(x, y, if k + 1 == p.subagents.len() { "\u{2514}" } else { "\u{251c}" }, faint);
            let (mark, tone) = match s.state {
                SubState::Running => ("\u{25cf}", theme.teal),
                SubState::Done => ("\u{2713}", theme.ok),
                SubState::Failed => ("\u{2715}", theme.bad),
                SubState::Unknown => ("\u{b7}", theme.faint),
            };
            buf.set_string(x + 2, y, mark, Style::new().fg(tone));
            let age = ago(s.age);
            let reserve = age.len() as u16 + 1;
            let kind = if s.open { theme.chosen() } else { Style::new().fg(theme.muted) };
            let end = self.text(buf, x + 4, y, &s.kind, reserve, kind);
            if !s.doing.is_empty() {
                self.text(buf, end, y, &format!(" \u{b7} {}", s.doing), reserve, faint);
            }
            buf.set_string(self.right - age.len() as u16, y, &age, Style::new().fg(if s.stale { theme.warn } else { theme.faint }));
            hits.subagent_rows.push((self.line(y), p.pane, s.id.clone()));
            y += 1;
        }
        y
    }

    /// A faint label flush right, cut with an ellipsis to what is left after `from`; nothing if no room.
    fn right_label(&self, buf: &mut Buffer, y: u16, from: u16, label: &str, theme: &Theme) {
        let room = self.right.saturating_sub(from) as usize;
        if label.is_empty() || room < 2 {
            return;
        }
        let shown: String = if label.chars().count() > room {
            label.chars().take(room - 1).chain(std::iter::once('\u{2026}')).collect()
        } else {
            label.to_string()
        };
        buf.set_string(self.right - shown.chars().count() as u16, y, &shown, Style::new().fg(theme.faint));
    }
}

fn name_style(chosen: bool, theme: &Theme) -> Style {
    if chosen { theme.chosen() } else { Style::new().fg(theme.text) }
}

pub fn state_color(state: AgentState, theme: &Theme) -> Color {
    match state {
        AgentState::Working => theme.warn,
        AgentState::Blocked => theme.wait,
        AgentState::Failed => theme.bad,
        AgentState::Done => theme.teal,
        AgentState::Idle => theme.muted,
        AgentState::Unknown => theme.faint,
    }
}

/// A block's title in the frame line at `y`, as every panel carries it.
fn title(buf: &mut Buffer, area: Rect, y: u16, text: &str, lit: bool, theme: &Theme) {
    buf.set_stringn(area.x + 2, y, format!(" {text} "), area.width.saturating_sub(8) as usize, theme.title(lit));
}

/// Block divider: a full-width line that meets the border's sides.
/// Block divider in the frame's own colour: it is part of the frame, and lights with it.
fn rule(buf: &mut Buffer, area: Rect, y: u16, color: Color) {
    let line = format!("\u{251c}{}\u{2524}", "\u{2500}".repeat(area.width as usize - 2));
    buf.set_stringn(area.x, y, line, area.width as usize, Style::new().fg(color));
}

/// What a space row is: standalone, a group's main checkout, or a worktree under it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Plain,
    Parent { collapsed: bool },
    /// `last`: the last worktree in the group gets └─ instead of ├─.
    Child { last: bool },
}

/// Row order: a group's main checkout then its worktrees, then the next group or standalone space;
/// a collapsed group yields only its main checkout. A worktree whose main checkout is not in the
/// list stands alone, so it doesn't disappear from the sidebar.
fn group_order(spaces: &[SpaceNode]) -> Vec<(usize, Kind)> {
    let mut out = Vec::with_capacity(spaces.len());
    for (i, space) in spaces.iter().enumerate() {
        let Some(key) = space.group.as_ref() else {
            out.push((i, Kind::Plain));
            continue;
        };
        if space.is_group_parent {
            out.push((i, Kind::Parent { collapsed: space.collapsed }));
            if space.collapsed {
                continue;
            }
            let kids: Vec<usize> = spaces
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.is_group_parent && s.group.as_ref() == Some(key))
                .map(|(k, _)| k)
                .collect();
            let n = kids.len();
            out.extend(kids.into_iter().enumerate().map(|(j, k)| (k, Kind::Child { last: j + 1 == n })));
        } else if !spaces.iter().any(|s| s.is_group_parent && s.group.as_ref() == Some(key)) {
            out.push((i, Kind::Plain));
        }
    }
    out
}

/// The loudest state in a group, so a waiting agent in a hidden worktree isn't missed.
fn loudest(spaces: &[SpaceNode], key: &GroupKey) -> AgentState {
    spaces
        .iter()
        .filter(|s| s.group.as_ref() == Some(key))
        .map(|s| s.state)
        .max_by_key(|s| s.priority())
        .unwrap_or(AgentState::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    const W: u16 = 30;
    const H: u16 = 24;
    /// needs you's title, a blank row, its three rows and the divider come first: the tree starts here.
    const FIRST: u16 = 8;

    fn space(index: usize, name: &str, branch: &str, state: AgentState) -> SpaceNode {
        SpaceNode {
            index,
            name: name.into(),
            branch: Some(branch.into()),
            state,
            active: false,
            selected: false,
            group: None,
            is_group_parent: false,
            collapsed: false,
            panes: Vec::new(),
        }
    }

    fn pane(pane: PaneId, agent: bool, name: &str, tab: Option<&str>, state: AgentState) -> PaneNode {
        PaneNode { pane, agent, name: name.into(), tab: tab.map(str::to_string), state, focused: false, selected: false, subagents: Vec::new(), open: false }
    }

    fn waiting(pane: PaneId, space: &str, state: AgentState) -> Waiting {
        Waiting { pane, name: "claude".into(), space: space.into(), tab: None, state, waited: Some(std::time::Duration::from_secs(12)), asks: None }
    }

    fn render(waits: &[Waiting], spaces: &[SpaceNode]) -> (Buffer, Hits) {
        let area = Rect::new(0, 0, W, H);
        let mut buf = Buffer::empty(area);
        let hits = draw(&mut buf, area, waits, &[], spaces, true, &Theme::dark());
        (buf, hits)
    }

    fn line(buf: &Buffer, y: u16) -> String {
        (0..W).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn needs_you_keeps_its_height_whatever_waits() {
        let spaces = [space(0, "nerd", "master", AgentState::Idle)];
        let (buf, hits) = render(&[waiting(7, "shop", AgentState::Blocked), waiting(8, "web", AgentState::Done)], &spaces);
        assert!(line(&buf, 0).contains(" needs you "));
        assert_eq!(line(&buf, 1).trim_matches(|c| c == '│' || c == ' '), "", "air under the title");
        let asks = line(&buf, 2);
        assert!(asks.contains("● claude · shop") && asks.trim_end_matches(['│', ' ']).ends_with("12s"), "no word, the wait: {asks:?}");
        assert!(!asks.contains("asks") && line(&buf, 3).contains("✓ claude · web") && !line(&buf, 3).contains("done"));
        assert_eq!(buf[(W - 4, 2)].fg, Theme::dark().faint);
        assert!(line(&buf, 6).starts_with("├─ spaces"), "{:?}", line(&buf, 6));
        assert!(line(&buf, FIRST).starts_with("│ ○ nerd"));
        assert_eq!(hits.panes.iter().map(|(_, p)| *p).collect::<Vec<_>>(), vec![7, 8]);

        // nothing waits: the block says so, and the tree stays where it was
        let (buf, _) = render(&[], &spaces);
        assert!(line(&buf, 2).starts_with("│ all quiet"), "{:?}", line(&buf, 2));
        assert!(line(&buf, 6).starts_with("├─ spaces"));
        assert!(line(&buf, FIRST).starts_with("│ ○ nerd"));

        // more than three: two, then how many more; still three rows
        let many: Vec<Waiting> = (0..5).map(|i| waiting(i, "s", AgentState::Done)).collect();
        let (buf, hits) = render(&many, &spaces);
        assert!(line(&buf, 4).starts_with("│ +3 more"), "{:?}", line(&buf, 4));
        assert_eq!(hits.panes.len(), 2);
        assert!(line(&buf, FIRST).starts_with("│ ○ nerd"));
    }

    #[test]
    fn answers_come_before_results_and_the_newest_first() {
        let w = |pane, state, secs| Waiting { waited: Some(std::time::Duration::from_secs(secs)), ..waiting(pane, "s", state) };
        let mut list = vec![
            w(1, AgentState::Done, 5),
            w(2, AgentState::Blocked, 300),
            w(3, AgentState::Done, 1),
            w(4, AgentState::Blocked, 20),
            Waiting { waited: None, ..waiting(5, "s", AgentState::Done) },
        ];
        sort_waiting(&mut list);
        assert_eq!(list.iter().map(|w| w.pane).collect::<Vec<_>>(), vec![4, 2, 3, 1, 5]);
    }

    #[test]
    fn equal_waits_keep_one_order_whatever_order_they_arrive_in() {
        let w = |pane| Waiting { waited: Some(std::time::Duration::from_secs(12)), ..waiting(pane, "s", AgentState::Done) };
        for arrival in [[1, 2, 3], [3, 1, 2], [2, 3, 1]] {
            let mut list: Vec<_> = arrival.into_iter().map(w).collect();
            sort_waiting(&mut list);
            assert_eq!(list.iter().map(|w| w.pane).collect::<Vec<_>>(), vec![1, 2, 3], "arrived as {arrival:?}");
        }
    }

    #[test]
    fn the_wait_reads_in_whole_units() {
        let s = std::time::Duration::from_secs;
        assert_eq!(ago(s(0)), "0s");
        assert_eq!(ago(s(59)), "59s");
        assert_eq!(ago(s(60)), "1m");
        assert_eq!(ago(s(3599)), "59m");
        assert_eq!(ago(s(3600)), "1h");
        assert_eq!(ago(s(86399)), "23h");
        assert_eq!(ago(s(2 * 86400)), "2d");
        assert_eq!(ago(s(1_790_000_000)), "99d", "never a count from the epoch");
    }

    #[test]
    fn a_space_is_one_line_with_its_branch_flush_right() {
        let (buf, hits) = render(&[], &[space(0, "nerd", "master", AgentState::Idle)]);
        let row = line(&buf, FIRST);
        assert!(row.starts_with("│ ○ nerd"), "{row:?}");
        assert!(row.ends_with("master │"), "{row:?}");
        assert_eq!(buf[(22, FIRST)].fg, Theme::dark().faint);
        assert_eq!(hits.spaces, vec![(Rect::new(1, FIRST, W - 2, 1), 0)]);
        assert_eq!(hits.new_space, Some(Rect::new(W - 5, 6, 3, 1)), "+ sits in the spaces title");
    }

    #[test]
    fn a_long_branch_is_cut_with_an_ellipsis_and_the_name_stays_whole() {
        let (buf, _) = render(&[], &[space(0, "dashboard", "feature/1234-primitives-and-more", AgentState::Working)]);
        let row = line(&buf, FIRST);
        assert!(row.contains("dashboard  feature/"), "{row:?}");
        assert!(row.contains('…') && row.ends_with("… │"), "{row:?}");
    }

    #[test]
    fn panes_hang_under_their_space_only_when_there_is_more_than_one() {
        let mut nerd = space(0, "nerd", "master", AgentState::Working);
        nerd.panes = vec![
            pane(1, true, "claude", Some("main"), AgentState::Working),
            pane(2, false, "vite", Some("server"), AgentState::Idle),
        ];
        let mut solo = space(1, "web", "main", AgentState::Idle);
        solo.panes = vec![pane(3, true, "claude", None, AgentState::Idle)];
        let (buf, hits) = render(&[], &[nerd, solo]);

        let col = |y: u16, s: &str| line(&buf, y).chars().collect::<Vec<_>>().windows(s.chars().count()).position(|w| w.iter().collect::<String>() == s);
        assert_eq!((col(FIRST, "●"), col(FIRST + 1, "├"), col(FIRST + 2, "└")), (Some(2), Some(2), Some(2)), "the line starts under the space's mark");
        assert!(line(&buf, FIRST + 1).starts_with("│ ├ ● claude") && line(&buf, FIRST + 1).ends_with("main │"));
        assert!(line(&buf, FIRST + 2).starts_with("│ └ › vite") && line(&buf, FIRST + 2).ends_with("server │"));
        assert_eq!(line(&buf, FIRST + 3).trim_matches(|c| c == '│' || c == ' '), "", "air after an opened space");
        assert!(line(&buf, FIRST + 4).starts_with("│ ○ web"), "a single pane stays folded");
        assert_eq!(hits.panes.iter().map(|(_, p)| *p).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(hits.spaces.iter().map(|(r, i)| (r.y, *i)).collect::<Vec<_>>(), vec![(FIRST, 0), (FIRST + 4, 1)]);
    }

    #[test]
    fn a_pane_and_its_subagents_share_the_mark_column() {
        let mut claude = pane(1, true, "claude", None, AgentState::Working);
        claude.subagents = two_subs();
        claude.open = true;
        let mut nerd = space(0, "nerd", "master", AgentState::Working);
        nerd.panes = vec![claude, pane(2, false, "vite", None, AgentState::Idle)];
        let (buf, _) = render(&[], &[nerd]);
        let col = |y: u16, s: &str| line(&buf, y).chars().skip(1).position(|c| c.to_string() == s).map(|c| c + 1);
        assert_eq!((col(FIRST + 1, "●"), col(FIRST + 2, "├"), col(FIRST + 3, "└")), (Some(4), Some(4), Some(4)), "the subagents hang from the pane's mark");
        assert_eq!((col(FIRST + 2, "│"), col(FIRST + 3, "│")), (Some(2), Some(2)), "the bar keeps the space's line: the space's mark column");
    }

    fn sub(id: &str, kind: &str, doing: &str, state: crate::hook::SubState, secs: u64, stale: bool) -> SubRow {
        SubRow { id: id.into(), kind: kind.into(), doing: doing.into(), state, age: std::time::Duration::from_secs(secs), stale, open: false }
    }

    fn two_subs() -> Vec<SubRow> {
        use crate::hook::SubState::*;
        vec![sub("a1", "Explore", "Read", Running, 75, false), sub("a2", "critic", "Bash", Done, 5, false)]
    }

    #[test]
    fn subagents_are_a_count_until_the_human_opens_them() {
        let mut claude = pane(1, true, "claude", None, AgentState::Working);
        claude.subagents = two_subs();
        let mut solo = space(0, "nerd", "master", AgentState::Working);
        solo.panes = vec![claude];
        let below = space(1, "web", "main", AgentState::Idle);

        // closed: the count sits on the space's line, and nothing below moves
        let (buf, hits) = render(&[], &[solo, below]);
        assert!(line(&buf, FIRST).starts_with("│ ● nerd ◦1"), "only the running one counts: {:?}", line(&buf, FIRST));
        assert_eq!(buf[(9, FIRST)].fg, Theme::dark().warn);
        assert_eq!(hits.subagent_toggles, vec![(Rect::new(9, FIRST, 2, 1), 1)]);
        assert!(line(&buf, FIRST + 1).starts_with("│ ○ web"), "{:?}", line(&buf, FIRST + 1));

        // none running (one done, one whose stop was lost): the rows still open from a faint count
        let mut claude = pane(1, true, "claude", None, AgentState::Idle);
        claude.subagents = vec![sub("a1", "Explore", "no report", crate::hook::SubState::Unknown, 75, false), sub("a2", "critic", "Bash", crate::hook::SubState::Done, 5, false)];
        let mut solo = space(0, "nerd", "master", AgentState::Idle);
        solo.panes = vec![claude];
        let (buf, hits) = render(&[], &[solo]);
        assert!(line(&buf, FIRST).contains("◦2"), "{:?}", line(&buf, FIRST));
        assert_eq!(buf[(9, FIRST)].fg, Theme::dark().faint, "nothing running: not in warn");
        assert_eq!(hits.subagent_toggles.len(), 1, "still a toggle");

        // opened by the human: the list hangs under the space, one row per subagent, each a click target
        let mut claude = pane(1, true, "claude", None, AgentState::Working);
        claude.subagents = two_subs();
        claude.open = true;
        let mut solo = space(0, "nerd", "master", AgentState::Working);
        solo.panes = vec![claude];
        let (buf, hits) = render(&[], &[solo, space(1, "web", "main", AgentState::Idle)]);
        let t = Theme::dark();
        assert!(line(&buf, FIRST + 1).starts_with("│ ├ ● Explore · Read"), "{:?}", line(&buf, FIRST + 1));
        assert!(line(&buf, FIRST + 1).ends_with("1m │"), "the time since it started: {:?}", line(&buf, FIRST + 1));
        assert_eq!((buf[(4, FIRST + 1)].fg, buf[(W - 4, FIRST + 1)].fg), (t.teal, t.faint));
        assert!(line(&buf, FIRST + 2).starts_with("│ └ ✓ critic · Bash"), "a finished one keeps its last tool: {:?}", line(&buf, FIRST + 2));
        assert_eq!(buf[(4, FIRST + 2)].fg, t.ok);
        assert!(line(&buf, FIRST + 4).starts_with("│ ○ web"));
        assert!(hits.panes.is_empty(), "a subagent's row no longer focuses the pane");
        let rows: Vec<_> = hits.subagent_rows.iter().map(|(r, p, id)| (r.y, *p, id.as_str())).collect();
        assert_eq!(rows, vec![(FIRST + 1, 1, "a1"), (FIRST + 2, 1, "a2")]);
    }

    #[test]
    fn a_row_names_the_last_tool_in_every_state_and_the_task_only_before_any() {
        use crate::hook::{Call, Outcome, Sub, SubState};
        let call = |tool: &str, outcome| Call { id: String::new(), tool: tool.into(), target: String::new(), at: 100, outcome, summary: String::new() };
        let base = Sub { id: "a".into(), kind: "Explore".into(), task: "find the callers".into(), started: 100, last: 100, ..Sub::default() };
        let done = Sub { stopped: Some(110), calls: vec![call("Read", Outcome::Ok), call("Bash", Outcome::Ok)], ..base.clone() };
        let failed = Sub { calls: vec![call("Edit", Outcome::Failed)], ..base.clone() };
        let row = |s: &Sub| {
            let r = SubRow::of(s, 140);
            (r.doing, r.state)
        };
        assert_eq!(row(&done), ("Bash".into(), SubState::Done));
        assert_eq!(row(&failed), ("Edit".into(), SubState::Failed));
        assert_eq!(row(&base), ("find the callers".into(), SubState::Running), "no call yet: the task says what it is for");
        let lost = Sub { alone: true, last: 130, calls: vec![call("Bash", Outcome::Ok)], ..base.clone() };
        assert_eq!(row(&lost), ("Bash".into(), SubState::Running));
        let quiet = SubRow::of(&lost, 100 + 5 * crate::hook::QUIET);
        assert_eq!(quiet.doing, "no report", "silent after the turn ended");
        assert!(!quiet.stale && quiet.age.as_secs() == 30, "its clock stopped at its last word, not ticking in warning");
        assert_eq!(SubRow::of(&base, 175).age, std::time::Duration::from_secs(75));
    }

    #[test]
    fn a_finished_row_names_the_work_not_the_report_that_ended_it() {
        // from the owner's session: an Explore whose last call was its handback, which has no target
        let events = "1790239605\ta086\tstart\tt1\tBash\tls src\tExplore\n\
                      1790239608\ta086\tok\tt1\tapp.rs  193.0K\n\
                      1790239650\ta086\tstart\tt7\tBash\tgrep \tExplore\n\
                      1790239651\ta086\tok\tt7\t237:    /// The human chose\n\
                      1790239656\ta086\tstart\tt8\tSubagentHandback\t\tExplore\n\
                      1790239657\ta086\tok\tt8\tReport delivered to your caller.\n\
                      1790239665\ta086\tstop\tExplore\tDemo subagent for the sidebar\t1790239602\n";
        let a = crate::hook::parse_activity(events, "");
        let row = SubRow::of(&a.subs[0], 1790239670);
        assert_eq!((row.kind.as_str(), row.doing.as_str(), row.state), ("Explore", "Bash", crate::hook::SubState::Done));
    }

    #[test]
    fn a_stuck_or_failed_subagent_shows_without_reading() {
        use crate::hook::SubState::*;
        let mut claude = pane(1, true, "claude", None, AgentState::Working);
        claude.subagents = vec![sub("a1", "Explore", "Bash", Running, 300, true), sub("a2", "critic", "Edit", Failed, 40, false)];
        claude.open = true;
        let mut solo = space(0, "nerd", "master", AgentState::Working);
        solo.panes = vec![claude];
        let (buf, _) = render(&[], &[solo]);
        let t = Theme::dark();
        assert!(line(&buf, FIRST + 1).ends_with("5m │"));
        assert_eq!(buf[(W - 4, FIRST + 1)].fg, t.warn, "quiet for two minutes: the time turns warn");
        assert!(line(&buf, FIRST + 2).starts_with("│ └ ✕ critic · Edit"), "{:?}", line(&buf, FIRST + 2));
        assert_eq!(buf[(4, FIRST + 2)].fg, t.bad);
    }

    #[test]
    fn mail_rows_share_needs_you_with_the_mark_of_a_blocked_agent() {
        let spaces = [space(0, "nerd", "master", AgentState::Idle)];
        let mail = [MailRow { id: "m1".into(), text: "coder → vault · held".into(), waited: std::time::Duration::from_secs(5) }];
        let area = Rect::new(0, 0, W, H);
        let mut buf = Buffer::empty(area);
        let hits = draw(&mut buf, area, &[waiting(7, "shop", AgentState::Done)], &mail, &spaces, true, &Theme::dark());
        assert!(line(&buf, 3).starts_with("│ ● coder → vault · held") && line(&buf, 3).trim_end_matches(['│', ' ']).ends_with("5s"), "{:?}", line(&buf, 3));
        assert_eq!(buf[(2, 3)].fg, Theme::dark().wait);
        assert_eq!(hits.mail, vec![(Rect::new(1, 3, W - 2, 1), "m1".to_string())]);
        assert!((0..H).all(|y| !line(&buf, y).contains('\u{2709}')));

        let rail = Rect::new(0, 0, RAIL_W, 12);
        let mut buf = Buffer::empty(rail);
        let hits = draw_rail(&mut buf, rail, &[], &mail, &spaces, &Theme::dark());
        assert_eq!(((0..RAIL_W).map(|x| buf[(x, 1)].symbol()).collect::<String>(), buf[(2, 1)].fg), ("│ ● │".to_string(), Theme::dark().wait));
        assert_eq!(hits.mail.len(), 1);
        assert_eq!(buf[(2, 5)].symbol(), "○", "the space keeps its own state mark");
    }

    #[test]
    fn a_row_names_its_pane_before_its_space() {
        let spaces = [space(0, "nerd", "master", AgentState::Blocked)];
        let rev = Waiting { name: "rev".into(), ..waiting(8, "nerd", AgentState::Done) };
        let (buf, hits) = render(&[waiting(7, "nerd", AgentState::Done), rev], &spaces);
        assert!(line(&buf, 2).starts_with("│ ✓ claude · nerd"), "{:?}", line(&buf, 2));
        assert!(line(&buf, 3).starts_with("│ ✓ rev · nerd"), "{:?}", line(&buf, 3));
        assert_eq!(hits.panes.iter().map(|h| h.1).collect::<Vec<_>>(), vec![7, 8]);
    }

    #[test]
    fn a_blocked_agent_says_what_it_asks() {
        let spaces = [space(0, "nerd", "master", AgentState::Blocked)];
        let w = Waiting { asks: Some("Bash".into()), ..waiting(7, "shop", AgentState::Blocked) };
        let (buf, _) = render(&[w], &spaces);
        assert!(line(&buf, 2).starts_with("│ ● claude · Allow Bash?"), "{:?}", line(&buf, 2));
        let done = Waiting { asks: Some("Bash".into()), ..waiting(7, "shop", AgentState::Done) };
        let (buf, _) = render(&[done], &spaces);
        assert!(!line(&buf, 2).contains("Allow"), "only a blocked one asks");
        let failed = waiting(8, "web", AgentState::Failed);
        let (buf, _) = render(&[failed], &spaces);
        assert!(line(&buf, 2).starts_with("│ ✕ claude · web"));
        assert_eq!(buf[(2, 2)].fg, Theme::dark().bad);
    }

    #[test]
    fn a_worktree_group_keeps_its_tree_and_a_collapsed_one_speaks_for_all() {
        let group = |mut s: SpaceNode, parent: bool| {
            s.group = Some(PathBuf::from("/r/.git"));
            s.is_group_parent = parent;
            s
        };
        let spaces = [
            group(space(0, "nerd", "master", AgentState::Idle), true),
            space(1, "plain", "main", AgentState::Idle),
            group(space(2, "nerd-feat", "feat", AgentState::Blocked), false),
        ];
        let (buf, hits) = render(&[], &spaces);
        assert!(line(&buf, FIRST).starts_with("│ ▾ ○ nerd"), "{:?}", line(&buf, FIRST));
        assert!(line(&buf, FIRST + 1).starts_with("│   └─ ● nerd-feat"), "the worktree moved up under its checkout");
        assert!(line(&buf, FIRST + 2).starts_with("│ ○ plain"));
        assert_eq!(hits.group_toggles, vec![(Rect::new(2, FIRST, 1, 1), 0)]);

        let mut folded = spaces;
        folded[0].collapsed = true;
        let (buf, hits) = render(&[], &folded);
        assert!(line(&buf, FIRST).starts_with("│ ▸ ● nerd"), "the hidden worktree's wait shows on the checkout");
        assert_eq!(buf[(4, FIRST)].fg, Theme::dark().wait);
        assert_eq!(hits.spaces.iter().map(|(_, i)| *i).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn the_active_space_is_chosen_and_the_cursor_covers_its_row_inside_the_border() {
        let theme = Theme::dark();
        let mut s = space(0, "nerd", "master", AgentState::Idle);
        s.active = true;
        let (buf, _) = render(&[], std::slice::from_ref(&s));
        assert_eq!(buf[(4, FIRST)].fg, theme.accent, "the active name takes the accent");
        assert!(buf[(4, FIRST)].modifier.contains(Modifier::BOLD), "and the weight, so colour is not alone");
        assert_eq!(buf[(1, FIRST)].bg, Color::Reset, "chosen is not a band");

        s.selected = true;
        let want = theme.cursor();
        for keys in [true, false] {
            let area = Rect::new(0, 0, W, H);
            let mut buf = Buffer::empty(area);
            draw(&mut buf, area, &[], &[], std::slice::from_ref(&s), keys, &theme);
            assert!((1..W - 1).all(|x| buf[(x, FIRST)].bg == want.bg.unwrap() && buf[(x, FIRST)].fg == want.fg.unwrap()),
                "keys {keys}: the whole row, the state mark and the branch included");
            assert_ne!(buf[(0, FIRST)].bg, want.bg.unwrap(), "the border stays the border");
            assert_eq!(buf[(0, FIRST)].fg, if keys { theme.border_focus } else { theme.border }, "the frame is lit while the keys are here");
            let divider = (0..H).find(|&y| buf[(0, y)].symbol() == "\u{251c}").unwrap();
            assert!((0..W).filter(|&x| buf[(x, divider)].symbol() == "\u{2500}").all(|x| buf[(x, divider)].fg == buf[(0, FIRST)].fg),
                "the divider lights with the frame");
            let title = (0..W).find(|&x| buf[(x, divider)].symbol() == "s").unwrap();
            assert_eq!(buf[(title, divider)].fg, if keys { theme.accent } else { theme.muted }, "and so does the block's title");
            assert!(buf[(4, FIRST)].modifier.contains(Modifier::BOLD), "chosen stays bold under the cursor");
        }
    }

    #[test]
    fn nothing_spills_past_the_frame_on_a_small_area() {
        let mut spaces: Vec<SpaceNode> = (0..20).map(|i| space(i, "a-very-long-space-name-indeed", "branch", AgentState::Idle)).collect();
        spaces[0].panes = (0..5).map(|p| pane(p, true, "claude-with-a-long-name", Some("tab"), AgentState::Idle)).collect();
        let (buf, hits) = render(&[waiting(1, "space", AgentState::Done)], &spaces);
        for y in 1..H - 1 {
            let row = line(&buf, y);
            assert!(row.starts_with('│') || row.starts_with('├'), "left border on row {y}: {row:?}");
            assert!(row.ends_with('│') || row.ends_with('┤'), "right border on row {y}: {row:?}");
        }
        assert!(hits.spaces.iter().chain(&hits.panes.iter().map(|(r, _)| (*r, 0)).collect::<Vec<_>>()).all(|(r, _)| r.bottom() < H));
    }

    #[test]
    fn the_rail_keeps_a_mark_per_waiting_agent_and_per_space() {
        let area = Rect::new(0, 0, RAIL_W, 12);
        let mut buf = Buffer::empty(area);
        let mut active = space(1, "dashboard", "main", AgentState::Working);
        active.active = true;
        let spaces = [space(0, "nerd", "master", AgentState::Idle), active];
        let hits = draw_rail(&mut buf, area, &[waiting(9, "shop", AgentState::Blocked)], &[], &spaces, &Theme::dark());
        let row = |y: u16| (0..RAIL_W).map(|x| buf[(x, y)].symbol()).collect::<String>();
        assert_eq!(row(1), "│ ● │", "the waiting agent");
        assert_eq!(row(4), "├───┤", "after three rows, whatever waits");
        assert_eq!((row(5), row(6)), ("│ ○ │".to_string(), "│ ● │".to_string()), "one mark per space");
        assert_eq!(buf[(1, 6)].bg, Theme::dark().slab, "the active space stays marked, visibly");
        assert_eq!(row(11), "└ » ┘");
        assert_eq!(hits.panes, vec![(Rect::new(1, 1, 3, 1), 9)]);
        assert_eq!(hits.spaces.iter().map(|(r, i)| (r.y, *i)).collect::<Vec<_>>(), vec![(5, 0), (6, 1)]);
        assert_eq!(hits.toggle, Some(Rect::new(1, 11, 3, 1)));
    }

    #[test]
    fn the_expanded_sidebar_offers_to_collapse_in_its_bottom_line() {
        let (buf, hits) = render(&[], &[space(0, "nerd", "master", AgentState::Idle)]);
        assert!(line(&buf, H - 1).starts_with("└─ « ─"), "{:?}", line(&buf, H - 1));
        assert_eq!(hits.toggle, Some(Rect::new(2, H - 1, 3, 1)));
    }

    #[test]
    fn state_colours_follow_the_reference() {
        let t = Theme::dark();
        assert_eq!(state_color(AgentState::Working, &t), t.warn);
        assert_eq!(state_color(AgentState::Blocked, &t), t.wait, "waiting is your turn, not a problem");
        assert_eq!(state_color(AgentState::Failed, &t), t.bad);
        assert_eq!(state_color(AgentState::Done, &t), t.teal);
        assert_eq!(state_color(AgentState::Idle, &t), t.muted, "a calm pane carries no color; the accent's green is the brand's");
        assert_eq!(state_color(AgentState::Unknown, &t), t.faint);
    }
}
