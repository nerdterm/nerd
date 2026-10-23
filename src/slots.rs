//! What plugins and public built-ins put on screen: badges on tree rows and tabs, status segments in the
//! top row, and previews by file type. One ingest checks every message the same way, wherever it came from; nerd draws the result.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ratatui::style::Color;

use crate::proto::PluginMsg;
use crate::render::Theme;

pub const ITEMS: usize = 5000;
pub const BADGE_CELLS: usize = 4;
/// Wider than `BADGE_CELLS`: a tab badge's count carries its unit word, a tree badge does not.
pub const TAB_BADGE_CELLS: usize = 8;
pub const SEGMENT_CELLS: usize = 24;
pub const SEGMENTS: usize = 4;
pub const PREVIEW_LINES: usize = 2000;
pub const LINE_CELLS: usize = 512;
const ID_LEN: usize = 64;
/// Views one preview may offer, and the cells of a view's label.
const MODES: usize = 4;
const MODE_CELLS: usize = 16;
pub const PANELS: usize = 4;
pub const PANEL_DEPTH: usize = 8;
const ITEM_ID: usize = 1024;
const TITLE_CELLS: usize = 64;

/// The theme roles a plugin may name; never a raw colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Text,
    Muted,
    Faint,
    Accent,
    Ok,
    Warn,
    Bad,
    Teal,
}

pub const ROLES: &[(&str, Role)] = &[
    ("text", Role::Text),
    ("muted", Role::Muted),
    ("faint", Role::Faint),
    ("accent", Role::Accent),
    ("ok", Role::Ok),
    ("warn", Role::Warn),
    ("bad", Role::Bad),
    ("teal", Role::Teal),
];

impl Role {
    /// Absent is `text`; a name outside the set refuses the message.
    fn parse(s: &str) -> Result<Role, String> {
        if s.is_empty() {
            return Ok(Role::Text);
        }
        ROLES.iter().find(|(n, _)| *n == s).map(|(_, r)| *r).ok_or_else(|| format!("{:?} is not a role", crate::log::escape(s)))
    }

    pub fn color(self, t: &Theme) -> Color {
        match self {
            Role::Text => t.text,
            Role::Muted => t.muted,
            Role::Faint => t.faint,
            Role::Accent => t.accent,
            Role::Ok => t.ok,
            Role::Warn => t.warn,
            Role::Bad => t.bad,
            Role::Teal => t.teal,
        }
    }
}

/// Plugin text as it may reach the screen: no control or bidi characters.
pub fn clean(s: &str) -> String {
    crate::render::unspoofed(s)
}

pub fn cells(s: &str) -> usize {
    ratatui::text::Span::raw(s).width()
}

/// Bytes a cell may take: combining marks are zero cells wide, so a cap on cells alone lets megabytes through.
const CELL_BYTES: usize = 32;

/// Over `cap` cells, or over what that many cells can honestly take in bytes.
fn wide(s: &str, cap: usize) -> bool {
    s.len() > cap * CELL_BYTES || cells(&clean(s)) > cap
}

/// Each grapheme of `s` as the terminal draws it, an emoji sequence as one: its byte offset in `s`, the grapheme and
/// its width in cells; `f` returns false to stop. A grapheme with a control character is skipped, as a frame skips it.
pub fn graphemes(s: &str, mut f: impl FnMut(usize, &str, usize) -> bool) {
    let span = ratatui::text::Span::raw(s);
    for g in span.styled_graphemes(ratatui::style::Style::default()) {
        // the span borrows `s`, so each grapheme is a slice of it
        if !f(g.symbol.as_ptr() as usize - s.as_ptr() as usize, g.symbol, cells(g.symbol)) {
            break;
        }
    }
}

/// `s` cut to `max` cells, with an ellipsis when cut; one pass, so a line of pane output costs its length.
pub fn fit(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_string();
    }
    let (mut out, mut used) = (String::new(), 0);
    graphemes(s, |_, g, w| {
        if used + w + 1 > max {
            return false;
        }
        out.push_str(g);
        used += w;
        true
    });
    out + "\u{2026}"
}

/// Items of a panel at any depth, and how deep it goes.
fn size(items: &[crate::proto::PanelItem]) -> (usize, usize) {
    items.iter().map(|i| size(&i.children)).fold((items.len(), 0), |(n, d), (cn, cd)| (n + cn, d.max(cd + 1)))
}

fn too_wide(lines: &[Vec<crate::proto::TextSpan>], what: &str) -> Option<String> {
    lines
        .iter()
        .position(|l| l.iter().map(|s| s.text.len()).sum::<usize>() > LINE_CELLS * CELL_BYTES || l.iter().map(|s| cells(&clean(&s.text))).sum::<usize>() > LINE_CELLS)
        .map(|i| format!("{what} line {} is wider than {LINE_CELLS} cells", i + 1))
}

fn bad_item(items: &[crate::proto::PanelItem]) -> Option<String> {
    items.iter().find_map(|i| {
        if i.id.is_empty() || i.id.len() > ITEM_ID {
            Some(format!("an item id is empty or longer than {ITEM_ID}"))
        } else if i.label.len() + i.detail.len() > LINE_CELLS * CELL_BYTES || cells(&clean(&i.label)) + cells(&clean(&i.detail)) > LINE_CELLS {
            Some(format!("item {:?} is wider than {LINE_CELLS} cells", crate::log::escape(&i.label)))
        } else {
            bad_item(&i.children)
        }
    })
}

/// Why a message is over the caps, if it is; the reader refuses these before they are queued.
pub fn over_cap(m: &PluginMsg) -> Option<String> {
    match m {
        PluginMsg::Badges { items, .. } if items.len() > ITEMS => Some(format!("{} badges, more than {ITEMS}", items.len())),
        PluginMsg::Badges { items, target } => {
            let cap = if target == "tree" { BADGE_CELLS } else { TAB_BADGE_CELLS };
            items.iter().find(|i| wide(&i.text, cap)).map(|i| format!("badge {:?} is wider than {cap} cells", crate::log::escape(&i.text)))
        }
        PluginMsg::Segment { text, .. } if wide(text, SEGMENT_CELLS) => {
            Some(format!("segment {:?} is wider than {SEGMENT_CELLS} cells", crate::log::escape(text)))
        }
        PluginMsg::Segment { id, .. } | PluginMsg::SegmentClear { id } if id.len() > ID_LEN => Some(format!("a segment id is longer than {ID_LEN}")),
        PluginMsg::Preview { lines, .. } if lines.len() > PREVIEW_LINES => Some(format!("{} preview lines, more than {PREVIEW_LINES}", lines.len())),
        PluginMsg::Preview { modes, .. } if modes.len() > MODES => Some(format!("{} preview views, more than {MODES}", modes.len())),
        PluginMsg::Preview { modes, mode, .. } if !mode.is_empty() && !modes.iter().any(|m| m.id == *mode) => Some(format!("preview view {:?} is not among its views", crate::log::escape(mode))),
        PluginMsg::Preview { modes, .. } if modes.iter().any(|m| m.id.is_empty() || m.id.len() > ID_LEN || wide(&m.label, MODE_CELLS)) => {
            Some(format!("a preview view needs an id of at most {ID_LEN} bytes and a label of at most {MODE_CELLS} cells"))
        }
        PluginMsg::Preview { lines, .. } => too_wide(lines, "preview"),
        PluginMsg::Panel { id, .. } if id.len() > ID_LEN => Some(format!("a panel id is longer than {ID_LEN}")),
        PluginMsg::Panel { title, .. } if wide(title, TITLE_CELLS) => Some(format!("a panel title is wider than {TITLE_CELLS} cells")),
        PluginMsg::Panel { items, lines, .. } => {
            let (n, depth) = size(items);
            if n > ITEMS {
                Some(format!("{n} panel items, more than {ITEMS}"))
            } else if depth > PANEL_DEPTH {
                Some(format!("panel items {depth} deep, more than {PANEL_DEPTH}"))
            } else if lines.len() > PREVIEW_LINES {
                Some(format!("{} panel lines, more than {PREVIEW_LINES}", lines.len()))
            } else {
                too_wide(lines, "panel").or_else(|| bad_item(items))
            }
        }
        _ => None,
    }
}

/// The slots a source declared: a plugin's manifest, or a public built-in's own list.
#[derive(Clone, Debug, Default)]
pub struct Decl {
    pub name: String,
    pub builtin: bool,
    pub badges: Vec<String>,
    pub segments: Vec<String>,
    pub commands: Vec<String>,
    /// The file extensions it previews.
    pub previews: Vec<String>,
    /// Its panels: id, title, and the menu that opens each.
    pub panels: Vec<(String, String, crate::agents::Place)>,
}

/// Public built-ins; their names are refused to plugins.
pub const BUILTINS: &[&str] = &["previews"];

impl Decl {
    pub fn of(m: &crate::agents::Manifest) -> Decl {
        Decl {
            name: m.name.clone(),
            builtin: false,
            badges: m.badges.clone(),
            segments: m.segments.iter().map(|s| s.id.clone()).collect(),
            commands: m.commands.iter().map(|c| c.id.clone()).collect(),
            previews: m.previews.iter().flat_map(|p| p.extensions.clone()).collect(),
            panels: m.panels.iter().map(|p| (p.id.clone(), p.title.clone(), p.place)).collect(),
        }
    }

    pub fn builtin(name: &str) -> Decl {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
        let (badges, segments, previews) = match name {
            "previews" => (v(&[]), v(&[]), v(crate::previews::EXTENSIONS)),
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };
        Decl { name: name.into(), builtin: true, badges, segments, commands: Vec::new(), previews, panels: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    pub text: String,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub source: String,
    pub text: String,
    pub role: Role,
    pub command: Option<String>,
}

/// A line of a preview: runs of text, each in its role.
pub type Line = Vec<(String, Role)>;

/// A source's answer to preview request `req`.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub source: String,
    pub req: u64,
    pub lines: Vec<Line>,
    pub cut: bool,
    /// (id, label) of each view the plugin offers, and the one these lines are.
    pub modes: Vec<(String, String)>,
    pub mode: String,
}

/// A source, built-ins first, then plugins by name.
pub type Src = (bool, String);
/// Per source, so a message from one copies none of the others' while the tree holds the map.
pub type TreeBadges = BTreeMap<Src, Arc<HashMap<PathBuf, Badge>>>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Tree,
    Tab,
    Segment(String),
    Panel(String),
}

enum Slot {
    Tree(HashMap<PathBuf, Badge>),
    Tab(HashMap<usize, Badge>),
    /// Its place in the manifest's list, and what it says now.
    Segment(usize, Option<Segment>),
    Panel(crate::panel::Data),
}

/// Latest-wins per (source, key): taken whenever a message comes, applied once per frame.
#[derive(Default)]
pub struct Store {
    pending: BTreeMap<(Src, Key), Slot>,
    pub tree: Arc<TreeBadges>,
    tabs: BTreeMap<Src, HashMap<usize, Badge>>,
    segments: BTreeMap<(Src, usize), Segment>,
    panels: BTreeMap<(Src, String), crate::panel::Data>,
    warned: HashSet<String>,
    /// Preview answers not yet matched against what the tree asked; the loop takes them.
    pub answers: Vec<Answer>,
    /// Frames that applied something.
    pub applied: usize,
}

fn items(items: Vec<crate::proto::PanelItem>) -> Result<Vec<crate::panel::Item>, String> {
    items
        .into_iter()
        .map(|i| Ok(crate::panel::Item { label: clean(&i.label), detail: clean(&i.detail), role: Role::parse(&i.role)?, children: self::items(i.children)?, opens: i.opens, id: i.id }))
        .collect()
}

fn lines(lines: &[Vec<crate::proto::TextSpan>]) -> Result<Vec<Line>, String> {
    lines.iter().map(|l| l.iter().map(|s| Ok((clean(&s.text), Role::parse(&s.role)?))).collect()).collect()
}

fn badge(text: &str, role: &str) -> Result<Badge, String> {
    Ok(Badge { text: clean(text), role: Role::parse(role)? })
}

impl Store {
    /// Checks a message against its source's declaration and queues it; a refusal is logged once per source.
    pub fn take(&mut self, d: &Decl, m: PluginMsg) -> bool {
        let what = m.name();
        match self.ingest(d, m) {
            Ok(()) => true,
            Err(why) => {
                if self.warned.insert(d.name.clone()) {
                    crate::log::warn(&format!("plugin {}: {what} refused: {why}", d.name));
                }
                false
            }
        }
    }

    pub fn ingest(&mut self, d: &Decl, m: PluginMsg) -> Result<(), String> {
        if let Some(why) = over_cap(&m) {
            return Err(why);
        }
        let src = (!d.builtin, d.name.clone());
        let (key, slot) = match m {
            PluginMsg::ReportState { .. } => return Err("ReportState is not a slot".into()),
            PluginMsg::Badges { target, items } => {
                if !d.badges.contains(&target) {
                    return Err(format!("badges on {:?} are not in its manifest", crate::log::escape(&target)));
                }
                match target.as_str() {
                    "tree" => {
                        let mut map = HashMap::new();
                        for i in items {
                            let p = i.path.ok_or("a tree badge needs a path")?;
                            if !p.is_absolute() || p.components().any(|c| c == Component::ParentDir) {
                                return Err(format!("{:?} is not an absolute path without ..", crate::log::escape(&p.to_string_lossy())));
                            }
                            map.insert(p, badge(&i.text, &i.role)?);
                        }
                        (Key::Tree, Slot::Tree(map))
                    }
                    _ => {
                        let mut map = HashMap::new();
                        for i in items {
                            map.insert(i.pane.ok_or("a tab badge needs a pane")?, badge(&i.text, &i.role)?);
                        }
                        (Key::Tab, Slot::Tab(map))
                    }
                }
            }
            PluginMsg::Segment { id, text, role, command } => {
                let at = d.segments.iter().position(|s| *s == id).ok_or_else(|| format!("segment {:?} is not in its manifest", crate::log::escape(&id)))?;
                if let Some(c) = command.as_ref().filter(|c| !d.commands.contains(c)) {
                    return Err(format!("segment command {:?} is not one of its commands", crate::log::escape(c)));
                }
                let seg = Segment { source: d.name.clone(), text: clean(&text), role: Role::parse(&role)?, command };
                (Key::Segment(id), Slot::Segment(at, Some(seg)))
            }
            PluginMsg::SegmentClear { id } => {
                let at = d.segments.iter().position(|s| *s == id).ok_or_else(|| format!("segment {:?} is not in its manifest", crate::log::escape(&id)))?;
                (Key::Segment(id), Slot::Segment(at, None))
            }
            PluginMsg::Preview { req, lines, cut, modes, mode } => {
                if d.previews.is_empty() {
                    return Err("previews are not in its manifest".into());
                }
                let lines = self::lines(&lines)?;
                let modes = modes.into_iter().map(|m| (m.id, clean(&m.label))).collect();
                self.answers.push(Answer { source: d.name.clone(), req, lines, cut, modes, mode });
                return Ok(());
            }
            PluginMsg::Panel { id, kind, title, items, lines } => {
                if !d.panels.iter().any(|p| p.0 == id) {
                    return Err(format!("panel {:?} is not in its manifest", crate::log::escape(&id)));
                }
                let kind = match kind.as_str() {
                    "list" => crate::panel::Kind::List,
                    "tree" => crate::panel::Kind::Tree,
                    "detail" => crate::panel::Kind::Detail,
                    _ => return Err(format!("{:?} is not a panel kind", crate::log::escape(&kind))),
                };
                let data = crate::panel::Data { kind, title: clean(&title), items: self::items(items)?, lines: self::lines(&lines)? };
                (Key::Panel(id), Slot::Panel(data))
            }
        };
        self.pending.insert((src, key), slot);
        Ok(())
    }

    /// Before a frame: what came since the last one takes effect. True when anything did.
    pub fn apply(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        for ((src, key), slot) in std::mem::take(&mut self.pending) {
            match slot {
                Slot::Tree(map) if map.is_empty() => {
                    Arc::make_mut(&mut self.tree).remove(&src);
                }
                Slot::Tree(map) => {
                    Arc::make_mut(&mut self.tree).insert(src, Arc::new(map));
                }
                Slot::Tab(map) if map.is_empty() => {
                    self.tabs.remove(&src);
                }
                Slot::Tab(map) => {
                    self.tabs.insert(src, map);
                }
                Slot::Segment(at, Some(s)) => {
                    self.segments.insert((src, at), s);
                }
                Slot::Segment(at, None) => {
                    self.segments.remove(&(src, at));
                }
                Slot::Panel(data) => {
                    let Key::Panel(id) = key else { continue };
                    self.panels.insert((src, id), data);
                }
            }
        }
        self.applied += 1;
        true
    }

    /// A source stopped: everything it put on screen goes. True when something did.
    pub fn clear(&mut self, name: &str) -> bool {
        self.warned.remove(name);
        let ours = |s: &Src| s.1 == name;
        let before = self.pending.len() + self.tree.len() + self.tabs.len() + self.segments.len() + self.panels.len();
        self.pending.retain(|(s, _), _| !ours(s));
        self.answers.retain(|a| a.source != name);
        if self.tree.keys().any(ours) {
            Arc::make_mut(&mut self.tree).retain(|s, _| !ours(s));
        }
        self.tabs.retain(|s, _| !ours(s));
        self.segments.retain(|(s, _), _| !ours(s));
        self.panels.retain(|(s, _), _| !ours(s));
        before != self.pending.len() + self.tree.len() + self.tabs.len() + self.segments.len() + self.panels.len()
    }

    /// One badge per source for a tab: of the first of its panes, in the order given, that has one.
    pub fn tab_badges(&self, panes: &[usize]) -> Vec<&Badge> {
        self.tabs.values().filter_map(|m| panes.iter().find_map(|p| m.get(p))).collect()
    }

    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.values()
    }

    pub fn panel(&self, src: &Src, id: &str) -> Option<&crate::panel::Data> {
        self.panels.get(&(src.clone(), id.to_string()))
    }

    /// Drops what a panel last showed: opened from another pane or row, it belonged to that one.
    pub fn forget_panel(&mut self, src: &Src, id: &str) {
        self.panels.remove(&(src.clone(), id.to_string()));
        self.pending.remove(&(src.clone(), Key::Panel(id.to_string())));
    }
}

/// The badges of one tree row, in source order.
pub fn tree_badges<'a>(t: &'a TreeBadges, p: &Path) -> Vec<&'a Badge> {
    t.values().filter_map(|m| m.get(p)).collect()
}

/// Where each segment of these widths starts in `[from, end)`: right-aligned, three cells apart; the rightmost drop first.
pub fn layout(widths: &[u16], from: u16, end: u16) -> Vec<u16> {
    let room = end.saturating_sub(from) as usize;
    let total = |n: usize| widths[..n].iter().map(|w| *w as usize).sum::<usize>() + 3 * n.saturating_sub(1);
    let mut n = widths.len();
    while n > 0 && total(n) > room {
        n -= 1;
    }
    let mut x = end - total(n) as u16;
    widths[..n]
        .iter()
        .map(|w| {
            let at = x;
            x += w + 3;
            at
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_measures_what_the_terminal_draws_in_one_pass() {
        let hearts = "\u{2764}\u{FE0F}".repeat(300);
        assert!(cells(&fit(&hearts, 100)) <= 100, "an emoji sequence is two cells, not one");
        let long = "ab".repeat(1_000_000);
        let t = std::time::Instant::now();
        assert_eq!(cells(&fit(&long, 1_000_000)), 1_000_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(1), "2 MB in one pass: {:?}", t.elapsed());
    }

    #[test]
    fn zero_width_text_is_capped_by_its_bytes() {
        let fat = format!("a{}", "\u{0301}".repeat(1_000_000));
        let m: PluginMsg = serde_json::from_value(serde_json::json!({ "Segment": { "id": "s", "text": fat } })).unwrap();
        assert!(over_cap(&m).is_some(), "megabytes in one cell are refused");
        let ok: PluginMsg = serde_json::from_value(serde_json::json!({ "Segment": { "id": "s", "text": "e\u{0301}\u{0302} 12°C" } })).unwrap();
        assert!(over_cap(&ok).is_none());
        // The widest real emoji take about 17 bytes a cell: two of them fill a tree badge.
        let kiss = "\u{1F469}\u{1F3FD}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F468}\u{1F3FB}".repeat(2);
        let badge: PluginMsg = serde_json::from_value(serde_json::json!({ "Badges": { "target": "tree", "items": [{ "path": "/x", "text": kiss }] } })).unwrap();
        assert!(over_cap(&badge).is_none(), "two kiss emoji fit a four-cell badge");
    }
    use crate::proto::BadgeItem;

    fn plug() -> Decl {
        Decl { name: "p".into(), builtin: false, badges: vec!["tree".into()], segments: vec!["a".into(), "b".into()], commands: vec!["go".into()], previews: vec!["xyz".into()], panels: vec![("p".into(), "P".into(), crate::agents::Place::Global)] }
    }

    fn seg(id: &str, text: &str) -> PluginMsg {
        PluginMsg::Segment { id: id.into(), text: text.into(), role: String::new(), command: None }
    }

    fn tree(items: Vec<(&str, &str)>) -> PluginMsg {
        PluginMsg::Badges { target: "tree".into(), items: items.into_iter().map(|(p, t)| BadgeItem { path: Some(p.into()), text: t.into(), ..Default::default() }).collect() }
    }

    #[test]
    fn a_message_from_one_source_copies_no_other_source_s_badges() {
        let mut s = Store::default();
        let q = Decl { name: "q".into(), ..plug() };
        s.ingest(&plug(), tree(vec![("/a", "1")])).unwrap();
        s.ingest(&q, tree(vec![("/b", "2")])).unwrap();
        s.apply();
        let held = s.tree.clone();
        s.ingest(&plug(), tree(vec![("/a", "3")])).unwrap();
        s.apply();
        let src = held.keys().find(|k| k.1 == "q").unwrap().clone();
        assert!(Arc::ptr_eq(&held[&src], &s.tree[&src]), "q's badges are shared, not copied");
        assert_eq!(tree_badges(&s.tree, Path::new("/a"))[0].text, "3");
        assert_eq!(tree_badges(&held, Path::new("/a"))[0].text, "1", "the tree keeps what it drew until the next frame");
    }

    #[test]
    fn every_rule_refuses_what_breaks_it() {
        let mut s = Store::default();
        let d = plug();
        let refused = |s: &mut Store, m: PluginMsg, why: &str| {
            let e = s.ingest(&d, m).unwrap_err();
            assert!(e.contains(why), "{e} lacks {why}");
        };
        refused(&mut s, PluginMsg::Badges { target: "tab".into(), items: vec![] }, "not in its manifest");
        refused(&mut s, seg("c", "x"), "not in its manifest");
        refused(&mut s, PluginMsg::SegmentClear { id: "c".into() }, "not in its manifest");
        refused(&mut s, PluginMsg::Segment { id: "a".into(), text: "x".into(), role: "pink".into(), command: None }, "not a role");
        refused(&mut s, PluginMsg::Segment { id: "a".into(), text: "x".into(), role: "#ff0000".into(), command: None }, "not a role");
        refused(&mut s, PluginMsg::Segment { id: "a".into(), text: "x".into(), role: String::new(), command: Some("rm".into()) }, "not one of its commands");
        refused(&mut s, seg("a", &"x".repeat(25)), "wider than 24");
        refused(&mut s, seg("a", &"界".repeat(13)), "wider than 24");
        refused(&mut s, seg(&"a".repeat(65), "x"), "longer than 64");
        refused(&mut s, tree(vec![("/a", "12345")]), "wider than 4");
        refused(&mut s, tree(vec![("/a", "界界界")]), "wider than 4");
        refused(&mut s, tree(vec![("rel/a", "1")]), "absolute");
        refused(&mut s, tree(vec![("/a/../etc", "1")]), "absolute");
        refused(&mut s, PluginMsg::Badges { target: "tree".into(), items: vec![BadgeItem { text: "1".into(), ..Default::default() }] }, "needs a path");
        let many = (0..=ITEMS).map(|i| BadgeItem { path: Some(format!("/f{i}").into()), text: "1".into(), ..Default::default() }).collect();
        refused(&mut s, PluginMsg::Badges { target: "tree".into(), items: many }, "more than 5000");
        refused(&mut s, PluginMsg::ReportState { pane: 1, state: "idle".into() }, "not a slot");
        assert!(!s.apply(), "nothing refused was queued");

        // At the caps, and spoofing and control characters taken out before the width is measured.
        assert!(s.ingest(&d, seg("a", &"界".repeat(12))).is_ok());
        assert!(s.ingest(&d, tree(vec![("/a", "界界"), ("/b", "a\u{202e}\u{1b}bcd")])).is_ok());
        let ok = (0..ITEMS).map(|i| BadgeItem { path: Some(format!("/f{i}").into()), text: "1".into(), ..Default::default() }).collect();
        assert!(s.ingest(&d, PluginMsg::Badges { target: "tree".into(), items: ok }).is_ok(), "5000 is allowed");
        s.ingest(&d, tree(vec![("/a", "界界"), ("/b", "a\u{202e}\u{1b}bcd")])).unwrap();
        assert!(s.apply());
        assert_eq!(tree_badges(&s.tree, Path::new("/b"))[0].text, "abcd");
        for (name, role) in ROLES {
            assert_eq!(Role::parse(name), Ok(*role));
        }
        assert_eq!(Role::parse(""), Ok(Role::Text));
    }

    #[test]
    fn latest_wins_and_a_frame_applies_once() {
        let mut s = Store::default();
        let d = plug();
        for i in 0..5000 {
            s.ingest(&d, seg("a", &format!("n{i}"))).unwrap();
            s.ingest(&d, tree(vec![("/x", &format!("{}", i % 1000))])).unwrap();
        }
        assert_eq!(s.pending.len(), 2, "one per key, however many came");
        assert!(s.apply());
        assert!(!s.apply(), "nothing new: the next frame applies nothing");
        assert_eq!(s.applied, 1);
        assert_eq!(s.segments().map(|g| g.text.as_str()).collect::<Vec<_>>(), ["n4999"], "the last one said");
        assert_eq!(tree_badges(&s.tree, Path::new("/x"))[0].text, "999");
        s.ingest(&d, PluginMsg::SegmentClear { id: "a".into() }).unwrap();
        s.apply();
        assert_eq!(s.segments().count(), 0);
    }

    #[test]
    fn plugins_come_by_name_and_a_stopped_source_leaves_nothing() {
        let mut s = Store::default();
        let (mut zed, mut abc) = (plug(), plug());
        zed.name = "zed".into();
        abc.name = "abc".into();
        s.ingest(&zed, seg("b", "zed-b")).unwrap();
        s.ingest(&zed, seg("a", "zed-a")).unwrap();
        s.ingest(&abc, seg("a", "abc")).unwrap();
        s.ingest(&abc, tree(vec![("/a", "1")])).unwrap();
        s.apply();
        assert_eq!(s.segments().map(|g| g.text.as_str()).collect::<Vec<_>>(), ["abc", "zed-a", "zed-b"]);
        s.ingest(&zed, seg("a", "late")).unwrap();
        assert!(s.clear("abc") && s.clear("zed"));
        assert!(!s.clear("zed"), "nothing left to clear");
        s.apply();
        assert!(s.segments().next().is_none(), "a queued message of a cleared source is gone too");
        assert!(s.tree.is_empty());
    }

    #[test]
    fn a_tab_shows_one_badge_per_source_from_its_first_pane_that_has_one() {
        let mut s = Store::default();
        let mut d = plug();
        d.badges = vec!["tab".into()];
        let item = |pane, text: &str| BadgeItem { pane: Some(pane), text: text.into(), ..Default::default() };
        s.ingest(&d, PluginMsg::Badges { target: "tab".into(), items: vec![item(1, "one"), item(2, "two")] }).unwrap();
        let a = Decl { name: "a".into(), ..d.clone() };
        s.ingest(&a, PluginMsg::Badges { target: "tab".into(), items: vec![item(2, "7")] }).unwrap();
        s.apply();
        let texts = |panes: &[usize]| s.tab_badges(panes).iter().map(|b| b.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&[2, 1]), ["7", "two"]);
        assert_eq!(texts(&[1, 2]), ["7", "one"]);
        assert!(texts(&[3]).is_empty());
    }

    #[test]
    fn a_tab_badge_carries_a_unit_word_a_tree_badge_does_not() {
        let mut s = Store::default();
        let mut d = plug();
        d.badges = vec!["tab".into()];
        let item = |text: &str| BadgeItem { pane: Some(1), text: text.into(), ..Default::default() };
        assert!(s.ingest(&d, PluginMsg::Badges { target: "tab".into(), items: vec![item("999 todo")] }).is_ok(), "8 cells fits a tab badge");
        let e = s.ingest(&d, PluginMsg::Badges { target: "tab".into(), items: vec![item("9999 todo")] }).unwrap_err();
        assert!(e.contains("wider than 8"), "{e}");
    }

    #[test]
    fn segments_are_right_aligned_and_the_rightmost_drop_first() {
        assert_eq!(layout(&[5, 3], 0, 20), vec![9, 17], "5 + 3 + 3 cells between, ending at 20");
        assert_eq!(layout(&[5, 3], 10, 20), vec![15], "no room for both: the right one goes");
        assert_eq!(layout(&[5, 3], 16, 20), Vec::<u16>::new());
        assert_eq!(layout(&[], 0, 20), Vec::<u16>::new());
        assert_eq!(layout(&[4], 30, 20), Vec::<u16>::new(), "an end before the start");
    }

    #[test]
    fn a_built_in_fits_its_text_under_the_cap() {
        assert_eq!(fit("feature/a-very-long-branch-name", 24), "feature/a-very-long-bra\u{2026}");
        assert_eq!(cells(&fit("界".repeat(30).as_str(), 24)), 23);
        assert_eq!(fit("main", 24), "main");
    }

    fn preview(req: u64, lines: Vec<Vec<(&str, &str)>>) -> PluginMsg {
        let lines = lines.into_iter().map(|l| l.into_iter().map(|(t, r)| crate::proto::TextSpan { text: t.into(), role: r.into() }).collect()).collect();
        PluginMsg::Preview { req, lines, cut: false, modes: vec![], mode: String::new() }
    }

    #[test]
    fn a_preview_answer_keeps_to_its_caps_and_roles_and_goes_with_its_source() {
        let mut s = Store::default();
        let d = plug();
        let refused = |s: &mut Store, d: &Decl, m: PluginMsg, why: &str| {
            let e = s.ingest(d, m).unwrap_err();
            assert!(e.contains(why), "{e} lacks {why}");
        };
        refused(&mut s, &Decl { previews: vec![], ..plug() }, preview(1, vec![vec![("x", "")]]), "previews are not in its manifest");
        refused(&mut s, &d, preview(1, vec![vec![("x", "pink")]]), "not a role");
        refused(&mut s, &d, preview(1, vec![vec![]; PREVIEW_LINES + 1]), "more than 2000");
        refused(&mut s, &d, preview(1, vec![vec![("a", ""), (&"b".repeat(LINE_CELLS), "")]]), "line 1 is wider than 512");
        refused(&mut s, &d, preview(1, vec![vec![], vec![(&"界".repeat(257), "")]]), "line 2 is wider than 512");
        assert!(s.answers.is_empty(), "nothing refused is kept");

        let wide = "界".repeat(LINE_CELLS / 2);
        assert!(s.ingest(&d, preview(3, vec![vec![(wide.as_str(), "")]; PREVIEW_LINES])).is_ok(), "at the caps");
        s.ingest(&d, preview(4, vec![vec![("a\u{202e}\u{1b}b", "ok"), ("c", "")]])).unwrap();
        assert!(!s.apply(), "an answer is not slot data: nothing to draw until the tree takes it");
        assert_eq!(s.answers[1], Answer { source: "p".into(), req: 4, lines: vec![vec![("ab".into(), Role::Ok), ("c".into(), Role::Text)]], cut: false, modes: vec![], mode: String::new() });
        s.clear("p");
        assert!(s.answers.is_empty(), "a stopped source's answers go with it");
    }

    fn panel(id: &str, kind: &str, items: Vec<crate::proto::PanelItem>) -> PluginMsg {
        PluginMsg::Panel { id: id.into(), kind: kind.into(), title: String::new(), items, lines: vec![] }
    }

    fn pitem(id: &str, children: Vec<crate::proto::PanelItem>) -> crate::proto::PanelItem {
        crate::proto::PanelItem { id: id.into(), label: id.into(), children, ..Default::default() }
    }

    #[test]
    fn a_panel_keeps_to_its_manifest_caps_and_roles_and_the_latest_wins() {
        let mut s = Store::default();
        let d = plug();
        let refused = |s: &mut Store, m: PluginMsg, why: &str| {
            let e = s.ingest(&d, m).unwrap_err();
            assert!(e.contains(why), "{e} lacks {why}");
        };
        refused(&mut s, panel("q", "list", vec![]), "panel \"q\" is not in its manifest");
        refused(&mut s, panel("p", "grid", vec![]), "not a panel kind");
        refused(&mut s, panel(&"p".repeat(65), "list", vec![]), "longer than 64");
        refused(&mut s, PluginMsg::Panel { id: "p".into(), kind: "list".into(), title: "界".repeat(33), items: vec![], lines: vec![] }, "title is wider than 64");
        refused(&mut s, panel("p", "list", vec![crate::proto::PanelItem { role: "pink".into(), ..pitem("a", vec![]) }]), "not a role");
        refused(&mut s, panel("p", "list", vec![pitem("", vec![])]), "empty or longer");
        refused(&mut s, panel("p", "list", vec![pitem(&"i".repeat(1025), vec![])]), "empty or longer");
        refused(&mut s, panel("p", "list", vec![crate::proto::PanelItem { detail: "d".repeat(LINE_CELLS), ..pitem("a", vec![]) }]), "wider than 512");
        let many = (0..ITEMS).map(|i| pitem(&format!("{i}"), vec![])).collect();
        refused(&mut s, panel("p", "tree", vec![pitem("top", many)]), "5001 panel items, more than 5000");
        let deep = (0..=PANEL_DEPTH).fold(Vec::new(), |acc, i| vec![pitem(&format!("{i}"), acc)]);
        refused(&mut s, panel("p", "tree", deep), "9 deep, more than 8");
        let line = |t: &str, r: &str| vec![crate::proto::TextSpan { text: t.into(), role: r.into() }];
        refused(&mut s, PluginMsg::Panel { id: "p".into(), kind: "detail".into(), title: String::new(), items: vec![], lines: vec![line("x", ""); PREVIEW_LINES + 1] }, "more than 2000");
        refused(&mut s, PluginMsg::Panel { id: "p".into(), kind: "detail".into(), title: String::new(), items: vec![], lines: vec![line(&"x".repeat(513), "")] }, "panel line 1 is wider");
        refused(&mut s, PluginMsg::Panel { id: "p".into(), kind: "detail".into(), title: String::new(), items: vec![], lines: vec![line("x", "red")] }, "not a role");
        assert!(!s.apply(), "nothing refused was queued");

        let deep = (0..PANEL_DEPTH).fold(Vec::new(), |acc, i| vec![pitem(&format!("{i}"), acc)]);
        assert!(s.ingest(&d, panel("p", "tree", deep)).is_ok(), "8 deep is allowed");
        let full = (0..ITEMS - 1).map(|i| pitem(&format!("{i}"), vec![])).collect();
        assert!(s.ingest(&d, panel("p", "tree", vec![pitem("top", full)])).is_ok(), "5000 at any depth is allowed");
        s.ingest(&d, panel("p", "list", vec![crate::proto::PanelItem { label: "a\u{202e}\u{1b}b".into(), role: "warn".into(), ..pitem("1", vec![]) }])).unwrap();
        assert!(s.apply());
        let got = s.panel(&(true, "p".into()), "p").unwrap();
        assert_eq!((got.kind, got.items.len(), got.items[0].label.as_str(), got.items[0].role), (crate::panel::Kind::List, 1, "ab", Role::Warn), "the last one said, cleaned");
        assert!(s.panel(&(false, "p".into()), "p").is_none(), "a built-in of the same name is another source");
        assert!(s.clear("p"));
        assert!(s.panel(&(true, "p".into()), "p").is_none(), "a stopped source's panel goes");
    }
}
