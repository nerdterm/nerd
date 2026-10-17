//! Settings screen (`option+,`): no save button, the caller writes `Config` on [`SettingsOutcome::Changed`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::config::{
    BorderMode, Config, ImageMode, PANE_GAP, PREVIEW_WIDTH, PathStyle, Prefix, SCROLLBACK, SIDEBAR_WIDTH, TREE_WIDTH, ThemeMode,
    TmuxOffer,
};
use crate::render::Theme;

const SCROLLBACK_HINT: &str = "for panes opened after the change";
const CONFIG_HINT: &str = "colors, machines, plugins and security";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Scrollbar,
    Clock,
    Battery,
    PaneBorder,
    SidebarWidth,
    TreeWidth,
    PaneGap,
    PreviewWidth,
    Scrollback,
    EditConfig,
    NotifySound,
    NotifyBell,
    RestoreScreens,
    Preview,
    Wrap,
    Images,
    Paths,
    Theme,
    Tmux,
    Prefix,
    Hook,
    Messaging,
    PanesStop,
    Topology,
    Accounts,
}

const ROWS: &[(Option<&str>, &str, Item)] = &[
    (Some("Look"), "Theme", Item::Theme),
    (None, "Pane borders", Item::PaneBorder),
    (None, "Gap between panes", Item::PaneGap),
    (None, "Scrollbars", Item::Scrollbar),
    (None, "Clock in the top row", Item::Clock),
    (None, "Battery in the top row", Item::Battery),
    (Some("Layout"), "Sidebar width", Item::SidebarWidth),
    (None, "File tree width", Item::TreeWidth),
    (None, "Preview width", Item::PreviewWidth),
    (Some("Files"), "File preview", Item::Preview),
    (None, "Wrap long lines", Item::Wrap),
    (None, "Images", Item::Images),
    (None, "Show paths", Item::Paths),
    (Some("When an agent needs you"), "Play a sound", Item::NotifySound),
    (None, "Ring the terminal bell", Item::NotifyBell),
    (Some("Terminal"), "Inside tmux", Item::Tmux),
    (None, "tmux-style prefix", Item::Prefix),
    (None, "History lines per pane", Item::Scrollback),
    (None, "Restore screens after a restart", Item::RestoreScreens),
    (Some("Messages"), "Messaging", Item::Messaging),
    (None, "Programs in panes may stop or update nerd", Item::PanesStop),
    (None, "Set up a topology…", Item::Topology),
    (None, "Accounts…", Item::Accounts),
    (Some("Agents"), "Agent hooks", Item::Hook),
    (None, "Edit config.json…", Item::EditConfig),
];

/// Same order as docs/configuration.md.
const BORDERS: [BorderMode; 3] = [BorderMode::Always, BorderMode::Multiple, BorderMode::Never];

const IMAGES: [ImageMode; 3] = [ImageMode::Auto, ImageMode::Blocks, ImageMode::Off];

const THEMES: [ThemeMode; 3] = [ThemeMode::Auto, ThemeMode::Dark, ThemeMode::Light];

const TMUX: [TmuxOffer; 3] = [TmuxOffer::Ask, TmuxOffer::NewWindow, TmuxOffer::Stay];

const PREFIXES: [Prefix; 3] = [Prefix::Off, Prefix::CtrlB, Prefix::CtrlA];

#[derive(Debug, PartialEq)]
pub enum SettingsOutcome {
    None,
    Changed,
    Close,
    /// Agents not hooked before: the caller asks first.
    InstallHook,
    /// Agents already hooked: the caller asks before taking the hook out.
    RemoveHook,
    EditConfig,
    /// A Messages row: stored by the server through the app, never in config.json.
    Mail(crate::app::MailAction),
    Topology,
    Accounts,
}

pub struct Settings {
    /// Index into [`ROWS`]; section headers aren't list rows and don't count.
    selected: usize,
    /// `draw()` only receives the theme, so the screen keeps its own copy of the values.
    cfg: Config,
    hook_installed: bool,
    /// An install error, red, under the list; without one the selected row's hint shows there.
    message: Option<(bool, String)>,
}

/// One key this screen answers: the handler dispatches on these rows and the strip prints them.
pub struct Key {
    pub code: crossterm::event::KeyCode,
    /// Empty when the row is a second code for the row above.
    pub label: &'static str,
    pub word: &'static str,
    pub what: &'static str,
    act: fn(&mut Settings, &mut Config) -> SettingsOutcome,
}

pub const KEYS: &[Key] = &[
    Key { code: crossterm::event::KeyCode::Up, label: "↑↓", word: "", what: "up and down the settings",
          act: |s, _| { s.step(-1); SettingsOutcome::None } },
    Key { code: crossterm::event::KeyCode::Down, label: "", word: "", what: "up and down the settings",
          act: |s, _| { s.step(1); SettingsOutcome::None } },
    Key { code: crossterm::event::KeyCode::Left, label: "←→", word: "change", what: "change the selected setting",
          act: |s, cfg| s.apply(-1, cfg) },
    Key { code: crossterm::event::KeyCode::Right, label: "", word: "", what: "change the selected setting",
          act: |s, cfg| s.apply(1, cfg) },
    Key { code: crossterm::event::KeyCode::Enter, label: "enter", word: "change", what: "change the selected setting",
          act: |s, cfg| s.apply(1, cfg) },
    Key { code: crossterm::event::KeyCode::Char(' '), label: "", word: "", what: "change the selected setting",
          act: |s, cfg| s.apply(1, cfg) },
    Key { code: crossterm::event::KeyCode::Esc, label: "esc", word: "close", what: "close the screen",
          act: |_, _| SettingsOutcome::Close },
];

impl Settings {
    pub fn new(cfg: &Config) -> Self {
        Self {
            selected: 0,
            cfg: cfg.clone(),
            hook_installed: crate::hook::is_installed(),
            message: None,
        }
    }

    /// Fits its widest row or hint, which the message line cuts rather than wraps, and the key strip under the list.
    pub fn rect(&self, screen: Rect) -> Rect {
        let rows = ROWS.iter().map(|(_, label, item)| label.chars().count() + 4 + self.value(*item).chars().count());
        let hints = [SCROLLBACK_HINT, CONFIG_HINT].map(|h| h.chars().count());
        let widest = rows.chain(hints).max().unwrap_or(0) as u16;
        let strip = hint_strip(crate::chrome::fit(screen, widest, 0).width.saturating_sub(4) as usize).len();
        // the list, the message line, the key strip
        crate::chrome::fit(screen, widest, (lines().len() + 1 + strip) as u16)
    }

    /// Draws the panel; returns each setting's row and its index into `ROWS`.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Vec<(Rect, usize)> {
        let mut rows = Vec::new();
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return rows;
        }
        crate::chrome::dialog(buf, r, Some("Settings"), theme.border_focus, theme, crate::chrome::Moat::All);
        let inner = crate::chrome::close_button(buf, r, theme);
        if inner.width == 0 || inner.height == 0 {
            return rows;
        }
        let width = inner.width as usize;
        let y = inner.y;

        // Carve off the bottom before the list, or a long list overwrites the hint.
        let mut bottom = crate::chrome::draw_hint(buf, inner, &hint_strip(width), theme).max(y);
        let note = self.message.as_ref().map(|(bad, t)| (*bad, t.as_str())).or(hint(ROWS[self.selected].2).map(|t| (false, t)));
        if let Some((is_error, text)) = note
            && bottom > y
        {
            bottom -= 1;
            let color = if is_error { theme.bad } else { theme.faint };
            buf.set_stringn(inner.x, bottom, text, width, Style::default().fg(color));
        }

        // A screen shorter than the list scrolls so the selected row stays in view.
        let all = lines();
        let room = bottom.saturating_sub(y) as usize;
        let at = all.iter().position(|l| *l == Line::Row(self.selected)).unwrap_or(0);
        for (y, line) in (y..).zip(all.iter().skip((at + 1).saturating_sub(room))) {
            if y >= bottom {
                break;
            }
            match *line {
                Line::Head(h) => buf.set_stringn(inner.x, y, h, width, Style::default().fg(theme.muted)).0,
                Line::Sep => buf.set_stringn(inner.x, y, "─".repeat(width), width, Style::default().fg(theme.border)).0,
                Line::Row(i) => {
                    let (_, label, item) = ROWS[i];
                    let row = Rect::new(inner.x, y, inner.width, 1);
                    buf.set_stringn(inner.x + 2, y, label, width.saturating_sub(2), Style::default().fg(theme.text));
                    let value = self.value(item);
                    let vw = value.chars().count() as u16;
                    let vx = inner.right().saturating_sub(vw);
                    if vx > inner.x + 2 {
                        buf.set_stringn(vx, y, &value, vw as usize, Style::default().fg(theme.muted));
                    }
                    if i == self.selected {
                        buf.set_style(row, theme.cursor());
                    }
                    rows.push((row, i));
                    0
                }
            };
        }
        rows
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent, cfg: &mut Config) -> SettingsOutcome {
        // Settings has no text field, so every key here is a command.
        let k = crate::keys::normalize(k);
        KEYS.iter()
            .find(|r| r.code == k.code)
            .map_or(SettingsOutcome::None, |r| (r.act)(self, cfg))
    }

    /// `rows`: where the last frame's `draw` put them.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, cfg: &mut Config, rows: &[(Rect, usize)]) -> SettingsOutcome {
        use crossterm::event::MouseEventKind::*;
        let at = row_at(rows, ev.column, ev.row);
        match ev.kind {
            Moved => {
                if let Some(i) = at {
                    self.selected = i;
                }
                SettingsOutcome::None
            }
            Down(button) => {
                let r = self.rect(screen);
                if !r.contains((ev.column, ev.row).into()) || crate::chrome::close_rect(r).contains((ev.column, ev.row).into()) {
                    return SettingsOutcome::Close;
                }
                let delta = match button {
                    crossterm::event::MouseButton::Left => 1,
                    crossterm::event::MouseButton::Right => -1,
                    _ => return SettingsOutcome::None,
                };
                match at {
                    Some(i) => {
                        self.selected = i;
                        self.apply(delta, cfg)
                    }
                    None => SettingsOutcome::None,
                }
            }
            ScrollUp => {
                self.step(-3);
                SettingsOutcome::None
            }
            ScrollDown => {
                self.step(3);
                SettingsOutcome::None
            }
            _ => SettingsOutcome::None,
        }
    }

    /// Selects the row with this label.
    #[cfg(test)]
    pub fn choose(&mut self, label: &str) {
        self.selected = ROWS.iter().position(|r| r.1 == label).expect("a row with that label");
    }

    #[cfg(test)]
    pub fn hook_row_value(&self) -> String {
        self.value(Item::Hook)
    }

    fn value(&self, item: Item) -> String {
        let c = &self.cfg;
        match item {
            Item::Scrollbar => on_off(c.scrollbar),
            Item::Clock => match c.clock {
                crate::config::Clock::Off => "off".into(),
                crate::config::Clock::Time => "time".into(),
                crate::config::Clock::Date => "date".into(),
            },
            Item::Battery => match c.battery {
                crate::config::Battery::Off => "off".into(),
                crate::config::Battery::Percent => "percent".into(),
                crate::config::Battery::Time => "time".into(),
            },
            Item::RestoreScreens => on_off(c.restore_screens),
            Item::Preview => on_off(c.preview),
            Item::Wrap => on_off(c.preview_wrap),
            Item::Theme => match c.theme {
                ThemeMode::Auto => "auto".into(),
                ThemeMode::Dark => "dark".into(),
                ThemeMode::Light => "light".into(),
            },
            Item::Paths => match c.paths {
                PathStyle::Home => "from home".into(),
                PathStyle::Full => "in full".into(),
            },
            Item::Images => match c.images {
                ImageMode::Auto => "auto".into(),
                ImageMode::Blocks => "blocks".into(),
                ImageMode::Off => "off".into(),
            },
            Item::Tmux => match c.tmux {
                TmuxOffer::Ask => "ask".into(),
                TmuxOffer::NewWindow => "new window".into(),
                TmuxOffer::Stay => "stay".into(),
            },
            Item::Prefix => c.prefix.label().into(),
            Item::NotifySound => on_off(c.notify.sound),
            Item::NotifyBell => on_off(c.notify.bell),
            Item::PaneBorder => match c.pane_border {
                BorderMode::Always => "always".into(),
                BorderMode::Multiple => "multiple".into(),
                BorderMode::Never => "never".into(),
            },
            Item::SidebarWidth => c.sidebar_width.to_string(),
            Item::TreeWidth => c.tree_width.to_string(),
            Item::PreviewWidth => c.preview_width.to_string(),
            Item::Scrollback => c.scrollback.to_string(),
            Item::PaneGap => c.pane_gap.to_string(),
            Item::EditConfig => String::new(),
            Item::Hook => if self.hook_installed { "installed" } else { "not installed" }.into(),
            Item::Messaging => on_off(crate::policy::current().on),
            Item::PanesStop => if crate::policy::current().allow_from_panes { "yes" } else { "no" }.into(),
            Item::Topology | Item::Accounts => String::new(),
        }
    }

    fn apply(&mut self, delta: i32, cfg: &mut Config) -> SettingsOutcome {
        self.message = None;
        match ROWS[self.selected].2 {
            Item::Scrollbar => cfg.scrollbar = !cfg.scrollbar,
            Item::Clock => {
                use crate::config::Clock::{Date, Off, Time};
                const WAYS: [crate::config::Clock; 3] = [Off, Time, Date];
                let now = WAYS.iter().position(|w| *w == cfg.clock).unwrap_or(0) as i32;
                cfg.clock = WAYS[(now + delta).rem_euclid(3) as usize];
            }
            Item::Battery => {
                use crate::config::Battery::{Off, Percent, Time};
                const WAYS: [crate::config::Battery; 3] = [Off, Percent, Time];
                let now = WAYS.iter().position(|w| *w == cfg.battery).unwrap_or(0) as i32;
                cfg.battery = WAYS[(now + delta).rem_euclid(3) as usize];
            }
            Item::RestoreScreens => cfg.restore_screens = !cfg.restore_screens,
            Item::Preview => cfg.preview = !cfg.preview,
            Item::Wrap => cfg.preview_wrap = !cfg.preview_wrap,
            Item::Theme => {
                let i = THEMES.iter().position(|m| *m == cfg.theme).unwrap_or(0) as i32;
                cfg.theme = THEMES[(i + delta).rem_euclid(THEMES.len() as i32) as usize];
            }
            Item::Paths => cfg.paths = if cfg.paths == PathStyle::Home { PathStyle::Full } else { PathStyle::Home },
            Item::Images => {
                let i = IMAGES.iter().position(|m| *m == cfg.images).unwrap_or(0) as i32;
                cfg.images = IMAGES[(i + delta).rem_euclid(IMAGES.len() as i32) as usize];
            }
            Item::Tmux => {
                let i = TMUX.iter().position(|m| *m == cfg.tmux).unwrap_or(0) as i32;
                cfg.tmux = TMUX[(i + delta).rem_euclid(TMUX.len() as i32) as usize];
            }
            Item::Prefix => {
                let i = PREFIXES.iter().position(|m| *m == cfg.prefix).unwrap_or(0) as i32;
                cfg.prefix = PREFIXES[(i + delta).rem_euclid(PREFIXES.len() as i32) as usize];
            }
            Item::NotifySound => cfg.notify.sound = !cfg.notify.sound,
            Item::NotifyBell => cfg.notify.bell = !cfg.notify.bell,
            Item::PaneBorder => {
                let i = BORDERS.iter().position(|b| *b == cfg.pane_border).unwrap_or(0) as i32;
                cfg.pane_border = BORDERS[(i + delta).rem_euclid(BORDERS.len() as i32) as usize];
            }
            // Step of two cells: at one per step, crossing a width takes forever.
            Item::SidebarWidth => cfg.sidebar_width = bump(cfg.sidebar_width, delta * 2, SIDEBAR_WIDTH),
            Item::TreeWidth => cfg.tree_width = bump(cfg.tree_width, delta * 2, TREE_WIDTH),
            Item::PreviewWidth => cfg.preview_width = bump(cfg.preview_width, delta * 2, PREVIEW_WIDTH),
            Item::PaneGap => cfg.pane_gap = bump(cfg.pane_gap, delta, PANE_GAP),
            Item::Scrollback => cfg.scrollback = snap_history(cfg.scrollback, delta),
            Item::Hook => return self.toggle_hook(),
            Item::EditConfig => return SettingsOutcome::EditConfig,
            Item::Messaging => return SettingsOutcome::Mail(crate::app::MailAction::SetOn(!crate::policy::current().on)),
            Item::PanesStop => return SettingsOutcome::Mail(crate::app::MailAction::SetAllowFromPanes(!crate::policy::current().allow_from_panes)),
            Item::Topology => return SettingsOutcome::Topology,
            Item::Accounts => return SettingsOutcome::Accounts,
        }
        self.cfg = cfg.clone();
        SettingsOutcome::Changed
    }

    /// Both ways ask first when a person has not yet read what changes; a hooked agent is removed even if its resume line changed.
    fn toggle_hook(&mut self) -> SettingsOutcome {
        if self.hook_installed {
            return SettingsOutcome::RemoveHook;
        }
        if !crate::hook::first_installs().is_empty() {
            return SettingsOutcome::InstallHook;
        }
        match crate::hook::install() {
            Ok(done) if done.iter().any(|(_, r)| r.is_ok()) => self.hook_installed = true,
            Ok(done) => self.message = Some((true, done.iter().filter_map(|(_, r)| r.as_ref().err().cloned()).collect::<Vec<_>>().join("; "))),
            Err(e) => self.message = Some((true, format!("couldn't install the hook: {e}"))),
        }
        // The hook lives in ~/.claude, not in the config: nothing for the caller to save.
        SettingsOutcome::None
    }

    fn step(&mut self, delta: isize) {
        let n = ROWS.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

}

fn on_off(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

/// Clamps rather than wraps - wrapping from 48 to 20 would look like a slider gone loose.
fn bump(v: u16, delta: i32, (lo, hi): (u16, u16)) -> u16 {
    (v as i32 + delta).clamp(lo as i32, hi as i32) as u16
}

/// The next round number: thousands up to ten thousand, ten thousands above, and the floor below the first thousand.
fn snap_history(v: usize, delta: i32) -> usize {
    let (lo, hi) = SCROLLBACK;
    let step = |v: usize| if v < 10_000 { 1_000 } else { 10_000 };
    let next = if delta > 0 {
        if v < 1_000 { 1_000 } else { (v / step(v) + 1) * step(v) }
    } else if v <= 1_000 {
        lo
    } else {
        // 10000 itself steps down by a thousand
        let s = step(v - 1);
        (v - 1) / s * s
    };
    next.clamp(lo, hi)
}

fn hint_strip(width: usize) -> Vec<String> {
    crate::chrome::hint(KEYS.iter().map(|k| (k.label, k.word)), width)
}

/// What the message line says under a selected row.
fn hint(item: Item) -> Option<&'static str> {
    match item {
        Item::Scrollback => Some(SCROLLBACK_HINT),
        Item::EditConfig => Some(CONFIG_HINT),
        _ => None,
    }
}

#[derive(PartialEq)]
enum Line {
    Head(&'static str),
    Sep,
    Row(usize),
}

/// The list top to bottom: section headers, rows, and a rule before the row that leaves the screen.
fn lines() -> Vec<Line> {
    let mut out = Vec::new();
    for (i, (header, _, item)) in ROWS.iter().enumerate() {
        if let Some(h) = header {
            out.push(Line::Head(h));
        }
        if *item == Item::EditConfig {
            out.push(Line::Sep);
        }
        out.push(Line::Row(i));
    }
    out
}

/// The row drawn at a cell, by its index.
fn row_at(rows: &[(Rect, usize)], col: u16, row: u16) -> Option<usize> {
    rows.iter().find(|(rect, _)| rect.contains((col, row).into())).map(|(_, i)| *i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn theme() -> Theme {
        Theme::dark()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    fn at(item: Item, cfg: &Config) -> Settings {
        let mut s = Settings::new(cfg);
        s.selected = ROWS.iter().position(|r| r.2 == item).expect("item is in the table");
        s
    }

    #[test]
    fn enter_flips_a_toggle_in_the_config() {
        let mut cfg = Config::default();
        assert!(cfg.scrollbar);

        let mut s = at(Item::Scrollbar, &cfg);
        assert_eq!(s.key(key(KeyCode::Enter), &mut cfg), SettingsOutcome::Changed);
        assert!(!cfg.scrollbar);
        assert_eq!(s.value(Item::Scrollbar), "off", "the screen's copy updated along with the config");

        s.key(key(KeyCode::Char(' ')), &mut cfg);
        assert!(cfg.scrollbar);
    }

    #[test]
    fn every_notify_switch_has_its_own_field() {
        let mut cfg = Config::default();
        for (item, get) in [
            (Item::NotifySound, (|c: &Config| c.notify.sound) as fn(&Config) -> bool),
            (Item::NotifyBell, |c: &Config| c.notify.bell),
        ] {
            let before = get(&cfg);
            at(item, &cfg).key(key(KeyCode::Enter), &mut cfg);
            assert_ne!(get(&cfg), before, "{item:?}");
        }
    }

    #[test]
    fn widths_stop_at_the_bounds_without_wrapping() {
        let mut cfg = Config::default();
        let mut s = at(Item::SidebarWidth, &cfg);
        for _ in 0..40 {
            s.key(key(KeyCode::Right), &mut cfg);
        }
        assert_eq!(cfg.sidebar_width, 48);
        for _ in 0..40 {
            s.key(key(KeyCode::Left), &mut cfg);
        }
        assert_eq!(cfg.sidebar_width, 20);

        let mut s = at(Item::TreeWidth, &cfg);
        for _ in 0..60 {
            s.key(key(KeyCode::Left), &mut cfg);
        }
        assert_eq!(cfg.tree_width, 20);
        for _ in 0..60 {
            s.key(key(KeyCode::Right), &mut cfg);
        }
        assert_eq!(cfg.tree_width, 80);

        let mut s = at(Item::PaneGap, &cfg);
        for _ in 0..9 {
            s.key(key(KeyCode::Right), &mut cfg);
        }
        assert_eq!(cfg.pane_gap, 3);
        for _ in 0..9 {
            s.key(key(KeyCode::Left), &mut cfg);
        }
        assert_eq!(cfg.pane_gap, 0);
    }

    #[test]
    fn the_clock_cycles_off_time_date() {
        use crate::config::Clock::{Date, Off, Time};
        let mut cfg = Config::default();
        let mut s = at(Item::Clock, &cfg);
        assert_eq!(cfg.clock, Time);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.clock, Date);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.clock, Off);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.clock, Time, "the cycle closed");
    }

    #[test]
    fn the_battery_cycles_off_percent_time() {
        use crate::config::Battery::{Off, Percent, Time};
        let mut cfg = Config::default();
        let mut s = at(Item::Battery, &cfg);
        assert_eq!(cfg.battery, Percent);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.battery, Time);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.battery, Off);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.battery, Percent, "the cycle closed");
    }

    #[test]
    fn pane_border_cycles_through_every_mode_both_ways() {
        let mut cfg = Config::default();
        let mut s = at(Item::PaneBorder, &cfg);
        assert_eq!(cfg.pane_border, BorderMode::Always);

        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.pane_border, BorderMode::Multiple);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.pane_border, BorderMode::Never);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.pane_border, BorderMode::Always, "the cycle closed");

        s.key(key(KeyCode::Left), &mut cfg);
        assert_eq!(cfg.pane_border, BorderMode::Never, "left goes the other way");
    }

    #[test]
    fn paths_switch_between_from_home_and_in_full_and_are_saved_by_name() {
        let mut cfg = Config::default();
        let mut s = at(Item::Paths, &cfg);
        assert_eq!(s.value(Item::Paths), "from home");
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!((cfg.paths, s.value(Item::Paths).as_str()), (PathStyle::Full, "in full"));
        assert!(serde_json::to_string(&cfg).unwrap().contains(r#""paths":"full""#));
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.paths, PathStyle::Home);
    }

    #[test]
    fn preview_toggles_and_images_cycle_both_ways() {
        let mut cfg = Config::default();
        assert!(cfg.preview);
        let mut s = at(Item::Preview, &cfg);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert!(!cfg.preview);
        assert_eq!(s.value(Item::Preview), "off");

        let mut s = at(Item::Images, &cfg);
        assert_eq!(cfg.images, ImageMode::Auto);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.images, ImageMode::Blocks);
        assert_eq!(s.value(Item::Images), "blocks");
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.images, ImageMode::Off);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.images, ImageMode::Auto, "the cycle closed");
        s.key(key(KeyCode::Left), &mut cfg);
        assert_eq!(cfg.images, ImageMode::Off, "left goes the other way");

        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""images":"off""#), "{json}");
        let old: Config = serde_json::from_str("{}").unwrap();
        assert!(old.preview && old.images == ImageMode::Auto, "an old config gets the defaults");
    }

    #[test]
    fn the_tmux_offer_cycles_both_ways() {
        let mut cfg = Config::default();
        assert_eq!(cfg.tmux, TmuxOffer::Ask);
        let mut s = at(Item::Tmux, &cfg);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.tmux, TmuxOffer::NewWindow);
        assert_eq!(s.value(Item::Tmux), "new window");
        s.key(key(KeyCode::Left), &mut cfg);
        assert_eq!(cfg.tmux, TmuxOffer::Ask, "left goes the other way");

        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""tmux":"ask""#), "{json}");
        cfg.tmux = TmuxOffer::NewWindow;
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""tmux":"new_window""#), "{json}");
        let old: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(old.tmux, TmuxOffer::Ask, "an old config gets the default");
    }

    #[test]
    fn the_prefix_is_off_until_chosen_and_cycles() {
        let mut cfg = Config::default();
        assert_eq!(cfg.prefix, Prefix::Off);
        let mut s = at(Item::Prefix, &cfg);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(s.value(Item::Prefix), "⌃+B");
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.prefix, Prefix::CtrlA);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.prefix, Prefix::Off, "the cycle closed");

        cfg.prefix = Prefix::CtrlB;
        assert!(serde_json::to_string(&cfg).unwrap().contains(r#""prefix":"ctrl_b""#));
        let old: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(old.prefix, Prefix::Off, "an old config gets the default");
    }

    #[test]
    fn arrows_move_among_the_rows_and_wrap() {
        let mut cfg = Config::default();
        let mut s = Settings::new(&cfg);
        assert_eq!(s.selected, 0);
        s.key(key(KeyCode::Down), &mut cfg);
        assert_eq!(s.selected, 1);
        s.key(key(KeyCode::Up), &mut cfg);
        assert_eq!(s.selected, 0);
        s.key(key(KeyCode::Up), &mut cfg);
        assert_eq!(s.selected, ROWS.len() - 1, "wraps at the top");
        s.key(key(KeyCode::Down), &mut cfg);
        assert_eq!(s.selected, 0, "wraps at the bottom");
    }

    #[test]
    fn esc_closes_the_screen() {
        let mut cfg = Config::default();
        let mut s = Settings::new(&cfg);
        assert_eq!(s.key(key(KeyCode::Esc), &mut cfg), SettingsOutcome::Close);
    }

    #[test]
    fn a_click_on_a_row_selects_and_toggles_it() {
        let mut cfg = Config::default();
        let screen = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(screen);
        let mut s = Settings::new(&cfg);
        let rows = s.draw(&mut buf, screen, &theme());

        let (rect, i) = *rows.iter().find(|(_, i)| ROWS[*i].2 == Item::SidebarWidth).unwrap();
        let before = cfg.sidebar_width;
        let click = mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 1, rect.y);
        assert_eq!(s.mouse(click, screen, &mut cfg, &rows), SettingsOutcome::Changed);
        assert_eq!(s.selected, i);
        assert_eq!(cfg.sidebar_width, before + 2);

        let back = mouse(MouseEventKind::Down(MouseButton::Right), rect.x + 1, rect.y);
        assert_eq!(s.mouse(back, screen, &mut cfg, &rows), SettingsOutcome::Changed);
        assert_eq!(cfg.sidebar_width, before, "a right click steps back");

        assert_eq!(
            s.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), screen, &mut cfg, &rows),
            SettingsOutcome::Close
        );
    }

    #[test]
    fn the_wheel_walks_the_selection() {
        let mut cfg = Config::default();
        let screen = Rect::new(0, 0, 60, 30);
        let mut s = Settings::new(&cfg);
        s.mouse(mouse(MouseEventKind::ScrollDown, 30, 15), screen, &mut cfg, &[]);
        assert_eq!(s.selected, 3);
        s.mouse(mouse(MouseEventKind::ScrollUp, 30, 15), screen, &mut cfg, &[]);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn the_theme_cycles_auto_dark_light() {
        let mut cfg = Config::default();
        let mut s = at(Item::Theme, &cfg);
        assert_eq!(cfg.theme, ThemeMode::Auto);
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!((cfg.theme, s.value(Item::Theme).as_str()), (ThemeMode::Dark, "dark"));
        s.key(key(KeyCode::Enter), &mut cfg);
        assert_eq!(cfg.theme, ThemeMode::Light);
        s.key(key(KeyCode::Left), &mut cfg);
        assert_eq!(cfg.theme, ThemeMode::Dark, "left goes back");
    }

    #[test]
    fn every_row_is_drawn_with_its_value() {
        let screen = Rect::new(0, 0, 80, 52);
        let mut buf = Buffer::empty(screen);
        let s = Settings::new(&Config::default());
        let rows = s.draw(&mut buf, screen, &theme());
        assert_eq!(rows.len(), ROWS.len(), "every row fit");

        let (rect, _) = rows[0];
        let line: String = (rect.x..rect.right()).map(|x| buf[(x, rect.y)].symbol()).collect();
        assert!(line.contains("Theme"), "{line:?}");
        assert!(line.trim_end().ends_with("auto"), "{line:?}");
    }

    #[test]
    fn the_cursor_is_the_accent_bar_and_values_stay_muted() {
        let cfg = Config::default();
        let s = at(Item::Prefix, &cfg);
        let screen = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let rows = s.draw(&mut buf, screen, &th);
        let (cur, _) = *rows.iter().find(|(_, i)| *i == s.selected).unwrap();
        assert!((cur.x..cur.right()).all(|x| buf[(x, cur.y)].bg == th.accent && buf[(x, cur.y)].fg == th.ground),
            "the whole row, its value included");
        let (other, _) = rows[0];
        assert_eq!(buf[(other.right() - 1, other.y)].fg, th.muted, "a value off the cursor is muted");
        assert_eq!(buf[(other.x + 2, other.y)].fg, th.text, "and its label plain text");
    }

    #[test]
    fn hints_fit_the_panel() {
        let inner = Settings::new(&Config::default()).rect(Rect::new(0, 0, 200, 60)).width - 2 * crate::chrome::DIALOG_FRAME.horizontal;
        for hint in [SCROLLBACK_HINT, CONFIG_HINT] {
            let w = hint.chars().count() as u16;
            assert!(w <= inner, "{hint:?} is {w} cells, and the panel holds {inner}; it would be cut mid-word");
        }
    }

    fn text_of(buf: &Buffer, rect: Rect) -> String {
        (rect.x..rect.right()).map(|x| buf[(x, rect.y)].symbol()).collect()
    }

    #[test]
    fn the_screen_reads_as_sentences_in_this_order() {
        // Messaging's rows read the policy, which tests that change it hold this for
        let _policy = crate::policy::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let screen = Rect::new(0, 0, 80, 52);
        let mut buf = Buffer::empty(screen);
        let mut cfg = Config::default();
        cfg.notify.bell = true;
        let s = Settings::new(&cfg);
        let rows = s.draw(&mut buf, screen, &theme());
        let text: Vec<String> = (0..screen.height).map(|y| text_of(&buf, Rect::new(0, y, 80, 1))).collect();
        assert!(text.iter().any(|l| l.contains("Settings")));
        let shown: Vec<(String, String)> = rows
            .iter()
            .map(|(r, _)| {
                let l = text_of(&buf, *r);
                let l = l.trim();
                let (label, value) = l.split_at(l.find("  ").unwrap_or(l.len()));
                (label.trim_start_matches('│').trim().to_string(), value.trim().trim_end_matches('│').trim().to_string())
            })
            .collect();
        let want = [
            ("Theme", "auto"), ("Pane borders", "always"), ("Gap between panes", "1"), ("Scrollbars", "on"), ("Clock in the top row", "time"), ("Battery in the top row", "percent"),
            ("Sidebar width", "34"), ("File tree width", "34"), ("Preview width", "60"),
            ("File preview", "on"), ("Wrap long lines", "off"), ("Images", "auto"), ("Show paths", "from home"),
            ("Play a sound", "on"), ("Ring the terminal bell", "on"),
            ("Inside tmux", "ask"), ("tmux-style prefix", "off"), ("History lines per pane", "10000"), ("Restore screens after a restart", "on"),
            ("Messaging", "on"), ("Programs in panes may stop or update nerd", "no"), ("Set up a topology…", ""), ("Accounts…", ""),
            ("Agent hooks", if s.hook_installed { "installed" } else { "not installed" }), ("Edit config.json…", ""),
        ];
        assert_eq!(shown.len(), want.len());
        for (got, want) in shown.iter().zip(want) {
            assert_eq!((got.0.as_str(), got.1.as_str()), want);
        }
        let heads: Vec<&str> = ["Look", "Layout", "Files", "When an agent needs you", "Terminal", "Messages", "Agents"].into();
        let at: Vec<usize> = heads.iter().map(|h| text.iter().position(|l| l.trim_matches(|c| c == '│' || c == ' ') == *h).expect(h)).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        assert!(text.iter().any(|l| l.contains("──────")), "a rule sits above Edit config.json");
    }

    #[test]
    fn every_value_cycles_through_the_values_it_shows() {
        let mut cfg = Config::default();
        let walk = |item, cfg: &mut Config| {
            let mut s = at(item, cfg);
            let mut seen = vec![s.value(item)];
            for _ in 0..3 {
                s.key(key(KeyCode::Right), cfg);
                seen.push(s.value(item));
            }
            seen
        };
        assert_eq!(walk(Item::Theme, &mut cfg), ["auto", "dark", "light", "auto"]);
        assert_eq!(walk(Item::PaneBorder, &mut cfg), ["always", "multiple", "never", "always"]);
        assert_eq!(walk(Item::PaneGap, &mut cfg), ["1", "2", "3", "3"]);
        assert_eq!(walk(Item::Images, &mut cfg), ["auto", "blocks", "off", "auto"]);
        assert_eq!(walk(Item::Tmux, &mut cfg), ["ask", "new window", "stay", "ask"]);
        assert_eq!(walk(Item::Prefix, &mut cfg), ["off", "⌃+B", "⌃+A", "off"]);
        assert_eq!(walk(Item::Paths, &mut cfg), ["from home", "in full", "from home", "in full"]);
    }

    #[test]
    fn preview_width_and_history_have_their_own_ranges_and_steps() {
        let mut cfg = Config::default();
        let mut s = at(Item::PreviewWidth, &cfg);
        for _ in 0..80 {
            s.key(key(KeyCode::Right), &mut cfg);
        }
        assert_eq!(cfg.preview_width, 120);
        for _ in 0..80 {
            s.key(key(KeyCode::Left), &mut cfg);
        }
        assert_eq!(cfg.preview_width, 30);

        let mut s = at(Item::Scrollback, &cfg);
        let mut walk = |delta: KeyCode, cfg: &mut Config, n| (0..n).map(|_| { s.key(key(delta), cfg); cfg.scrollback }).collect::<Vec<_>>();
        assert_eq!(walk(KeyCode::Right, &mut cfg, 3), [20_000, 30_000, 40_000]);
        assert_eq!(walk(KeyCode::Left, &mut cfg, 5), [30_000, 20_000, 10_000, 9_000, 8_000]);
        cfg.scrollback = 100;
        assert_eq!(walk(KeyCode::Left, &mut cfg, 1), [100], "the floor holds");
        assert_eq!(walk(KeyCode::Right, &mut cfg, 1), [1_000], "from the floor up goes to a thousand");
        assert_eq!(walk(KeyCode::Left, &mut cfg, 1), [100], "and a thousand down goes back to the floor");
        cfg.scrollback = 10_500;
        assert_eq!(walk(KeyCode::Left, &mut cfg, 1), [10_000], "a value from the file snaps to a round one");
        cfg.scrollback = 12_345;
        assert_eq!(walk(KeyCode::Right, &mut cfg, 1), [20_000]);
        cfg.scrollback = 2_500;
        assert_eq!(walk(KeyCode::Right, &mut cfg, 1), [3_000]);
        cfg.scrollback = 100_000;
        assert_eq!(walk(KeyCode::Right, &mut cfg, 1), [100_000], "the ceiling holds");
        assert_eq!(at(Item::Scrollback, &cfg).value(Item::Scrollback), "100000");
    }

    #[test]
    fn every_limit_is_one_number_for_settings_the_drag_and_the_file() {
        use crate::config::{PANE_GAP, PREVIEW_WIDTH, SIDEBAR_WIDTH, TREE_WIDTH};
        assert_eq!((SIDEBAR_WIDTH, TREE_WIDTH, PREVIEW_WIDTH, PANE_GAP), ((20, 48), (20, 80), (30, 120), (0, 3)));
        let mut cfg = Config::default();
        for (item, get, lo, hi) in [
            (Item::SidebarWidth, (|c: &Config| c.sidebar_width) as fn(&Config) -> u16, SIDEBAR_WIDTH.0, SIDEBAR_WIDTH.1),
            (Item::TreeWidth, |c| c.tree_width, TREE_WIDTH.0, TREE_WIDTH.1),
            (Item::PreviewWidth, |c| c.preview_width, PREVIEW_WIDTH.0, PREVIEW_WIDTH.1),
        ] {
            let mut s = at(item, &cfg);
            for _ in 0..100 {
                s.key(key(KeyCode::Left), &mut cfg);
            }
            assert_eq!(get(&cfg), lo, "{item:?}");
            for _ in 0..100 {
                s.key(key(KeyCode::Right), &mut cfg);
            }
            assert_eq!(get(&cfg), hi, "{item:?}");
        }
        let wild = Config { sidebar_width: 1, tree_width: 1, preview_width: 500, scrollback: 5, ..Config::default() }.clamped();
        assert_eq!((wild.sidebar_width, wild.tree_width, wild.preview_width, wild.scrollback), (20, 20, 120, 100));
    }

    #[test]
    fn the_message_line_explains_the_selected_row() {
        let screen = Rect::new(0, 0, 100, 40);
        let cfg = Config::default();
        for (item, want) in [(Item::Scrollback, SCROLLBACK_HINT), (Item::EditConfig, CONFIG_HINT)] {
            let mut buf = Buffer::empty(screen);
            let s = at(item, &cfg);
            s.draw(&mut buf, screen, &theme());
            let all: String = (0..screen.height).map(|y| text_of(&buf, Rect::new(0, y, 100, 1))).collect::<Vec<_>>().join("\n");
            assert!(all.contains(want), "{item:?}");
        }
    }

    #[test]
    fn a_hooked_agent_asks_to_be_unhooked_and_edit_config_leaves_the_screen() {
        let mut cfg = Config::default();
        let mut s = at(Item::Hook, &cfg);
        s.hook_installed = true;
        assert_eq!(s.key(key(KeyCode::Enter), &mut cfg), SettingsOutcome::RemoveHook);
        let mut s = at(Item::EditConfig, &cfg);
        assert_eq!(s.key(key(KeyCode::Enter), &mut cfg), SettingsOutcome::EditConfig);
    }

    #[test]
    fn a_short_screen_scrolls_to_the_selected_row() {
        let screen = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(screen);
        let s = at(Item::EditConfig, &Config::default());
        let rows = s.draw(&mut buf, screen, &theme());
        assert!(rows.iter().any(|(_, i)| *i == ROWS.len() - 1), "the last row is on screen");
    }

    #[test]
    fn tiny_screen_does_not_panic() {
        for (w, h) in [(1, 1), (3, 2), (10, 4), (46, 3)] {
            let screen = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(screen);
            let mut s = Settings::new(&Config::default());
            s.message = Some((true, "an error wider than the window itself, so it gets truncated".to_string()));
            s.draw(&mut buf, screen, &theme());
        }
    }
}
