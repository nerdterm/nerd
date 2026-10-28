//! Render snapshots of the tree, the Changes view, the tabs and the top row, as golden text with their styles.

use super::*;
use ratatui::buffer::Buffer;

/// Each row's text, then each run of one style: a change of a cell or a colour shows in the diff.
pub(crate) fn dump(buf: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.y..area.bottom() {
        out.extend((area.x..area.right()).map(|x| buf[(x, y)].symbol().to_string()));
        out.push('\n');
    }
    for y in area.y..area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let key = |c: &ratatui::buffer::Cell| (c.fg, c.bg, c.modifier);
            let k = key(&buf[(x, y)]);
            let start = x;
            while x < area.right() && key(&buf[(x, y)]) == k {
                x += 1;
            }
            out.push_str(&format!("{y}:{start}-{} {:?} {:?} {:?}\n", x - 1, k.0, k.1, k.2));
        }
    }
    out
}

pub(crate) fn one_space() -> App {
    let mut app = App::new(Config::default());
    app.spaces.push(Space {
        name: Some("s".into()),
        cwd: PathBuf::from("/"),
        tabs: vec![
            Tab { name: None, layout: Layout::new(1), panes: HashMap::new(), isolation_new: None },
            Tab { name: Some("logs".into()), layout: Layout::new(2), panes: HashMap::new(), isolation_new: None },
        ],
        active_tab: 0,
        isolation_new: None,
    });
    app.screen = Rect::new(0, 0, 120, 30);
    app
}

/// The top row of a whole frame.
pub(crate) fn top_row(app: &mut App, w: u16) -> String {
    // A test run from a nerd pane inherits its NERD_INSTANCE: the row names only a server the test sets.
    if crate::session::tests::INSTANCE.with_borrow(Option::is_none) {
        crate::session::tests::INSTANCE.set(Some(String::new()));
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 12)).unwrap();
    term.draw(|f| {
        crate::ui::frame(f, app);
    })
    .unwrap();
    dump(term.backend().buffer(), Rect::new(0, 0, w, 1))
}

/// A folder whose rows name nothing that changes between runs.
pub(crate) fn fixed_tree(tag: &str) -> (PathBuf, crate::tree::Tree) {
    // The `../` row shows the parent's tail, so the tail is fixed and long.
    let dir = std::env::temp_dir().join(format!("nerd-golden-snapshot-of-the-tree-{tag}")).join("proj");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(dir.join("README.md"), "# proj\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "// TODO one\n".repeat(40)).unwrap();
    std::fs::write(dir.join("src/deep/a.txt"), "a").unwrap();
    let dir = dir.canonicalize().unwrap();
    let mut t = crate::tree::Tree::new(dir.clone());
    t.preview_on = false;
    (dir, t)
}

pub(crate) fn tree_text(t: &crate::tree::Tree, dir: &std::path::Path) -> String {
    let area = Rect::new(0, 0, 44, 12);
    let mut buf = Buffer::empty(area);
    let mut v = crate::tree::TreeView::default();
    t.draw_pair(&mut v, &mut buf, area, &crate::render::Theme::dark(), 40);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    dump(&buf, area)
}

pub(crate) fn marks(dir: &std::path::Path) -> HashMap<PathBuf, char> {
    [("src/main.rs", 'M'), ("src/deep/a.txt", 'N'), ("README.md", 'D')].into_iter().map(|(p, m)| (dir.join(p), m)).collect()
}

#[test]
fn golden_tree_and_changes_as_master_drew_them() {
    let (dir, mut t) = fixed_tree("files");
    t.set_git_marks(marks(&dir));
    t.select_path(&dir.join("src/deep/a.txt"));
    crate::hook::tests::golden("ui/tree.txt", &tree_text(&t, &dir));

    let (dir, mut t) = fixed_tree("changes");
    t.set_git_marks(marks(&dir));
    t.set_view(crate::tree::View::Changes);
    crate::hook::tests::golden("ui/changes.txt", &tree_text(&t, &dir));
}

#[test]
fn golden_tabs_as_master_drew_them() {
    use crate::chrome::{draw_tabs, TabItem};
    let theme = crate::render::Theme::dark();
    let tabs = |names: &[&str]| names.iter().enumerate().map(|(i, n)| TabItem { index: i, name: n.to_string(), active: i == 0, ..TabItem::default() }).collect::<Vec<_>>();
    let mut out = String::new();
    for (w, keys, names) in [(40u16, false, &["main", "logs"][..]), (40, true, &["main", "logs"]), (14, false, &["main", "a long name"]), (30, false, &["1", "2", "3", "журнал"])] {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::empty(area);
        let (rects, plus) = draw_tabs(&mut buf, area, &tabs(names), keys, &theme);
        out += &format!("{w} keys={keys} {rects:?} {plus:?}\n");
        out += &dump(&buf, area);
    }
    crate::hook::tests::golden("ui/tabs.txt", &out);
}

#[test]
fn golden_top_row_as_master_drew_it() {
    type Case = (&'static str, u16, fn(&mut App));
    let cases: [Case; 8] = [
        ("sidebar", 120, |_| {}),
        ("rail", 120, |a| a.sidebar = false),
        ("notice", 120, |a| a.notice = Some("config failed to save: disk full".into())),
        ("toast", 120, |a| a.toast = Some("copied".into())),
        ("machine", 120, |a| a.caps.machine = Some("studio".into())),
        ("notice and machine", 120, |a| {
            a.caps.machine = Some("studio".into());
            a.notice = Some("not renamed: taken".into());
        }),
        ("narrow machine", 40, |a| {
            a.sidebar = false;
            a.caps.machine = Some("a-machine-with-a-long-name".into());
        }),
        ("tabs on the bar", 120, |a| a.focus = Focus::Tabs),
    ];
    let mut out = String::new();
    for (tag, w, set) in cases {
        let mut app = one_space();
        set(&mut app);
        out += &format!("== {tag}\n{}", top_row(&mut app, w));
    }
    crate::hook::tests::golden("ui/top.txt", &out);
}

#[test]
fn golden_search_window() {
    use crate::find::Group;
    crate::session::tests::with_temp_state_dir("golden-search", || {
        let mut app = one_space();
        for (id, state) in [(1, AgentState::Working), (2, AgentState::Blocked)] {
            let p = agent_pane(&app, id, state, 0);
            app.spaces[0].tabs[id - 1].panes.insert(id, p);
        }
        let Overlay::Find(mut f) = app.run_action(Action::SearchAll, &MenuCtx::Global) else { panic!("Search opens") };
        let screen = Rect::new(0, 0, 100, 30);
        let mut out = String::new();
        let mut shot = |tag: &str, f: &crate::find::Find| {
            let mut buf = Buffer::empty(screen);
            f.draw(&mut buf, screen, &crate::render::Theme::dark());
            out += &format!("== {tag}\n{}", dump(&buf, screen));
        };
        shot("all, nothing typed: nothing listed", &f);
        f.set_tab(Some(Group::Panes));
        shot("panes with their chips", &f);
        f.chip = crate::find::Chip::Working;
        f.set_tab(Some(Group::Panes));
        shot("the working chip lists its panes", &f);
        f.chip = crate::find::Chip::All;
        f.set_tab(None);
        f.paste("split");
        shot("commands on All", &f);
        f.query.clear();
        f.set_tab(Some(Group::Settings));
        f.paste("wrap");
        shot("a setting and its value", &f);
        crate::hook::tests::golden("ui/search.txt", &out);
    });
}

/// The menu button, cell by cell, in the open sidebar and in the rail, quiet and with its badge: any change to
/// where it stands or how the badge meets the border lines shows here.
#[test]
fn golden_the_menu_button_against_the_border_lines() {
    let mut out = String::new();
    let theme = crate::render::Theme::dark();
    for (tag, sidebar, marked) in [("open, quiet", true, false), ("open, badge", true, true), ("rail, quiet", false, false), ("rail, badge", false, true)] {
        let mut app = one_space();
        app.sidebar = sidebar;
        app.update_available = marked;
        if crate::session::tests::INSTANCE.with_borrow(Option::is_none) {
            crate::session::tests::INSTANCE.set(Some(String::new()));
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 12)).unwrap();
        term.draw(|f| {
            crate::ui::frame(f, &mut app);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // the border the badge meets: the open sidebar's right edge at 33, the rail's edges at 0 and 4
        let (strokes, edges): (u16, &[u16]) = if sidebar { (32, &[33]) } else { (2, &[0, 4]) };
        for &x in edges {
            assert!(["\u{2510}", "\u{250c}"].contains(&buf[(x, 2)].symbol()), "{tag}: a border corner at {x}");
        }
        assert_eq!(buf[(strokes, 0)].symbol(), "\u{2261}", "{tag}: the strokes stay in cell {strokes}");
        if marked {
            assert_eq!((buf[(strokes, 0)].fg, buf[(strokes, 0)].bg), (theme.ground, theme.warn), "{tag}: dark strokes on the badge");
            assert_eq!((buf[(strokes - 1, 0)].symbol(), buf[(strokes - 1, 0)].fg), ("\u{2590}", theme.warn), "{tag}: the right half of the cell before");
            assert_eq!((buf[(strokes + 1, 0)].symbol(), buf[(strokes + 1, 0)].fg), ("\u{258c}", theme.warn), "{tag}: the left half of the cell after");
            if sidebar {
                assert_eq!(strokes + 1, 33, "{tag}: the badge's right half sits in the border's cell, ending on its line");
            } else {
                assert_eq!((strokes - 1, strokes + 1), (1, 3), "{tag}: centred between the rail's borders");
            }
        } else {
            assert_eq!(buf[(strokes, 0)].fg, theme.muted, "{tag}: quiet strokes");
            assert!([strokes - 1, strokes + 1].iter().all(|&x| buf[(x, 0)].symbol() == " "), "{tag}: nothing beside them");
        }
        out += &format!("== {tag}\n{}", dump(&buf, Rect::new(0, 0, 40, 3)));
        // the menu opens from one cell whether the badge shows or not: 31 in the open sidebar, the rail's edge
        let click = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: strokes, row: 0, modifiers: crossterm::event::KeyModifiers::NONE };
        app.mouse(click, Rect::new(0, 0, 120, 12));
        let Overlay::Menu(m, _) = &app.overlay else { panic!("{tag}: a click on the strokes opens the menu") };
        assert_eq!(m.rect(Rect::new(0, 0, 120, 12)).x, if sidebar { 31 } else { 0 }, "{tag}: where the menu opens");
    }
    crate::session::tests::INSTANCE.set(None);
    crate::hook::tests::golden("ui/menu-button.txt", &out);
}

/// An agent pane with no process behind it, in `state`, with `subs` subagents running.
pub(crate) fn agent_pane(app: &App, id: PaneId, state: AgentState, subs: usize) -> Pane {
    use std::os::fd::IntoRawFd;
    let fd = std::fs::File::open("/dev/null").unwrap().into_raw_fd();
    let mut activity = crate::hook::Activity::default();
    for i in 0..subs {
        let now = crate::hook::now();
        activity.subs.push(crate::hook::Sub { id: format!("s{i}"), kind: "x".into(), task: String::new(), started: now, last: now, stopped: None, calls: Vec::new(), alone: false });
    }
    let mut p = Pane {
        id,
        term: Term::new(80, 24, 100),
        pty: Pty::adopt(id, fd, None, 80, 24, app.bell.clone()).unwrap(),
        cwd: PathBuf::from("/"),
        argv: Vec::new(),
        label: None,
        agent_session: None,
        agent: Some("claude".into()),
        command: "claude".into(),
        activity,
        activity_key: [None, None],
        mouse_left_on: false,
        isolation: Default::default(),
    };
    p.term.set_hook_state(Some(state));
    p.term.tick_state();
    p
}

fn plugin_decl(name: &str) -> crate::slots::Decl {
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
    crate::slots::Decl { name: name.into(), builtin: false, badges: v(&["tree", "tab"]), segments: v(&["a", "b"]), commands: v(&["go"]), previews: Vec::new(), panels: Vec::new() }
}

fn segment(id: &str, text: &str, role: &str, command: Option<&str>) -> crate::proto::PluginMsg {
    crate::proto::PluginMsg::Segment { id: id.into(), text: text.into(), role: role.into(), command: command.map(Into::into) }
}

fn badges(target: &str, items: Vec<(Option<PathBuf>, Option<usize>, &str, &str)>) -> crate::proto::PluginMsg {
    let items = items.into_iter().map(|(path, pane, text, role)| crate::proto::BadgeItem { path, pane, text: text.into(), role: role.into() }).collect();
    crate::proto::PluginMsg::Badges { target: target.into(), items }
}

#[test]
fn golden_tree_with_badges_after_the_mark_and_before_the_size() {
    let mut store = crate::slots::Store::default();
    let mut out = String::new();
    for (tag, view) in [("files", crate::tree::View::Files), ("changes", crate::tree::View::Changes)] {
        let (dir, mut t) = fixed_tree(&format!("badges-{tag}"));
        t.set_git_marks(marks(&dir));
        t.select_path(&dir.join("src/deep/a.txt"));
        t.set_view(view);
        let items = vec![(Some(dir.join("src/lib.rs")), None, "40", "teal"), (Some(dir.join("src/main.rs")), None, "\u{2605}", "accent"), (Some(dir.join("src")), None, "d", "bad")];
        store.ingest(&plugin_decl("aa"), badges("tree", items)).unwrap();
        store.ingest(&plugin_decl("zz"), badges("tree", vec![(Some(dir.join("src/main.rs")), None, "界", "warn")])).unwrap();
        store.apply();
        t.badges = store.tree.clone();
        let text = tree_text(&t, &dir);
        out += &format!("== {tag}\n{text}");
        let row = |name: &str| text.lines().find(|l| l.contains(name)).unwrap().to_string();
        if view == crate::tree::View::Files {
            assert!(row("main.rs").contains("main.rs M \u{2605} 界 ") && row("main.rs").trim_end_matches(['│', ' ']).ends_with("13B"), "{}", row("main.rs"));
            assert!(row("lib.rs").contains("lib.rs 40 ") && row("lib.rs").trim_end_matches(['│', ' ']).ends_with("480B"), "exact path: {}", row("lib.rs"));
            assert!(row("▾ src").contains("src M d"), "a folder shows only its own badge: {}", row("▾ src"));
            assert!(!row("a.txt").contains("40") && !row("deep").contains("40"), "nothing is summed up into folders");
        }
    }
    crate::hook::tests::golden("ui/tree-badges.txt", &out);
}

#[test]
fn golden_tabs_with_badges_that_drop_before_any_tab_does() {
    use crate::chrome::{chip_width, draw_tabs, TabItem};
    let theme = crate::render::Theme::dark();
    let tabs = || {
        vec![
            TabItem { index: 0, name: "main".into(), active: true, badges: vec![("7".into(), theme.teal), ("9".into(), theme.warn)], ..TabItem::default() },
            TabItem { index: 1, name: "logs".into(), active: false, badges: vec![("界".into(), theme.accent)], ..TabItem::default() },
        ]
    };
    let full = tabs().iter().map(|t| chip_width(&t.name, &t.badges)).sum::<u16>() + chip_width("+", &[]);
    assert_eq!(full, 14 + 13 + 5, "badges are measured in cells, the separator with the first of them");
    let mut out = String::new();
    for (w, want) in [(40u16, "all"), (full, "all"), (full - 1, "none"), (21, "none"), (12, "a tab goes")] {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::empty(area);
        let (rects, plus) = draw_tabs(&mut buf, area, &tabs(), false, &theme);
        let row: String = (0..w).map(|x| buf[(x, 0)].symbol()).collect();
        match want {
            "all" => assert!(row.contains("main \u{b7} 7 9") && row.contains("logs \u{b7} 界"), "{w}: {row}"),
            "none" => assert!(row.starts_with("  main ▕  logs ▕") && plus.is_some(), "{w}: {row}"),
            _ => assert!(rects.len() == 1 && !row.contains('7'), "{w}: {row}"),
        }
        if want == "all" {
            assert_eq!(rects[0].0.width, 14, "the hit rect holds the badges");
            assert_eq!((buf[(9, 0)].fg, buf[(11, 0)].fg), (theme.teal, theme.warn), "each badge in its role");
        }
        out += &format!("{w} {want} {rects:?} {plus:?}\n{}", dump(&buf, area));
    }
    crate::hook::tests::golden("ui/tabs-badges.txt", &out);
}

/// The top row with an agent that needs you, two plugins' segments and the clock.
fn with_segments() -> App {
    let mut app = one_space();
    let pane = agent_pane(&app, 1, AgentState::Blocked, 2);
    app.spaces[0].tabs[0].panes.insert(1, pane);
    app.clock = "09:05".into();
    let s = &mut app.plugins.slots;
    s.ingest(&plugin_decl("zeta"), segment("a", "zeta", "ok", None)).unwrap();
    s.ingest(&plugin_decl("alpha"), segment("b", "alpha-b", "bad", Some("go"))).unwrap();
    s.ingest(&plugin_decl("alpha"), segment("a", "alpha-a", "", None)).unwrap();
    app
}

#[test]
fn the_battery_stands_before_the_clock_a_dot_apart() {
    let mut app = with_segments();
    app.battery = Some(crate::battery::Charge { percent: 46, power: crate::battery::Power::Battery, low_power: false, minutes: None, watts: None });
    let row = top_row(&mut app, 120);
    let line = row.lines().next().unwrap();
    assert!(line.ends_with("alpha-a · alpha-b · zeta · 46% · 09:05  "), "{line:?}");
    app.battery = Some(crate::battery::Charge { percent: 71, power: crate::battery::Power::Charging, low_power: true, minutes: None, watts: None });
    app.clock.clear();
    let row = top_row(&mut app, 120);
    assert!(row.lines().next().unwrap().ends_with("zeta · \u{3df} 71% L  "), "no clock: the battery ends the row, the bolt first, L for Low Power Mode last");
    app.cfg.battery = crate::config::Battery::Time;
    app.battery = Some(crate::battery::Charge { percent: 46, power: crate::battery::Power::Battery, low_power: false, minutes: Some(250), watts: None });
    app.clock = "09:05".into();
    let row = top_row(&mut app, 120);
    let line = row.lines().next().unwrap();
    assert!(line.ends_with("zeta · 4:10 · 09:05  "), "time to empty instead of the percent: {line:?}");
    let x = line[..line.find("4:10").unwrap()].chars().count() as u16;
    assert!(app.view.hits.segments.iter().any(|(r, h)| *h == crate::mouse::SegmentHit::Battery && r.x == x && r.width == 4), "a click on it switches");
    app.battery_hover = Some(std::time::Instant::now());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 12)).unwrap();
    term.draw(|f| {
        crate::ui::frame(f, &mut app);
    })
    .unwrap();
    let row = dump(term.backend().buffer(), Rect::new(0, 1, 120, 1));
    let under = row.lines().next().unwrap();
    assert!(under.trim_end().ends_with("46%, on battery, 4:10 left"), "the details under it, right-aligned to it: {under:?}");
    let mut one_row = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 1)).unwrap();
    one_row.draw(|f| {
        crate::ui::frame(f, &mut app);
    })
    .unwrap();
    let (x, y) = app.view.hits.segments.iter().find(|(_, h)| *h == crate::mouse::SegmentHit::Battery).map(|(r, _)| (r.x, r.y)).unwrap();
    let mv = |column, row| crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Moved, column, row, modifiers: crossterm::event::KeyModifiers::NONE };
    app.battery_hover = None;
    app.mouse(mv(x, y), Rect::new(0, 0, 120, 1));
    assert!(app.battery_hover.is_some(), "a move over it shows the details");
    app.mouse(mv(5, 0), Rect::new(0, 0, 120, 1));
    assert!(app.battery_hover.is_none(), "a move off it hides them");
    app.battery_hover = Some(std::time::Instant::now() - crate::battery::HOVER);
    assert!(app.tick_battery() && app.battery_hover.is_none(), "and they go by themselves: a pointer that left the window sends nothing");
}

#[test]
fn a_click_on_the_clock_adds_the_date_and_keeps_the_choice() {
    let home = crate::paths::test_dir("clock-click");
    let old = crate::paths::TEST_HOME.with(|h| h.replace(Some(home.clone())));
    let mut app = one_space();
    app.clock = "09:05".into();
    let _ = top_row(&mut app, 120);
    let (x, y) = app.view.hits.segments.iter().find(|(_, h)| *h == crate::mouse::SegmentHit::Clock).map(|(r, _)| (r.x, r.y)).unwrap();
    let click = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE };
    app.mouse(click, Rect::new(0, 0, 120, 12));
    let saved = std::fs::read_to_string(crate::paths::config_path()).unwrap_or_default();
    crate::paths::TEST_HOME.with(|h| h.replace(old));
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(app.cfg.clock, crate::config::Clock::Date);
    assert!(saved.contains("\"clock\": \"date\""), "kept in config.json: {saved}");
}

#[test]
fn a_short_row_keeps_the_time_of_a_dated_clock() {
    let mut app = with_segments();
    app.clock = "Sun 4. 10.  09:05".into();
    let row = top_row(&mut app, 120);
    assert!(row.lines().next().unwrap().ends_with("zeta · Sun 4. 10.  09:05  "), "room for all: {row}");
    let line = top_row(&mut app, 32).lines().next().unwrap().to_string();
    assert!(line.trim_end().ends_with("09:05") && !line.contains("Sun"), "short: the time alone: {line:?}");
    assert!(app.view.hits.segments.iter().any(|(_, h)| *h == crate::mouse::SegmentHit::Clock), "and still a click back");
}

#[test]
fn golden_top_row_with_segments_in_the_free_gap() {
    let mut out = String::new();
    let mut app = with_segments();
    let row = top_row(&mut app, 120);
    out += &format!("== all\n{row}");
    let line = row.lines().next().unwrap();
    assert!(line.ends_with("alpha-a · alpha-b · zeta · 09:05  "), "plugins by name, then the clock, right-aligned: {line:?}");
    assert!(line.contains("  1 claude ▕  logs ▕  +") && !line.contains("need"), "nothing of nerd's own but the clock: {line:?}");
    let hits: Vec<_> = app.view.hits.segments.iter().filter(|(_, h)| *h != crate::mouse::SegmentHit::Clock).cloned().collect();
    assert_eq!(hits.len(), 1, "only the one with a command, besides the clock");
    assert_eq!(hits[0].1, crate::mouse::SegmentHit::Command("alpha".into(), "go".into()));
    let x = |s: &str| line[..line.find(s).unwrap()].chars().count() as u16;
    assert_eq!((hits[0].0.x, hits[0].0.width), (x("alpha-b"), 7));

    // Too narrow for all: the plugins' go first, the clock stays.
    let row = top_row(&mut app, 32);
    out += &format!("== narrow\n{row}");
    let line = row.lines().next().unwrap();
    assert!(line.contains("  1 ▕  logs ▕  + ") && line.trim_end().ends_with("09:05") && !line.contains("alpha"), "{line:?}");
    assert!(app.view.hits.segments.iter().all(|(_, h)| *h == crate::mouse::SegmentHit::Clock), "no plugin's click left, only the clock's");

    // A notice is a card over the panes now: the top row keeps its segments and their clicks.
    app.notice = Some("config failed to save: disk full".into());
    let row = top_row(&mut app, 120);
    out += &format!("== notice\n{row}");
    let line = row.lines().next().unwrap();
    assert!(!line.contains("disk full") && line.contains("alpha-a · alpha-b · zeta · 09:05"), "{line:?}");
    assert_eq!(app.view.hits.segments.len(), 2, "the segment with a command and the clock are still clicked");

    app.notice = None;
    app.caps.machine = Some("studio".into());
    let row = top_row(&mut app, 120);
    out += &format!("== machine\n{row}");
    assert!(row.lines().next().unwrap().ends_with("zeta · 09:05 @ studio  "), "{row}");
    crate::hook::tests::golden("ui/top-segments.txt", &out);
}

/// A tree over `files` in a folder of its own, `area` wide, with these badges and marks.
fn small_tree(tag: &str, files: &[&str], marks: &[(&str, char)], marked: &[(&str, &str)], flat: bool, width: u16) -> (Buffer, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("nerd-snap-small-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in files {
        std::fs::create_dir_all(dir.join(f).parent().unwrap()).unwrap();
        std::fs::write(dir.join(f), "x").unwrap();
    }
    let dir = dir.canonicalize().unwrap();
    let mut t = crate::tree::Tree::new(dir.clone());
    t.preview_on = false;
    t.set_git_marks(marks.iter().map(|(p, m)| (dir.join(p), *m)).collect());
    if flat {
        t.set_view(crate::tree::View::Changes);
        t.toggle_changes_shape();
    }
    let mut store = crate::slots::Store::default();
    store.ingest(&plugin_decl("aa"), badges("tree", badges_at(&dir, marked))).unwrap();
    store.apply();
    t.badges = store.tree.clone();
    let area = Rect::new(0, 0, width, 8);
    let mut buf = Buffer::empty(area);
    t.draw_pair(&mut crate::tree::TreeView::default(), &mut buf, area, &crate::render::Theme::dark(), width - 4);
    let rows = (0..8).map(|y| (0..width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    (buf, rows)
}

fn badges_at<'a>(dir: &std::path::Path, b: &[(&str, &'a str)]) -> Vec<(Option<PathBuf>, Option<usize>, &'a str, &'a str)> {
    b.iter().map(|(p, t)| (Some(dir.join(p)), None, *t, "teal")).collect()
}

#[test]
fn a_badge_follows_a_wide_name_by_its_cells() {
    let theme = crate::render::Theme::dark();
    let (buf, rows) = small_tree("cjk", &["a.rs", "日本語.rs"], &[("日本語.rs", 'M')], &[("日本語.rs", "7")], false, 40);
    let y = rows.iter().position(|r| r.contains("日")).unwrap_or_else(|| panic!("{rows:#?}")) as u16;
    // The frame, its padding and the arrow column: the name starts at x = 4 and takes 9 cells.
    assert_eq!((buf[(14, y)].symbol(), buf[(14, y)].fg), ("M", theme.warn), "{:?}", rows[y as usize]);
    assert_eq!((buf[(16, y)].symbol(), buf[(16, y)].fg), ("7", theme.teal), "{:?}", rows[y as usize]);
    assert!(rows[y as usize].trim_end_matches(['│', ' ']).ends_with("1B"));
}

#[test]
fn a_long_name_in_changes_is_cut_so_its_badge_still_shows() {
    let long = "a-rather-long-folder-name/and-a-long-file-name.rs";
    let (_, rows) = small_tree("elide", &[long], &[(long, 'N')], &[(long, "12")], true, 30);
    let row = rows.iter().find(|r| r.contains("12")).unwrap_or_else(|| panic!("the badge was cut off: {rows:?}"));
    assert!(row.contains("…") && row.contains("-name.rs 12"), "{row:?}");
}

#[test]
fn a_tab_shows_the_badge_of_its_focused_pane_first() {
    let mut app = one_space();
    let d = plugin_decl("p");
    let tab = &mut app.spaces[0].tabs[0];
    tab.layout = Layout::new(1);
    tab.layout.split(Dir::Horizontal, 2);
    for id in [1, 2] {
        let p = agent_pane(&app, id, AgentState::Idle, 0);
        app.spaces[0].tabs[0].panes.insert(id, p);
    }
    app.plugins.slots.ingest(&d, badges("tab", vec![(None, Some(1), "one", ""), (None, Some(2), "two", "")])).unwrap();
    for (focus, want) in [(2, "two"), (1, "one")] {
        app.spaces[0].tabs[0].layout.focus(focus);
        let row = top_row(&mut app, 120);
        assert!(row.lines().next().unwrap().contains(&format!("  1 claude \u{b7} {want} ")), "focused {focus}: {row}");
    }
}

#[test]
fn a_source_s_refusals_are_logged_once() {
    let log = crate::session::tests::with_temp_state_dir("slots-warn-once", || {
        let mut s = crate::slots::Store::default();
        for _ in 0..3 {
            assert!(!s.take(&plugin_decl("p"), segment("undeclared", "x", "", None)));
            assert!(!s.take(&plugin_decl("p"), segment("a", "x", "pink", None)));
        }
        s.clear("p");
        assert!(!s.take(&plugin_decl("p"), segment("undeclared", "x", "", None)));
        std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default()
    });
    assert_eq!(log.matches("plugin p: Segment refused").count(), 2, "once, and once again after it stopped: {log}");
}

#[test]
fn a_size_that_would_run_into_a_badge_is_left_out() {
    for width in 14..34 {
        let (_, rows) = small_tree(&format!("collide-{width}"), &["a.rs", "abcdef.rs"], &[], &[("abcdef.rs", "1234")], false, width);
        let row = rows.iter().find(|r| r.contains("abcdef")).unwrap();
        if let Some(size) = row.find("1B") {
            let badge = row.find("1234").unwrap_or_else(|| panic!("{width}: the size is shown and the badge is not: {row:?}"));
            assert!(badge + 4 < size, "{width}: {row:?}");
        }
    }
}

/// A folder with one file of each kind the preview shows, under a name whose tail never changes.
pub(crate) fn preview_files(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nerd-golden-snapshot-of-the-preview-{tag}")).join("proj");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "plain words\n\tindented by a tab\n\nlast line\n").unwrap();
    std::fs::write(dir.join("main.rs"), "// a comment\nfn main() {\n    let n = 42;\n    println!(\"hi {n}\");\n}\n").unwrap();
    std::fs::write(dir.join("data.json"), "{\"name\": \"nerd\", \"tags\": [\"a\", \"b\"], \"n\": 1.5e3, \"ok\": true, \"none\": null, \"empty\": {}, \"nested\": {\"deep\": [1, {\"x\": \"y\"}]}}\n").unwrap();
    std::fs::write(dir.join("README.md"), "# Title\n\nSome *text* with a [link](https://example.com) and `code`.\n\n## List\n\n- one\n  - two\n1. first\n\n> quoted\n\n```rust\nfn x() {}\n```\n\n---\n").unwrap();
    std::fs::write(dir.join("data.csv"), "name,size,note\na.rs,12,\"has, comma\"\nb.rs,7,\"two\nlines\"\nc.rs\n").unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(640u32.to_be_bytes());
    png.extend(480u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    std::fs::write(dir.join("pic.png"), png).unwrap();
    dir.canonicalize().unwrap()
}

/// The preview half of a tree with `file` selected, as the loop would draw it once loaded.
pub(crate) fn preview_text(t: &mut crate::tree::Tree, file: &std::path::Path) -> String {
    t.select_path(file);
    let mut v = crate::tree::TreeView::default();
    t.settle(&mut v);
    let area = Rect::new(0, 0, 100, 16);
    let mut buf = Buffer::empty(area);
    let pair = t.draw_pair(&mut v, &mut buf, area, &crate::render::Theme::dark(), 30);
    dump(&buf, pair.preview.unwrap())
}

#[test]
fn golden_previews_as_master_drew_them() {
    let dir = preview_files("master");
    let mut t = crate::tree::Tree::new(dir.clone());
    let mut out = String::new();
    for f in ["notes.txt", "main.rs", "data.json", "README.md", "data.csv", "pic.png"] {
        if f == "pic.png" {
            t.set_image_view(crate::preview::ImageView::Off);
        }
        out += &format!("== {f}\n{}", preview_text(&mut t, &dir.join(f)));
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    crate::hook::tests::golden("ui/previews.txt", &out);
}

#[test]
fn golden_previews_of_the_public_built_ins() {
    let dir = preview_files("builtins");
    let mut t = crate::tree::Tree::new(dir.clone());
    let mut store = crate::slots::Store::default();
    let mut out = String::new();
    for f in ["data.csv", "data.json", "README.md"] {
        let path = dir.join(f);
        t.select_path(&path);
        let mut v = crate::tree::TreeView::default();
        t.settle(&mut v);
        let m = crate::previews::answer(1, &path, 61).unwrap();
        store.take(&crate::slots::Decl::builtin("previews"), m);
        let a = store.answers.pop().unwrap();
        let kind = crate::previews::kind(&crate::previews::extension(&path).unwrap());
        assert!(t.set_preview(&mut v, &t.wanted().unwrap(), crate::preview::Preview::styled(&path, a.lines, a.cut, kind, a.modes, a.mode)));
        let area = Rect::new(0, 0, 100, 16);
        let mut buf = Buffer::empty(area);
        let pair = t.draw_pair(&mut v, &mut buf, area, &crate::render::Theme::dark(), 30);
        out += &format!("== {f}\n{}", dump(&buf, pair.preview.unwrap()));
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    let rows: Vec<&str> = out.lines().collect();
    assert!(rows.iter().any(|r| r.starts_with("name │ size │ note")) && rows.iter().any(|r| r.starts_with("b.rs │ 7    │ two↵lines")), "{out}");
    assert!(rows.iter().any(|r| r.starts_with("  \"tags\": [")) && rows.iter().any(|r| r.starts_with("Title ")), "{out}");
    crate::hook::tests::golden("ui/previews-builtin.txt", &out);
}

/// Unix seconds of a local wall-clock time, so the history reads the same in every time zone.
fn local_secs(mon: i32, mday: i32, hour: i32, min: i32) -> u64 {
    // SAFETY: tm is plain data; mktime reads it and normalises it in place.
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    (tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_isdst) = (125, mon - 1, mday, hour, min, -1);
    (unsafe { nix::libc::mktime(&mut tm) }) as u64
}

#[test]
fn golden_session_history_as_master_drew_it() {
    use crate::clients::{ClientRecord, End, Sessions};
    use crossterm::event::{KeyCode, KeyEvent};
    let rec = |id, since, host: Option<&str>, program: Option<&str>, ended| ClientRecord { id, since, host: host.map(Into::into), program: program.map(Into::into), ended };
    let t = |d, h, m| local_secs(3, d, h, m);
    let list = vec![
        rec(6, t(14, 9, 30), Some("this-host"), Some("nerd"), None),
        rec(5, t(14, 8, 5), Some("this-host"), Some("nerd"), Some((t(14, 9, 30), End::TakenOver))),
        rec(4, t(13, 22, 10), Some("MacBook-Air.local"), Some("sshd-session"), Some((t(14, 8, 5), End::Closed))),
        rec(3, t(13, 18, 0), Some("this-host"), Some("nerd"), Some((t(13, 18, 40), End::Detached))),
        rec(2, t(12, 7, 45), None, None, Some((t(13, 18, 0), End::TakenOver))),
        rec(1, t(11, 23, 59), Some("this-host"), Some("a-python-relay-with-a-long-name"), Some((t(12, 7, 45), End::Lost))),
    ];
    let mut more = list.clone();
    for id in 7..=16 {
        more.insert(0, rec(id, t(15, 10, id as i32), Some("studio.local"), Some("nerd"), Some((t(15, 11, 0), End::Disconnected))));
    }
    let own = Some("this-host".to_string());
    let theme = crate::render::Theme::dark();
    let mut out = String::new();
    for (tag, list, w, h, downs) in [("first", &list, 100u16, 30u16, 0), ("remote", &list, 100, 30, 2), ("seeded", &list, 100, 30, 4), ("narrow", &list, 40, 30, 5), ("scrolled", &more, 100, 40, 12)] {
        let screen = Rect::new(0, 0, w, h);
        let mut s = Sessions::new(list.clone(), own.clone());
        for _ in 0..downs {
            s.key(KeyEvent::from(KeyCode::Down), screen);
        }
        let mut buf = Buffer::empty(screen);
        let drawn = s.draw(&mut buf, screen, &theme);
        out += &format!("== {tag} {:?} {:?}\n{}", drawn.rows, drawn.close, dump(&buf, screen));
    }
    crate::hook::tests::golden("ui/sessions.txt", &out);
}

#[test]
fn golden_servers_window() {
    use crate::server_list::{tests::fixture, Machine};
    let theme = crate::render::Theme::dark();
    let mut out = String::new();
    let listed = || -> crate::server_list::ServerList {
        let mut w = fixture(true);
        w.choose("m:studio");
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter), Rect::new(0, 0, 120, 40));
        let lab = crate::proto::RemoteInstance { id: "7".into(), label: "lab".into(), running: true, spaces: 1, shown: Some(false), folders: vec!["/Users/me/lab".into()], ..Default::default() };
        w.listed("studio", Machine::Listed(vec![lab], true));
        w.choose("r:studio:7");
        w
    };
    type Case = (&'static str, u16, u16, fn() -> crate::server_list::ServerList);
    let cases: [Case; 4] = [
        ("80x24 shown elsewhere", 80, 24, || fixture(true)),
        ("120x40 shown elsewhere", 120, 40, || fixture(true)),
        ("120x40 a machine listed", 120, 40, listed),
        ("80x24 from another Mac", 80, 24, || fixture(false)),
    ];
    for (tag, w, h, make) in cases {
        let screen = Rect::new(0, 0, w, h);
        let mut win = make();
        if !tag.contains("listed") {
            win.choose("s:2");
        }
        let mut buf = Buffer::empty(screen);
        let drawn = win.draw(&mut buf, screen, &theme);
        out += &format!("== {tag} {:?} {:?}\n{}", drawn.buttons, drawn.close, dump(&buf, screen));
    }
    crate::hook::tests::golden("ui/servers.txt", &out);
}

#[test]
fn golden_the_current_server_on_the_top_row() {
    crate::session::tests::with_temp_state_dir("server-badge", || {
        type Case = (&'static str, u16, &'static str, fn(&mut App));
        let cases: [Case; 7] = [
            ("sidebar", 120, "work", |_| {}),
            ("rail", 120, "work", |a| a.sidebar = false),
            ("rail and machine", 120, "work", |a| {
                a.sidebar = false;
                a.caps.machine = Some("studio".into());
            }),
            ("sidebar, a long name cut", 120, "a-server-name-of-thirty-two-ch", |_| {}),
            ("rail, a long name cut", 120, "a-server-name-of-thirty-two-ch", |a| a.sidebar = false),
            ("number only", 120, "", |a| a.sidebar = false),
            ("narrow, the machine kept", 40, "work", |a| {
                a.sidebar = false;
                a.caps.machine = Some("a-machine-with-a-long-name".into());
            }),
        ];
        let mut out = String::new();
        crate::session::tests::INSTANCE.set(Some("7".into()));
        std::fs::create_dir_all(crate::session::instances_dir().join("7")).unwrap();
        for (tag, w, name, set) in cases {
            crate::session::set_name("7", name).unwrap();
            let mut app = one_space();
            set(&mut app);
            out += &format!("== {tag}\n{}", top_row(&mut app, w));
        }
        crate::session::tests::INSTANCE.set(None);
        crate::hook::tests::golden("ui/server-badge.txt", &out);
    });
}
