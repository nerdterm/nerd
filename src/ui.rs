//! Frame assembly: sidebar, tabs, panes. Rebuilds the click registry every frame since
//! ratatui doesn't remember widgets between frames.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::{App, ViewState};
use std::path::PathBuf;

use crate::chrome::TabItem;
use crate::mouse::PaneHit;

/// The top bar: nerd's name and menu over the sidebar, tabs over the panes, then an empty row so the
/// chips don't sit on the frames. Every column starts under it.
const TOP_H: u16 = 2;
/// Between every two columns.
const SIDE_GAP: u16 = 1;
/// From a frame's outer edge to the text inside it: the border cell and one of padding.
pub(crate) const TEXT_INSET: u16 = 2;
/// The menu button, expanded or collapsed.
const MENU: &str = "\u{2261}";
macro_rules! wordmarks {
    ($($h:literal),*) => {
        [$(($h, include_bytes!(concat!("../brand/nerd-wordmark-dark-", $h, ".png")).as_slice(),
            include_bytes!(concat!("../brand/nerd-wordmark-light-", $h, ".png")).as_slice())),*]
    };
}
/// The logo's wordmark, one row high, where the terminal draws images; letters at about 80% of the height.
/// (row height in device pixels, dark, light), one per height scripts/brand.mjs renders.
static LOGOS: [(u16, &[u8], &[u8]); 9] = wordmarks!(16, 20, 24, 28, 32, 36, 40, 48, 64);
const LOGO_ID: u32 = 2;
/// Cells the wordmark takes at one row high (its canvas is 4.5:1, a cell about 1:2).
const LOGO_COLS: u16 = 9;

/// What a frame asks for beyond its cells; the loop sends it right after the frame, in the same turn.
#[derive(Default)]
pub struct Effects {
    /// Bytes for the screen client, outside the frame: Kitty images.
    pub screen: Vec<u8>,
    /// Text to put on the clipboard.
    pub copy: Option<String>,
    pub toast: Option<String>,
}

/// Where each part of the screen goes. The loop sizes the panes from it before drawing, so a frame never resizes a process.
pub struct Areas {
    pub top: Rect,
    pub body: Rect,
    /// The tree takes the screen: no sidebar, and no panes.
    pub zoom: bool,
    pub sidebar: Option<Rect>,
    pub rail: Option<Rect>,
    /// The tree's half of the pair, clamped to what the window leaves.
    pub tree_w: u16,
    /// The frame holding the tree and its preview, when the tree is open and fits.
    pub pair: Option<Rect>,
    /// What is left for the panes, and for the tabs above them.
    pub panes: Rect,
}

pub fn areas(app: &App, full: Rect) -> Areas {
    let top = Rect { height: TOP_H.min(full.height), ..full };
    let body = Rect { y: full.y + top.height, height: full.height - top.height, ..full };
    // A zoom left over from before the preview was switched off must not hide the sidebar.
    let zoom = app.tree_zoom && app.cfg.preview;
    let show_sidebar = app.sidebar && !zoom && full.width > app.cfg.sidebar_width + 24;
    // A sidebar with no room collapses to its rail: navigation never vanishes.
    let show_rail = !show_sidebar && !zoom && full.width > crate::sidebar::RAIL_W + 24;
    let beside = |w: u16| Rect { x: full.x + w + SIDE_GAP, width: full.width.saturating_sub(w + SIDE_GAP), ..body };
    let (sidebar, rail, right) = if show_sidebar {
        let w = app.cfg.sidebar_width;
        (Some(Rect { width: w, ..body }), None, beside(w))
    } else if show_rail {
        let w = crate::sidebar::RAIL_W;
        (None, Some(Rect { width: w, ..body }), beside(w))
    } else {
        (None, None, body)
    };
    // One frame holds the tree and its preview: the tree half first, the preview after it.
    let (tw, pw) = (app.cfg.tree_width, app.cfg.preview_width);
    // A tree half dragged wide in full screen must not swallow a narrow window when it leaves it.
    let tw = tw.min(right.width.saturating_sub(24)).max(20);
    // zoomed: the pair takes the whole area; panes get a zero one, so no geometry
    let (pair, panes) = match app.tree {
        Some(_) if right.width > tw + 24 => {
            // tree + seam + preview, plus the frame's borders and its column of padding.
            let pair_w = if zoom {
                right.width
            } else if app.cfg.preview && right.width > tw + pw + 24 {
                tw + pw + 7
            } else {
                tw + 4
            };
            let pair = Rect { x: right.right() - pair_w, width: pair_w, ..right };
            let panes = if zoom { Rect { width: 0, ..right } } else { Rect { width: right.width.saturating_sub(pair_w + SIDE_GAP), ..right } };
            (Some(pair), panes)
        }
        _ => (None, right),
    };
    Areas { top, body, zoom, sidebar, rail, tree_w: tw, pair, panes }
}

/// Places, replaces or removes the logo image so it stands at `at` (None: nowhere) in the current theme;
/// returns the bytes that do it.
fn place_logo(app: &App, view: &mut ViewState, at: Option<(u16, u16)>) -> Vec<u8> {
    let dark = app.theme_dark;
    let (h, dark_png, light_png) = *logo_for(app.caps.cell_px);
    let want = at.map(|(x, y)| (x, y, dark, h));
    if want == view.logo_at {
        return Vec::new();
    }
    view.logo_at = want;
    match want {
        Some((x, y, dark, _)) => {
            let png = if dark { dark_png } else { light_png };
            crate::preview::png_bytes(png, x, y, &format!("c={LOGO_COLS},r=1"), LOGO_ID)
        }
        None => crate::preview::clear_image_bytes(LOGO_ID),
    }
}

/// The wordmark for rows `cell_px` device pixels high: the shortest at least that tall, as terminals shrink
/// images without smoothing; the tallest beyond them, and 32 when the client did not say.
fn logo_for(cell_px: u16) -> &'static (u16, &'static [u8], &'static [u8]) {
    let want = if cell_px == 0 { 32 } else { cell_px };
    LOGOS.iter().find(|l| l.0 >= want).unwrap_or(&LOGOS[LOGOS.len() - 1])
}

/// The menu button: quiet strokes, or a filled badge while the menu holds a mark (a new build).
/// Thin strokes carry colour badly, so the marked one is filled instead.
fn menu_marked(app: &App) -> bool {
    matches!(app.global_menu((0, 0)), crate::app::Overlay::Menu(m, _) if m.items.iter().any(|i| i.mark))
}

/// Draws the strokes at `x`; the badge adds half a cell either side (▐ ≡ ▌), so the strokes never move
/// and its edges fall mid-cell, where border lines are drawn. Returns the cells it covers.
fn draw_menu(buf: &mut Buffer, x: u16, y: u16, marked: bool, theme: &crate::render::Theme) -> Rect {
    if !marked {
        buf.set_string(x, y, MENU, Style::default().fg(theme.muted));
        return Rect::new(x, y, 1, 1);
    }
    let badge = Style::default().fg(theme.ground).bg(theme.warn).add_modifier(ratatui::style::Modifier::BOLD);
    let edge = Style::default().fg(theme.warn).bg(theme.ground);
    let left = x.saturating_sub(1);
    buf.set_string(left, y, "\u{2590}", edge);
    buf.set_string(x, y, MENU, badge);
    buf.set_string(x + 1, y, "\u{258c}", edge);
    Rect::new(left, y, x + 2 - left, 1)
}

/// The current server's name or number, cut to `max` cells; none for the legacy server, which has neither.
fn server_label(max: usize) -> Option<String> {
    let id = crate::session::instance_name();
    if id.is_empty() || max == 0 {
        return None;
    }
    let l = crate::session::instance_label(&id);
    Some(if l.chars().count() <= max { l } else { format!("{}…", l.chars().take(max - 1).collect::<String>()) })
}

/// The server's name, then `@ <machine>` in `warn` (Session history's colour for "from elsewhere"), at the top
/// row's right edge, never left of `from`; cut short when narrow. Returns the cells it covers.
fn machine_badge(buf: &mut Buffer, machine: Option<&str>, server: Option<&str>, from: u16, right: u16, y: u16, theme: &crate::render::Theme) -> Option<Rect> {
    let at = machine.map(|m| (format!("@ {m}"), Style::default().fg(theme.warn)));
    let mut parts: Vec<(String, Style)> = server.map(|s| (s.to_string(), Style::default().fg(theme.muted))).into_iter().chain(at).collect();
    let end = right.saturating_sub(TEXT_INSET);
    let width = |parts: &[(String, Style)]| parts.iter().map(|(t, _)| t.chars().count() + 1).sum::<usize>().checked_sub(1);
    // Too narrow for both: the machine says more, so the server's name gives way.
    if parts.len() > 1 && width(&parts)? > end.saturating_sub(from) as usize {
        parts.remove(0);
    }
    let text_w = width(&parts)?;
    let w = (text_w as u16).min(end.saturating_sub(from));
    if w == 0 {
        return None;
    }
    let (mut x, mut left) = (end - w, w as usize);
    for (i, (text, style)) in parts.iter().enumerate() {
        if i > 0 {
            (x, left) = (x + 1, left.saturating_sub(1));
        }
        let n = text.chars().count().min(left);
        buf.set_stringn(x, y, text, n, *style);
        (x, left) = (x + n as u16, left - n);
    }
    Some(Rect::new(end - w, y, w, 1))
}

/// The loop's frame: pane geometry needs `&mut App` (history), so it is settled before the view is lifted out.
pub fn frame(f: &mut Frame, app: &mut App) -> Effects {
    app.plugins.slots.apply();
    if let Some(t) = app.tree.as_mut() {
        t.badges = app.plugins.slots.tree.clone();
    }
    let geometry = app.pane_geometry(areas(app, f.area()).panes);
    if matches!(&app.overlay, crate::app::Overlay::Messages(w) if w.message.is_some()) {
        let v = app.mail_view();
        if let crate::app::Overlay::Messages(w) = &mut app.overlay
            && !w.settle(&v)
        {
            app.overlay = crate::app::Overlay::None;
        }
    }
    let mut view = std::mem::take(&mut app.view);
    let fx = draw(f, app, &mut view, &geometry);
    app.view = view;
    crate::render::strip_spoofing(f.buffer_mut());
    fx
}

/// Draws the frame from `areas` into `view`; the panes must already be sized to it, and placed as `geometry`.
pub fn draw(f: &mut Frame, app: &App, view: &mut ViewState, geometry: &[crate::app::PaneGeometry]) -> Effects {
    crate::paths::set_full(app.cfg.paths == crate::config::PathStyle::Full);
    let full = f.area();
    let theme = app.theme;
    let mut fx = Effects::default();
    view.hits.clear();

    // fill the whole background, otherwise the host terminal shows through behind the panes
    f.buffer_mut()
        .set_style(full, Style::default().bg(theme.ground).fg(theme.text));

    let a = areas(app, full);
    let (top, body, zoom) = (a.top, a.body, a.zoom);
    if let Some(side) = a.sidebar {
        let w = side.width;
        // the top row over the sidebar: nerd's name, and the app menu across from it
        let name = Style::default().fg(theme.accent).add_modifier(ratatui::style::Modifier::BOLD);
        // words line up with the words under them, not with the frame lines, which sit mid-cell:
        // nerd with the sidebar's marks, menu with its right-hand labels. Where the terminal draws
        // images, nerd is the logo's wordmark; elsewhere, the word.
        let logo = app.graphics && !app.caps.mux && w > LOGO_COLS + TEXT_INSET + 4;
        if logo {
            fx.screen.extend(place_logo(app, view, Some((side.x + TEXT_INSET, top.y))));
        } else {
            fx.screen.extend(place_logo(app, view, None));
            f.buffer_mut().set_stringn(side.x + TEXT_INSET, top.y, "nerd", w.saturating_sub(TEXT_INSET) as usize, name);
        }
        // the strokes sit in the cell before the sidebar's right border, so the badge's right half ends on it
        let menu = Rect::new(side.right().saturating_sub(2), top.y, 1, 1);
        let marked = menu_marked(app);
        let button = draw_menu(f.buffer_mut(), menu.x, menu.y, marked, &theme);
        let (waiting, mail, spaces) = sidebar_data(app);
        let hits = crate::sidebar::draw(f.buffer_mut(), side, &waiting, &mail, &spaces, app.focus == crate::app::Focus::Sidebar, &theme);
        view.hits.sidebar_rows = hits.panes;
        view.hits.sidebar_spaces = hits.spaces;
        view.hits.sidebar_group_toggles = hits.group_toggles;
        view.hits.sidebar_new = hits.new_space;
        view.hits.sidebar_toggle = hits.toggle;
        view.hits.sidebar_subagents = hits.subagent_toggles;
        view.hits.sidebar_subagent_rows = hits.subagent_rows;
        view.sidebar_mail = hits.mail;
        // what was drawn, not a second guess at it: the badge is wider than the bare strokes
        view.hits.sidebar_menu = Some(button);
        // the menu opens a cell left of the strokes, with the badge or without
        view.hits.menu_at = Some((menu.x.saturating_sub(1), top.y));
        // width is dragged from the border's right edge
        view.hits.sidebar_divider = Some(Rect { x: side.x + w - 1, width: 1, ..body });
    } else if let Some(rail) = a.rail {
        fx.screen.extend(place_logo(app, view, None));
        let w = rail.width;
        // the menu stays a click away: ≡ over the rail, the update dot beside it
        let marked = menu_marked(app);
        let button = draw_menu(f.buffer_mut(), rail.x + w / 2, top.y, marked, &theme);
        let (waiting, mail, spaces) = sidebar_data(app);
        let hits = crate::sidebar::draw_rail(f.buffer_mut(), rail, &waiting, &mail, &spaces, &theme);
        view.hits.sidebar_rows = hits.panes;
        view.sidebar_mail = hits.mail;
        view.hits.sidebar_spaces = hits.spaces;
        view.hits.sidebar_toggle = hits.toggle;
        // the whole rail width is the target here: the rail is narrow and the strokes sit alone on it
        view.hits.sidebar_menu = Some(Rect { x: rail.x, width: w.max(button.width), ..button });
        view.hits.menu_at = Some((rail.x, top.y));
    } else {
        fx.screen.extend(place_logo(app, view, None));
        view.sidebar_mail.clear();
    }

    let tw = a.tree_w;
    // Kitty bytes only reach a terminal that answered the graphics query; tmux/screen would mangle them.
    let kitty = app.graphics && !app.caps.mux;
    match (app.tree.as_ref(), a.pair) {
        (Some(tree), Some(pair_area)) => {
            let pair = tree.draw_pair(&mut view.tree, f.buffer_mut(), pair_area, &theme, tw);
            // preview image goes to the client as Kitty graphics, outside the frame - ratatui cells can't hold it
            let img = view.tree.take_image_bytes();
            let copied = view.tree.take_copy();
            if kitty {
                fx.screen.extend(img);
            }
            // The preview has no screen to go back to, so its text comes off the frame that drew it.
            if let Some(text) = copied {
                fx.copy = Some(text);
                fx.toast = Some("copied".to_string());
            }
            view.tree_frame = Some(pair.frame);
            view.tree_rect = Some(pair.tree);
            view.preview_rect = pair.preview;
            // The outer border drags the pair's width, the seam the ratio between its halves.
            // Full screen has no width of its own to give, so only the seam answers there.
            view.hits.tree_divider = (!zoom).then_some(Rect { x: pair_area.x, width: 1, ..pair_area });
            view.hits.preview_divider = pair.seam.map(|x| Rect { x, width: 1, ..pair_area });
        }
        (tree, _) => {
            // Too narrow for the tree: an image sent earlier must still go.
            if let Some(img) = tree.map(|_| view.tree.take_image_bytes())
                && kitty
            {
                fx.screen.extend(img);
            }
            view.tree_frame = None;
            view.tree_rect = None;
            view.preview_rect = None;
        }
    }
    let right = a.panes;

    let tabs: Vec<TabItem> = app
        .space()
        .map(|sp| sp.tabs.iter().enumerate().map(|(i, t)| tab_item(app, i, t, i == sp.active_tab)).collect())
        .unwrap_or_default();

    // Without the sidebar the server's name joins the machine badge, cut short so the tabs keep their room.
    let server = if a.sidebar.is_none() { server_label(16) } else { None };
    let badge = machine_badge(f.buffer_mut(), app.caps.machine.as_deref(), server.as_deref(), right.x, full.right(), top.y, &theme);
    // The tabs, and with them the + button, stop a cell short of the badge.
    let tabs_w = badge.map_or(right.width, |b| right.width.min(b.x.saturating_sub(right.x + 1)));
    let tabs_area = Rect { x: right.x, y: top.y, width: tabs_w, height: 1 };
    let (tab_rects, plus) = crate::chrome::draw_tabs(f.buffer_mut(), tabs_area, &tabs, app.focus == crate::app::Focus::Tabs, &theme);
    let mut from = plus.or(tab_rects.last().map(|(r, _)| *r)).map_or(tabs_area.x, |r| r.right() + 1);
    let end = badge.map_or(full.right().saturating_sub(TEXT_INSET), |b| b.x.saturating_sub(1));
    if app.prefix {
        let label = app.cfg.prefix.label();
        let w = crate::slots::cells(label) as u16;
        if end > from + w {
            f.buffer_mut().set_string(from + 1, top.y, label, Style::default().fg(theme.accent));
            from += w + 3;
        }
    }
    let seg_hits = draw_segments(f.buffer_mut(), app, from, end, top.y);
    view.hits.segments = seg_hits;
    view.hits.tabs = tab_rects;
    view.hits.new_tab = plus;

    let panes_area = right;

    let mode = app.cfg.pane_border;
    let gap = app.cfg.pane_gap;
    let (frames, hits, cursor, borders) = {
        let Some(tab) = app.tab() else { return fx };
        let focused = tab.layout.focused();

        let mut frames = Vec::new();
        let mut hits = Vec::new();
        let mut cursor = None;

        for g in geometry {
            let Some(pane) = tab.panes.get(&g.id) else { continue };
            let pr = g;
            let inner = g.text;
            // lit only while the keys are in the panes: a lit frame says where the keys are
            let is_focused = g.id == focused && app.focus == crate::app::Focus::Panes;

            crate::render::blit_grid(f.buffer_mut(), inner, pane.term.screen(), &theme);
            if let Some(sel) = app.selection.filter(|s| s.pane == g.id) {
                crate::render::highlight(f.buffer_mut(), inner, &sel, &theme);
            }
            // only while scrolled up: at the live screen there is nothing to say
            if let Some((offset, above)) = g.scrollbar
                && offset > 0
            {
                crate::render::draw_scrollbar(f.buffer_mut(), g.inner, offset, above, theme.muted, &theme);
            }
            if let Some((_, s)) = app.search.as_ref().filter(|(sid, _)| *sid == g.id) {
                s.highlight(f.buffer_mut(), inner, pane.term.scroll, &theme);
                if inner.height > 0 {
                    let bar = Rect { y: inner.bottom() - 1, height: 1, ..inner };
                    s.draw_bar(f.buffer_mut(), bar, &theme);
                }
            }
            if is_focused {
                cursor = crate::render::cursor_at(inner, pane.term.screen());
            }

            // agent state lives in the sidebar only; the name is the pane's own
            let mut title = pane.label();
            if !pane.isolation.is_open() {
                title = format!("{title} — {}", pane.isolation.words());
            }
            if tab.layout.is_zoomed() {
                title.push_str("  □");
            }
            frames.push((pr.rect, title, is_focused));

            hits.push(PaneHit {
                id: pr.id,
                rect: pr.rect,
                inner,
                wants_mouse: pane.term.wants_mouse(),
                mouse_mode: pane.term.screen().mouse_protocol_mode(),
                wheel: pane.term.wheel_routing(),
            });
        }
        let borders = tab.layout.borders(panes_area, gap);
        (frames, hits, cursor, borders)
    };

    crate::render::draw_panes(f.buffer_mut(), &frames, mode, &theme);
    view.hits.panes = hits;
    view.hits.borders = borders;

    // A subagent's run: the right part of its agent's pane, as wide as the file tree until its seam
    // is dragged; the pane keeps its size underneath, so nothing on screen moves for it. Zoomed,
    // it takes everything under the top bar.
    let mut cursor = cursor;
    if let Some(pane) = app.run.as_ref().map(|r| r.pane) {
        let placed = geometry.iter().find(|g| g.id == pane).map(|g| (g.rect, g.inner != g.rect));
        let focused = app.tab().is_some_and(|t| t.layout.focused() == pane) && app.focus == crate::app::Focus::Panes;
        let data = app.pane(pane).map(|p| {
            let id = app.run.as_ref().map(|r| r.id.clone()).unwrap_or_default();
            (p.label(), p.activity.subs.iter().find(|s| s.id == id).cloned(), p.activity.cut)
        });
        let width = app.run_widths.get(&pane).copied().unwrap_or(tw + 4);
        let zoomed = app.run_zoom;
        let border = if focused || zoomed { theme.border_focus } else { theme.border };
        let copied = match (placed, data, app.run.as_ref()) {
            (_, Some((name, sub, cut)), Some(run)) if zoomed => {
                cursor = None;
                run.draw(&mut view.run, f.buffer_mut(), body, None, false, border, true, &name, &theme, sub.as_ref(), cut, crate::hook::now())
            }
            (Some((rect, framed)), Some((name, sub, cut)), Some(run)) if rect.width >= crate::run::MIN_W + crate::run::PANE_MIN_W => {
                let w = width.clamp(crate::run::MIN_W, rect.width - crate::run::PANE_MIN_W);
                let section = Rect { x: rect.right() - w, width: w, ..rect };
                let copied = run.draw(&mut view.run, f.buffer_mut(), section, Some(rect), framed, border, focused, &name, &theme, sub.as_ref(), cut, crate::hook::now());
                if cursor.is_some_and(|(x, y)| view.run.contains(x, y)) {
                    cursor = None;
                }
                copied
            }
            _ => {
                view.run.hide();
                None
            }
        };
        if let Some(text) = copied {
            fx.copy = Some(text);
            fx.toast = Some("copied".to_string());
        }
    }

    // the battery's details under it, below a notice card that shares the corner
    if app.battery_hover.is_some()
        && let Some(b) = app.battery
        && let Some((r, _)) = view.hits.segments.iter().find(|(_, h)| *h == crate::mouse::SegmentHit::Battery)
        && r.y + 1 < full.bottom()
    {
        let text = format!(" {} ", b.details());
        let w = (crate::slots::cells(&text) as u16).min(full.width);
        let x = r.right().min(full.right()).saturating_sub(w).max(full.x);
        f.buffer_mut().set_string(x, r.y + 1, &text, Style::default().fg(theme.text).bg(theme.surface));
    }

    // Over the panes' top right corner, below the top row: the menu button and the tabs stay in reach.
    let msg = match (&app.notice, &app.toast) {
        (Some(m), _) => Some((m.as_str(), true)),
        (None, Some(m)) => Some((m.as_str(), false)),
        _ => None,
    };
    if let Some((msg, error)) = msg {
        let area = Rect { x: a.panes.x, width: full.right().saturating_sub(a.panes.x), ..body };
        notice_card(f.buffer_mut(), area, msg, error, app.card_more().is_some(), &theme, &mut view.hits);
    }

    view.dialog = Default::default();
    match &app.overlay {
        crate::app::Overlay::Menu(m, _) => m.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Prompt(p, _) => {
            p.draw(f.buffer_mut(), full, &theme);
            let (cx, cy) = p.cursor(full);
            f.set_cursor_position((cx, cy));
            return fx;
        }
        crate::app::Overlay::Find(w) => {
            view.dialog.find = w.draw(f.buffer_mut(), full, &theme);
            if let Some(at) = w.cursor(full) {
                f.set_cursor_position(at);
                return fx;
            }
        }
        crate::app::Overlay::Confirm(c, _) => view.dialog.rects = c.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Picker(p, _) => view.dialog.rects = p.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::NewSpace(w) => {
            view.dialog.rects = w.draw(f.buffer_mut(), full, &theme);
            let (cx, cy) = w.cursor(full);
            f.set_cursor_position((cx, cy));
            return fx;
        }
        crate::app::Overlay::Settings(s) => view.dialog.rows = s.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Keys(h) => h.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Sessions(w) => view.dialog.panel = w.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Plugins(w) => view.dialog.panel = w.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Servers(w) => view.dialog.panel = w.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Panel(p) => view.dialog.panel = p.draw(f.buffer_mut(), full, &theme, app.plugins.slots.panel(&p.src, &p.id)),
        crate::app::Overlay::Messages(w) => {
            let named = app.named_panes();
            view.dialog.messages = w.draw(f.buffer_mut(), full, &theme, &app.mail_view(), &|a, b| crate::app::App::mail_between_in(&named, a, b));
        }
        crate::app::Overlay::Isolation(w) => view.dialog.isolation = w.draw(f.buffer_mut(), full, &theme),
        crate::app::Overlay::Accounts(w) => view.dialog.isolation = w.draw(f.buffer_mut(), full, &theme, &crate::policy::current().accounts),
        crate::app::Overlay::None => {}
    }

    // placed last - borders may have redrawn the cells around it; while a menu or dialog is open keys go to it,
    // and the pane's cursor would show through it
    if let Some((x, y)) = cursor
        && matches!(app.overlay, crate::app::Overlay::None)
    {
        f.set_cursor_position((x, y));
    }
    fx
}

/// Widest a notice card gets, frame included.
const NOTICE_W: u16 = 64;
/// Columns between the card and the right edge of the area: the pane's border and a cell of ground.
const NOTICE_INSET: u16 = 3;

/// One line in a frame at the top right of `area`: cut with "… more" when long, and a × that closes it.
/// A click on the card opens the whole text; the hits say where.
/// `more`: a window stands behind the card, so "… more" shows even when the text fits.
fn notice_card(buf: &mut Buffer, area: Rect, msg: &str, error: bool, more: bool, theme: &crate::render::Theme, hits: &mut crate::mouse::Hits) {
    let msg = crate::render::unspoofed(&msg.replace('\n', " "));
    let len = crate::slots::cells(&msg) as u16 + if more { 7 } else { 0 };
    // the frame's border and padding, then the text, then " ×"
    let pad = 2 * crate::chrome::DIALOG_FRAME.horizontal + 2;
    // Inside the pane's frame: its top line and its right edge, with the ground cell around the card, stay in sight.
    let w = (len + pad).min(NOTICE_W).min(area.width.saturating_sub(2 * NOTICE_INSET));
    // Room for a few letters and × is all a card needs; a short message makes a small card.
    if area.width.saturating_sub(2 * NOTICE_INSET) < pad + 4 || area.height < 4 {
        return;
    }
    let r = Rect { x: area.right().saturating_sub(w + NOTICE_INSET), y: area.y + 1, width: w, height: 3 };
    let border = if error { theme.bad } else { theme.border_focus };
    // Nothing is painted above it: the top row stays as it is.
    let inner = crate::chrome::dialog(buf, r, None, border, theme, crate::chrome::Moat::NotAbove);
    let ground = Style::default().bg(theme.ground);
    let room = inner.width.saturating_sub(2) as usize;
    let shown: String = if len as usize <= room {
        if more { format!("{msg} ") } else { msg.clone() }
    } else {
        let mut out = String::new();
        for c in msg.chars() {
            if crate::slots::cells(&out) + crate::slots::cells(&c.to_string()) > room.saturating_sub(6) {
                break;
            }
            out.push(c);
        }
        out
    };
    buf.set_stringn(inner.x, inner.y, &shown, room, ground.fg(theme.text));
    if more || shown.len() < msg.len() {
        let x = inner.x + crate::slots::cells(&shown) as u16;
        buf.set_string(x, inner.y, "… ", ground.fg(theme.text));
        buf.set_string(x + 2, inner.y, "more", ground.fg(theme.accent));
    }
    let close = Rect { x: inner.right().saturating_sub(1), y: inner.y, width: 1, height: 1 };
    buf.set_string(close.x, close.y, "×", ground.fg(theme.muted));
    hits.notice = Some(r);
    hits.notice_close = Some(Rect { x: close.x.saturating_sub(1), width: 3, ..close });
}

/// A tab's chip: its name, then one badge per plugin.
fn tab_item(app: &App, index: usize, t: &crate::app::Tab, active: bool) -> TabItem {
    let focused = t.layout.focused();
    let mut ids = t.layout.pane_ids();
    ids.sort_unstable_by_key(|id| (*id != focused, *id));
    let badges = app.plugins.slots.tab_badges(&ids).into_iter().map(|b| (b.text.clone(), b.role.color(&app.theme))).collect();
    TabItem { index, name: t.label(index), active, badges }
}

/// The top row's segments in `[from, end)`: the plugins', the battery, then the clock; returns what a click reaches.
fn draw_segments(buf: &mut Buffer, app: &App, from: u16, end: u16, y: u16) -> Vec<(Rect, crate::mouse::SegmentHit)> {
    use crate::mouse::SegmentHit;
    let theme = &app.theme;
    let mut hits = Vec::new();
    // the clock keeps its place, then the battery before it; the plugins' segments drop first
    // the date goes first when the row is short: the time alone stays, and its click with it
    let fits = |s: &str| end >= from + crate::slots::cells(s) as u16;
    let shown = if fits(&app.clock) { app.clock.as_str() } else { app.clock.rsplit_once("  ").map_or(app.clock.as_str(), |(_, time)| time) };
    let clock = crate::slots::cells(shown) as u16;
    let mut clock_x = None;
    let end = if clock > 0 && end >= from + clock {
        buf.set_string(end - clock, y, shown, Style::default().fg(theme.muted));
        clock_x = Some(end - clock);
        hits.push((Rect::new(end - clock, y, clock, 1), SegmentHit::Clock));
        (end - clock).saturating_sub(3)
    } else {
        end
    };
    // the battery before it: a bolt on the charger, the number yellow then red as it runs out
    let mut battery_x = None;
    let time = app.cfg.battery == crate::config::Battery::Time;
    let end = match app.battery.map(|b| (b, if time { b.time_text() } else { b.text() })) {
        Some((b, text)) if end >= from + crate::slots::cells(&text) as u16 => {
            use crate::battery::{Level, Power};
            let w = crate::slots::cells(&text) as u16;
            let number = match b.level(app.cfg.battery_low, app.cfg.battery_critical) {
                Level::Critical => theme.bad,
                Level::Low => theme.warn,
                Level::Fine => theme.muted,
            };
            let mut x = end - w;
            battery_x = Some(x);
            hits.push((Rect::new(x, y, w, 1), SegmentHit::Battery));
            if b.power != Power::Battery {
                buf.set_string(x, y, "\u{3df}", Style::default().fg(if b.power == Power::Charging { theme.ok } else { theme.faint }));
                x += 2;
            }
            let body = if time { b.time() } else { format!("{}%", b.percent) };
            buf.set_string(x, y, &body, Style::default().fg(number));
            if b.low_power {
                buf.set_string(x + crate::slots::cells(&body) as u16 + 1, y, "L", Style::default().fg(theme.faint));
            }
            (end - w).saturating_sub(3)
        }
        _ => end,
    };
    let segs: Vec<_> = app.plugins.slots.segments().map(|s| (s.text.clone(), s.role.color(theme), s.command.clone().map(|c| SegmentHit::Command(s.source.clone(), c)))).collect();
    let widths: Vec<u16> = segs.iter().map(|s| crate::slots::cells(&s.0) as u16).collect();
    let mut plugins: Option<Rect> = None;
    for (i, (((text, fg, hit), w), x)) in segs.into_iter().zip(&widths).zip(crate::slots::layout(&widths, from, end)).enumerate() {
        if i > 0 {
            buf.set_string(x - 2, y, "\u{b7}", Style::default().fg(theme.faint));
        }
        buf.set_string(x, y, &text, Style::default().fg(fg));
        let r = Rect::new(x, y, *w, 1);
        plugins = Some(plugins.map_or(r, |s| s.union(r)));
        if let Some(h) = hit {
            hits.push((r, h));
        }
    }
    // a dot before the battery and the clock, when something stands left of them
    let mut dot = |x: u16| buf.set_string(x - 2, y, "\u{b7}", Style::default().fg(theme.faint));
    if let Some(b) = battery_x.filter(|_| plugins.is_some()) {
        dot(b);
    }
    if let Some(c) = clock_x.filter(|_| plugins.is_some() || battery_x.is_some()) {
        dot(c);
    }
    hits
}

/// The sidebar's data: agents waiting on you, newest change first, the mail waiting on you, and every space with its panes.
fn sidebar_data(app: &App) -> (Vec<crate::sidebar::Waiting>, Vec<crate::sidebar::MailRow>, Vec<crate::sidebar::SpaceNode>) {
    use crate::sidebar::{MailRow, PaneNode, SpaceNode, Waiting};
    use crate::term::AgentState;
    let active = app.active;
    let focused_pane = app.tab().map(|t| t.layout.focused());
    let repo = |sp: &crate::app::Space| app.repos.get(&sp.cwd);
    let keys: Vec<Option<PathBuf>> = app.spaces.iter().map(|sp| repo(sp).and_then(|r| r.key.clone())).collect();
    // a group needs more than one worktree; a lone main checkout stays a plain row
    let members = |k: &PathBuf| keys.iter().filter(|g| g.as_ref() == Some(k)).count();
    let mut waiting = Vec::new();
    let mut spaces = Vec::new();
    let now = crate::hook::now();
    // one instant for every row: rows whose events are microseconds apart would otherwise trade places each frame
    let frame = std::time::Instant::now();
    let mail: Vec<MailRow> = app
        .mail
        .waiting()
        .map(|m| MailRow {
            id: m.id.clone(),
            text: match m.state {
                crate::mail::State::Held(crate::mail::Hold::RouteRequest) => format!("{} asks a route to {}", m.from.name(), m.to.name()),
                crate::mail::State::Held(_) => format!("{} → {} · held", m.from.name(), m.to.name()),
                _ => format!("{} → {}", m.from.name(), m.to.name()),
            },
            waited: std::time::Duration::from_secs(now.saturating_sub(m.created)),
        })
        .collect();
    for (i, sp) in app.spaces.iter().enumerate() {
        let many = sp.tabs.len() > 1;
        // panes of one space with the same label get a number, in pane order, so their rows differ
        let mut alike: std::collections::BTreeMap<String, Vec<crate::layout::PaneId>> = Default::default();
        for p in sp.tabs.iter().flat_map(|t| t.panes.values()) {
            alike.entry(p.label()).or_default().push(p.id);
        }
        alike.values_mut().for_each(|v| v.sort_unstable());
        let mut panes = Vec::new();
        for (ti, tab) in sp.tabs.iter().enumerate() {
            let tab_name = many.then(|| tab.label(ti));
            let mut ids = tab.layout.pane_ids();
            ids.sort_unstable();
            for id in ids {
                let Some(p) = tab.panes.get(&id) else { continue };
                let state = p.term.shown();
                if p.agent.is_some() && matches!(state, AgentState::Done | AgentState::Blocked | AgentState::Failed) {
                    let waited = p.term.changed_at().map(|at| frame.saturating_duration_since(at));
                    let asks = p.activity.asking().map(str::to_string);
                    let name = match &alike[&p.label()][..] {
                        [_] => p.label(),
                        ids => format!("{} {}", p.label(), ids.iter().position(|i| *i == p.id).unwrap_or(0) + 1),
                    };
                    waiting.push(Waiting { pane: p.id, name, space: sp.label(), tab: tab_name.clone(), state, waited, asks });
                }
                let subagents = p
                    .activity
                    .subs
                    .iter()
                    .filter(|s| s.shown(now))
                    .map(|s| crate::sidebar::SubRow {
                        open: app.run.as_ref().is_some_and(|r| r.pane == p.id && r.id == s.id),
                        ..crate::sidebar::SubRow::of(s, now)
                    })
                    .collect();
                panes.push(PaneNode {
                    pane: p.id,
                    agent: p.agent.is_some(),
                    name: p.label(),
                    tab: tab_name.clone(),
                    state,
                    focused: Some(p.id) == focused_pane,
                    // entering the sidebar puts its cursor on the focused pane, so it has no away cursor
                    selected: app.focus == crate::app::Focus::Sidebar
                        && app.sidebar_sel == Some(crate::app::SidebarSel::Pane(p.id)),
                    subagents,
                    open: app.subagents_open.contains(&p.id),
                });
            }
        }
        // agents first, terminals after; the sort is stable, so each keeps its tab order
        panes.sort_by_key(|p| !p.agent);
        let group = keys[i].clone().filter(|k| members(k) > 1);
        spaces.push(SpaceNode {
            index: i,
            selected: app.focus == crate::app::Focus::Sidebar
                && app.sidebar_sel == Some(crate::app::SidebarSel::Space(i)),
            name: sp.label(),
            branch: repo(sp).and_then(|r| r.branch.clone()),
            state: loudest_state(sp),
            active: i == active,
            collapsed: group.as_ref().is_some_and(|g| app.collapsed_groups.contains(g)),
            is_group_parent: group.is_some() && repo(sp).is_some_and(|r| r.main),
            group,
            panes,
        });
    }
    crate::sidebar::sort_waiting(&mut waiting);
    (waiting, mail, spaces)
}

fn loudest_state(sp: &crate::app::Space) -> crate::term::AgentState {
    loudest_agent(sp.tabs.iter().flat_map(|t| t.panes.values()).map(|p| (p.agent.is_some(), p.term.shown())))
}

/// Plain terminals turn "done" after every command, so they would drown out a working agent.
fn loudest_agent(panes: impl Iterator<Item = (bool, crate::term::AgentState)>) -> crate::term::AgentState {
    panes
        .filter(|(agent, _)| *agent)
        .map(|(_, s)| s)
        .max_by_key(|s| s.priority())
        .unwrap_or(crate::term::AgentState::Unknown)
}

#[cfg(test)]
mod tests {
    use crate::term::AgentState::*;

    /// The logo placed as a frame would place it, with the app's own view.
    fn logo(app: &mut crate::app::App, at: Option<(u16, u16)>) -> Vec<u8> {
        let mut view = std::mem::take(&mut app.view);
        let bytes = super::place_logo(app, &mut view, at);
        app.view = view;
        bytes
    }

    #[test]
    fn agents_of_one_space_with_the_same_label_get_numbers_in_pane_order() {
        use crate::app::{Space, Tab};
        let mut app = crate::app::App::new(crate::config::Config::default());
        let blocked = |app: &crate::app::App, id| crate::app::snapshots::agent_pane(app, id, Blocked, 0);
        let (one, two, mut rev) = (blocked(&app, 1), blocked(&app, 2), blocked(&app, 3));
        rev.label = Some("rev".into());
        let mut layout = crate::layout::Layout::new(1);
        layout.split(crate::layout::Dir::Horizontal, 2);
        layout.split(crate::layout::Dir::Horizontal, 3);
        let tab = Tab { name: None, layout, panes: std::collections::HashMap::from([(1, one), (2, two), (3, rev)]), isolation_new: None };
        app.spaces.push(Space { name: Some("nerd".into()), cwd: "/".into(), tabs: vec![tab], active_tab: 0, isolation_new: None });
        let (waiting, _, _) = super::sidebar_data(&app);
        let rows: Vec<(usize, &str)> = waiting.iter().map(|w| (w.pane, w.name.as_str())).collect();
        assert_eq!(rows, [(1, "claude 1"), (2, "claude 2"), (3, "rev")]);
    }

    #[test]
    fn a_sidebar_with_no_room_collapses_to_its_rail() {
        let app = crate::app::App::new(crate::config::Config::default());
        let w = app.cfg.sidebar_width;
        let wide = super::areas(&app, ratatui::layout::Rect::new(0, 0, w + 60, 20));
        assert!(wide.sidebar.is_some() && wide.rail.is_none());
        let narrow = super::areas(&app, ratatui::layout::Rect::new(0, 0, w + 10, 20));
        assert!(narrow.sidebar.is_none() && narrow.rail.is_some(), "collapsed, not gone");
        assert_eq!(narrow.panes.x, crate::sidebar::RAIL_W + super::SIDE_GAP);
        let mut app = app;
        app.screen = ratatui::layout::Rect::new(0, 0, w + 10, 20);
        app.run_action(crate::app::Action::ToggleSidebar, &crate::app::MenuCtx::Global);
        assert!(app.sidebar && app.toast.as_deref() == Some("the window is too narrow for the sidebar"), "its rail's toggle does not switch it off");
    }

    #[test]
    fn a_dark_theme_with_its_own_ground_keeps_the_dark_logo() {
        let mut cfg = crate::config::Config::default();
        cfg.colors.insert("ground".into(), "#123456".into());
        let mut app = crate::app::App::new(cfg);
        app.apply_theme();
        let _ = logo(&mut app, Some((2, 0)));
        assert_eq!(app.view.logo_at, Some((2, 0, true, 32)));
    }

    #[test]
    fn the_logo_is_sent_once_per_place_and_theme_and_taken_away() {
        let mut app = crate::app::App::new(crate::config::Config::default());
        let sent = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();

        let first = sent(logo(&mut app, Some((2, 0))));
        assert!(first.contains("\x1b[1;3H") && first.contains("a=T,f=100,i=2,c=9,r=1,"), "nine cells by one row at the corner");
        assert_eq!(sent(logo(&mut app, Some((2, 0)))), "", "nothing new to send");
        // a resize clears the client's screen, and a terminal that draws images deletes them with it
        app.images_lost();
        assert!(sent(logo(&mut app, Some((2, 0)))).contains("a=T"), "the same place is not the same as still there");

        app.cfg.theme = crate::config::ThemeMode::Light;
        app.apply_theme();
        assert!(sent(logo(&mut app, Some((2, 0)))).contains("a=T"), "the light wordmark replaces the dark one");

        assert_eq!(sent(logo(&mut app, None)), "\x1b_Ga=d,d=i,i=2,q=2\x1b\\", "collapsed: the image goes");
        assert_eq!(sent(logo(&mut app, None)), "", "and only once");
    }

    #[test]
    fn the_wordmark_comes_from_the_shortest_raster_at_least_a_row_tall() {
        let h = |px| super::logo_for(px).0;
        assert_eq!(h(0), 32, "the client did not say");
        assert_eq!((h(16), h(17), h(25), h(35)), (16, 20, 28, 36));
        assert_eq!(h(70), 64, "taller than all: the tallest");
        // a PNG's width and height sit at bytes 16..24
        let size = |png: &[u8]| (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()));
        for (h, dark, light) in super::LOGOS {
            let want = (u32::from(h) * 9 / 2, u32::from(h));
            assert_eq!((size(dark), size(light)), (want, want), "the {h} px wordmark is 4.5:1");
        }
    }

    #[test]
    fn another_row_height_sends_the_logo_again() {
        let mut app = crate::app::App::new(crate::config::Config::default());
        let png = |px| crate::preview::png_bytes(super::logo_for(px).1, 2, 0, "c=9,r=1", super::LOGO_ID);
        app.caps.cell_px = 18;
        assert_eq!(logo(&mut app, Some((2, 0))), png(18));
        app.caps.cell_px = 19;
        assert!(logo(&mut app, Some((2, 0))).is_empty(), "the same raster: nothing to send");
        app.caps.cell_px = 35;
        assert_eq!(logo(&mut app, Some((2, 0))), png(35), "another display: the taller raster replaces it");
    }

    #[test]
    fn what_is_drawn_is_what_can_be_clicked() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = crate::render::Theme::dark();
        let area = Rect::new(0, 0, 10, 1);
        for marked in [false, true] {
            let mut buf = Buffer::empty(area);
            let button = super::draw_menu(&mut buf, 5, 0, marked, &theme);
            let filled: Vec<u16> = (0..area.width).filter(|&x| buf[(x, 0)].bg == theme.warn).collect();
            if marked {
                assert_eq!(filled, vec![5], "the badge's whole cell is the strokes'; its halves are drawn by glyphs");
            } else {
                assert!(filled.is_empty(), "nothing is filled while nothing waits");
            }
            let clickable: Vec<u16> = (button.x..button.right()).collect();
            if marked {
                assert_eq!(clickable, vec![4, 5, 6], "the half cells either side open the menu too");
            } else {
                assert_eq!(clickable, vec![5], "the strokes alone are the target");
            }
        }
    }

    #[test]
    fn the_menu_button_becomes_a_badge_while_an_update_waits() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = crate::render::Theme::dark();
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        let rect = super::draw_menu(&mut buf, 5, 0, false, &theme);
        assert_eq!((buf[(5, 0)].symbol(), buf[(5, 0)].fg), ("\u{2261}", theme.muted), "strokes alone until something waits");
        assert_eq!(rect, Rect::new(5, 0, 1, 1), "flush against the edge it was given");
        assert_ne!(buf[(4, 0)].bg, theme.warn, "nothing filled beside it");

        let mut buf = Buffer::empty(area);
        let rect = super::draw_menu(&mut buf, 5, 0, true, &theme);
        assert_eq!(rect, Rect::new(4, 0, 3, 1), "half a cell either side of the strokes");
        assert_eq!((buf[(5, 0)].symbol(), buf[(5, 0)].fg, buf[(5, 0)].bg), ("\u{2261}", theme.ground, theme.warn), "the strokes stay in their cell");
        assert_eq!((buf[(4, 0)].symbol(), buf[(4, 0)].fg, buf[(4, 0)].bg), ("\u{2590}", theme.warn, theme.ground), "the right half of the cell before");
        assert_eq!((buf[(6, 0)].symbol(), buf[(6, 0)].fg, buf[(6, 0)].bg), ("\u{258c}", theme.warn, theme.ground), "the left half of the cell after");
        assert_ne!(buf[(3, 0)].bg, theme.warn, "nothing beyond the halves");
        assert_ne!(buf[(7, 0)].bg, theme.warn, "nothing beyond the halves");
    }

    #[test]
    fn a_quiet_terminal_does_not_hide_a_working_agent() {
        assert_eq!(super::loudest_agent([(true, Working), (false, Done), (false, Blocked)].into_iter()), Working);
        assert_eq!(super::loudest_agent([(true, Working), (true, Done)].into_iter()), Done);
        assert_eq!(super::loudest_agent([(false, Done)].into_iter()), Unknown);
    }
}
