//! Application state and the event loop.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use crate::config::Config;
use crate::keys::{global_label, global_of, Bind, KeyAction, KeyActionName};
use crate::layout::{Dir, Layout, Nav, PaneId};
use crate::mouse::Router;
use crate::pty::Pty;
use crate::render::{pick_theme, Theme};
use crate::session::{PaneSnapshot, Saver, SpaceSnapshot, TabSnapshot, WorkspaceSnapshot};
use crate::term::{AgentState, Term};

pub const CFG_BROKEN: &str = "config.json does not parse; nerd keeps the last good settings and does not write the file until it parses";

mod commands;
mod deliver;
mod input;
mod lifecycle;
mod mail;
mod model;
mod remote;
#[cfg(test)]
pub(crate) mod snapshots;

pub use commands::*;
pub use lifecycle::*;
pub use mail::*;
pub use model::*;
pub use remote::*;

/// Where the keys go. Panes are the default; the rest are reached with control and an arrow,
/// and `esc` comes back. Overlays and the search bar sit on top of this rather than in it:
/// they are modes, not places on the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    #[default]
    Panes,
    Sidebar,
    Tabs,
    /// The file tree, or its preview; which of the two is the tree's own business.
    Tree,
}

/// A row of the sidebar the keys can sit on. Spaces and panes are drawn in one list, so the
/// keys walk one list too, in the order the rows appear on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarSel {
    Space(usize),
    Pane(PaneId),
}

/// A card that opens a window of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum More {
    /// A new build waits on disk: the update window.
    Update,
    /// This build just took over: what it brought.
    Updated,
}

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    /// The dark theme was picked: colours from the config may move its ground anywhere.
    pub theme_dark: bool,
    pub spaces: Vec<Space>,
    pub active: usize,
    pub router: Router,
    pub saver: Saver,
    pub sidebar: bool,
    /// Screen size from the last frame: menus need to know where they fit.
    pub screen: Rect,
    /// The open menu or input field. While it's open, input goes to it, not the pane.
    pub overlay: Overlay,
    /// Current mouse selection, if any. Lives until the next click.
    pub selection: Option<crate::select::Selection>,
    /// Word-drag anchor after a double-click: the selection grows whole words from here.
    word_anchor: Option<((u16, u16), (u16, u16))>,
    /// The line of a triple-click: a drag after it grows whole lines at a time.
    line_anchor: Option<u16>,
    /// Collapsed worktree groups, keyed by shared `.git` directory.
    pub collapsed_groups: HashSet<PathBuf>,
    /// Who reports `blocked`/`done` for panes out of focus.
    notifier: crate::notify::Notifier,
    /// Search within a pane: which one and what we're looking for. While open, keys belong to it.
    pub search: Option<(PaneId, crate::search::Search)>,
    search_at: Instant,
    /// When the branches, and the tree's marks, were last asked of git.
    git_at: Instant,
    /// When width was dragged: write to config after a pause.
    cfg_save_pending: Option<Instant>,
    /// Mtime of the config as we last wrote or read it, so an edit made outside nerd is noticed.
    cfg_mtime: Option<std::time::SystemTime>,
    cfg_poll_at: Instant,
    /// When plugins.lock and plugins-off.json last changed, as last seen.
    plugins_seen: [Option<std::time::SystemTime>; 2],
    /// Each pinned plugin's commit as last seen: new code under the same manifest restarts it too.
    plugins_commits: HashMap<String, String>,
    /// The Plugins window opens again with this plugin chosen once its remove question is answered.
    pub plugins_back: Option<String>,
    /// The git fetch the Plugins window started, while it runs and after it failed.
    pub plugin_job: Option<crate::plugin_list::Job>,
    /// Plugins a pane staged that the Plugins window has asked about.
    plugins_asked: HashSet<String>,
    /// The plugins off when plugins-off.json was last read.
    plugins_off: HashSet<String>,
    /// config.json did not parse: the last good settings stay and nothing writes the file.
    pub cfg_broken: bool,
    /// Right-hand file tree panel; None means hidden.
    pub tree: Option<crate::tree::Tree>,
    /// Which area has the keys. The tree is visible even when it does not.
    pub focus: Focus,
    /// The prefix chord was pressed and the next key is its second half; the top row says so.
    pub prefix: bool,
    /// The sidebar row the keys are on, while the sidebar has them.
    pub sidebar_sel: Option<SidebarSel>,
    /// The tree and its preview take the whole screen; panes are not drawn.
    pub tree_zoom: bool,
    /// What the last frame drew where; draw writes it, the mouse and the keys read it.
    pub view: ViewState,
    /// What git says about each space's folder; the draw only reads it.
    pub repos: HashMap<PathBuf, Repo>,
    /// Message at the bottom of the screen: what went wrong.
    pub notice: Option<String>,
    /// Neutral message ("copied"): not an error, no need to color it. It goes by itself after `TOAST`.
    pub toast: Option<String>,
    /// The toast on screen and since when.
    toast_since: Option<(String, Instant)>,
    /// Bytes for the client terminal, bypassing the frame (OSC 52). The daemon picks them up.
    pub to_client: Vec<u8>,
    /// The pointer shape last sent to the screen client; empty: none yet.
    pub pointer: String,
    /// Debug tracing to the log (NERD_TRACE).
    pub trace: bool,
    /// The client terminal answered the Kitty graphics query.
    pub graphics: bool,
    /// A client terminal reply (OSC/DCS/APC) arriving as keys: how much has been swallowed so far, for the log.
    reply: Option<(Instant, String)>,
    pub next_id: PaneId,
    /// Rung by every pane's reader; the daemon puts its own in before any pane starts.
    pub bell: crate::bell::Bell,
    /// Panes whose process left, and until when their last output may still come.
    exits: Vec<(PaneId, Instant)>,
    /// How each of those left, for the line that closes it.
    exit_how: HashMap<PaneId, String>,
    pub dirty: bool,
    /// Detach this client, and the log line that says why.
    pub quit: Option<&'static str>,
    /// The human asked for the new build: the daemon hands over and execs.
    pub upgrade: bool,
    /// The human chose Disconnect everyone: the daemon ends this client and every waiting socket.
    pub disconnect_all: bool,
    /// The human chose Stop this server: the daemon ends its loop as `nerd --stop` does.
    pub stop_server: bool,
    /// The daemon's sockets that have not attached yet, counted in that dialog.
    pub waiting: crate::transport::Waiting,
    /// A newer binary sits at the daemon's path; shown until the swap.
    pub update_available: bool,
    /// When a frame last went out to move the sidebar's wait times on.
    wait_tick: Instant,
    /// Agents whose subagent list the human opened in the sidebar.
    pub subagents_open: std::collections::HashSet<PaneId>,
    /// A subagent's run, shown as a section of its agent's pane until closed or the pane goes.
    pub run: Option<crate::run::Run>,
    /// The section was opened or clicked last: Esc closes it rather than reaching the pane.
    run_keys: bool,
    /// The section takes the whole screen, as the file tree does.
    pub run_zoom: bool,
    /// Each pane's section width, once its seam was dragged; for this session only.
    pub run_widths: HashMap<PaneId, u16>,
    /// This server's state lives where a plain `nerd --resume` looks for it, so a window
    /// opened from here can reach it. Read once: the environment does not change under us.
    default_state_root: bool,
    /// The client already offered a window outside tmux; one offer per client.
    offered: Option<u64>,
    /// The `open` process started by the offer, until a client attaches or the wait runs out.
    pub opening: Option<Opening>,
    /// The pane of the space a failed spawn opened in place of the last one.
    fresh: Option<PaneId>,
    /// Its version line, as printed by `nerd --version`, for the dialog.
    pub update_info: String,
    /// What that build brings over this one, as rows for the update window.
    pub update_changes: Vec<Vec<String>>,
    /// The human skipped that build: no card and no mark for it.
    pub update_skipped: bool,
    /// After an update: what this build brought over the one before; None when that build did not say which it was.
    pub updated_changes: Option<Vec<Vec<String>>>,
    /// What the card's "… more" opens, while the card still shows this text.
    pub toast_more: Option<(String, More)>,
    /// Clients attached since the server started, newest first.
    pub clients: Vec<crate::clients::ClientRecord>,
    /// The attached client's id: a takeover reads the caps of the one it replaces.
    pub client: u64,
    /// What the attached client can do.
    pub caps: crate::proto::ClientCaps,
    /// The machine Connect to... asked the client about, and until when an answer counts.
    remote_ask: Option<(String, Instant)>,
    /// What runs in each pane, from the process table, and what its agent's hook wrote.
    probe: crate::worker::Worker<Vec<ProbeAsk>, Vec<Probed>>,
    probed_at: Instant,
    /// Tails the open run's transcript on the probe's tick.
    transcripts: crate::worker::Worker<crate::transcript::Ask, crate::transcript::Found>,
    git: crate::worker::Worker<GitAsk, GitFound>,
    /// Loads the tree's preview and makes what its images need.
    preview: crate::worker::Worker<PreviewAsk, PreviewFound>,
    pub plugins: crate::plugins::Plugins,
    /// The built-in sources run; tests that must draw exactly what master drew leave them off.
    pub builtins: bool,
    /// The menu bar's time, or its date and time, drawn at the right of the top row.
    pub clock: String,
    /// The minute (or second) and the form the clock was last written for; it is written again only when one changes.
    clock_key: Option<(i64, crate::config::Clock, bool)>,
    /// The battery's charge before the clock, None when off or with no battery.
    pub battery: Option<crate::battery::Charge>,
    /// When the pointer last moved over the battery: its details show under it for `HOVER`.
    pub battery_hover: Option<Instant>,
    /// The critical charge was told once; told again only after the battery left it.
    battery_warned: bool,
    /// The preview last asked of a plugin or a public built-in, and the number the next request takes.
    preview_asked: Option<Asked>,
    /// The view of its previews the user chose last, per plugin: asked for again with every file.
    preview_modes: HashMap<String, String>,
    preview_req: u64,
    /// Messages between agents: the live records and what delivery keeps.
    pub mail: MailState,
    /// A window attached from inside one of this server's panes.
    pub client_inside: bool,
}

/// A space's folder as git sees it: the group key never changes for a folder, the branch does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Repo {
    /// Checkouts of one repository share it, which is how the sidebar groups them.
    pub key: Option<PathBuf>,
    /// The main checkout, not a linked worktree: it heads its group.
    pub main: bool,
    pub branch: Option<String>,
}

/// The last frame's geometry and what it still owes the terminal: written only by draw.
#[derive(Default)]
pub struct ViewState {
    pub hits: crate::mouse::Hits,
    /// Where the logo image stands on the client's screen, in which theme and from which raster: (x, y, dark, height). None: not shown.
    pub logo_at: Option<(u16, u16, bool, u16)>,
    /// Where the tree was in the last frame, for mouse routing.
    pub tree_rect: Option<Rect>,
    /// The frame around both halves: the title row carries □ and ×, so the mouse is routed by it.
    pub tree_frame: Option<Rect>,
    /// Preview column to the left of the tree, if it fit.
    pub preview_rect: Option<Rect>,
    pub tree: crate::tree::TreeView,
    /// The subagent section's place and buttons.
    pub run: crate::run::RunView,
    /// Where the open dialog put its rows and buttons.
    pub dialog: DialogView,
    /// The sidebar's needs-you rows for mail, by message id: a click opens Messages on it.
    pub sidebar_mail: Vec<(Rect, String)>,
}

/// A dialog's rows and buttons from the last frame, for its mouse; each dialog fills what it has.
#[derive(Default)]
pub struct DialogView {
    /// A confirm's buttons, or a picker's rows.
    pub rects: Vec<Rect>,
    /// Rows with what each one stands for.
    pub rows: Vec<(Rect, usize)>,
    /// Where the open panel put its rows and close button: Session history or a plugin's.
    pub panel: crate::panel::Drawn,
    /// The Messages window: cards, headers, lines and buttons.
    pub messages: crate::messages::Drawn<crate::messages::Click>,
    /// The Isolation and Accounts windows.
    pub isolation: crate::messages::Drawn<crate::isolation_ui::Click>,
}

impl App {
    #[cfg(test)]
    pub fn new(cfg: Config) -> Self {
        Self { builtins: false, ..Self::with_bell(cfg, crate::bell::new().0) }
    }

    /// `bell` is the loop's: every pane's reader and every worker rings it.
    pub fn with_bell(cfg: Config, bell: crate::bell::Bell) -> Self {
        let (theme, theme_dark) = pick_theme(&cfg, None);
        Self {
            theme,
            theme_dark,
            sidebar: cfg.sidebar_visible,
            screen: Rect::new(0, 0, 80, 24),
            overlay: Overlay::None,
            selection: None,
            notice: None,
            toast: None,
            to_client: Vec::new(),
            pointer: String::new(),
            trace: false,
            graphics: false,
            reply: None,
            word_anchor: None,
            line_anchor: None,
            collapsed_groups: HashSet::new(),
            notifier: crate::notify::Notifier::new(),
            search: None,
            prefix: false,
            search_at: Instant::now(),
            git_at: Instant::now(),
            cfg_save_pending: None,
            cfg_mtime: crate::config::mtime(),
            cfg_poll_at: Instant::now(),
            toast_since: None,
            plugins_seen: crate::install::seen(),
            plugins_commits: crate::install::commits(),
            plugins_asked: HashSet::new(),
            plugins_back: None,
            plugin_job: None,
            plugins_off: crate::install::read_off().unwrap_or_default().into_keys().collect(),
            cfg_broken: false,
            tree: None,
            focus: Focus::default(),
            sidebar_sel: None,
            tree_zoom: false,
            view: ViewState::default(),
            repos: HashMap::new(),
            router: Router::new(),
            saver: Saver::new(),
            cfg,
            spaces: Vec::new(),
            active: 0,
            next_id: 1,
            probe: crate::worker::Worker::spawn("probe", bell.clone(), probe_work, |_| None),
            probed_at: Instant::now() - Duration::from_secs(1),
            transcripts: crate::worker::Worker::spawn("transcript", bell.clone(), crate::transcript::work, |_| None),
            git: crate::worker::Worker::spawn("git", bell.clone(), git_work, git_failed),
            preview: crate::worker::Worker::spawn("preview", bell.clone(), preview_work(), preview_failed),
            plugins: crate::plugins::Plugins::default(),
            builtins: true,
            clock: String::new(),
            clock_key: None,
            battery: None,
            battery_hover: None,
            battery_warned: false,
            preview_asked: None,
            preview_modes: HashMap::new(),
            preview_req: 0,
            bell,
            exits: Vec::new(),
            exit_how: HashMap::new(),
            dirty: true,
            quit: None,
            upgrade: false,
            disconnect_all: false,
            stop_server: false,
            waiting: Default::default(),
            update_available: false,
            wait_tick: Instant::now(),
            subagents_open: std::collections::HashSet::new(),
            run: None,
            run_keys: false,
            run_zoom: false,
            run_widths: HashMap::new(),
            default_state_root: std::env::var_os("NERD_SOCKET").is_none() && std::env::var_os("NERD_STATE_DIR").is_none(),
            offered: None,
            opening: None,
            fresh: None,
            update_info: String::new(),
            update_changes: Vec::new(),
            update_skipped: false,
            updated_changes: None,
            toast_more: None,
            clients: Vec::new(),
            client: 0,
            caps: Default::default(),
            remote_ask: None,
            mail: MailState::default(),
            client_inside: false,
        }
    }

    /// Brings pty sizes in line with the layout; the process redraws itself, so no replay is needed.
    pub fn sync_sizes(&mut self, area: Rect) {
        let geometry = self.pane_geometry(area);
        let Some(tab) = self.space_mut().and_then(Space::tab_mut) else { return };
        for g in geometry {
            if let Some(p) = tab.panes.get_mut(&g.id)
                && (g.text.width != p.pty.cols || g.text.height != p.pty.rows) {
                    p.pty.resize(g.text.width, g.text.height);
                    p.term.resize(g.text.width, g.text.height);
                }
        }
    }

    /// Where each pane is and how much text space it has. Sizing and drawing share
    /// this one computation, or the process and the screen could disagree on width.
    pub fn pane_geometry(&mut self, area: Rect) -> Vec<PaneGeometry> {
        let gap = self.cfg.pane_gap;
        let mode = self.cfg.pane_border;
        let scrollbar = self.cfg.scrollbar;
        let Some(tab) = self.space_mut().and_then(Space::tab_mut) else { return Vec::new() };
        let placed = tab.layout.panes(area, gap);
        let count = placed.len();
        placed
            .into_iter()
            .filter_map(|pr| {
                let p = tab.panes.get_mut(&pr.id)?;
                let inner = crate::render::pane_inner(pr.rect, mode, count);
                let (offset, above, alt) = p.term.history();
                let show = scrollbar && crate::render::has_scrollback(offset, above, alt);
                Some(PaneGeometry {
                    id: pr.id,
                    rect: pr.rect,
                    inner,
                    // The scrollbar column stays reserved when enabled, or its appearing/disappearing would resize the process (SIGWINCH).
                    text: crate::render::content_area(inner, scrollbar),
                    scrollbar: show.then_some((offset, above)),
                })
            })
            .collect()
    }

    /// Where the panes are drawn: the screen less the sidebar and whatever the tree took.
    /// Navigation between panes is relative, so only the proportions matter.
    fn pane_area(&self) -> Rect {
        let a = crate::ui::areas(self, self.screen);
        let left = a.sidebar.or(a.rail).map_or(0, |r| r.width);
        let right = self
            .view
            .tree_frame
            .or(self.view.preview_rect)
            .or(self.view.tree_rect)
            .map_or(self.screen.width, |r| r.x)
            .max(left);
        Rect::new(left, 0, right.saturating_sub(left).max(1), self.screen.height.max(1))
    }

    /// The theme follows the setting, and in auto mode the appearance the client reported.
    pub fn apply_theme(&mut self) {
        let was = self.theme_dark;
        (self.theme, self.theme_dark) = pick_theme(&self.cfg, self.caps.dark);
        // programs that asked (mode 2031) hear that the theme turned, and ask again for its colours
        if was != self.theme_dark {
            let report = crate::term::theme_report(self.theme_dark);
            for p in self.spaces.iter_mut().flat_map(|s| s.tabs.iter_mut()).flat_map(|t| t.panes.values_mut()) {
                if p.term.theme_notify {
                    p.pty.reply(&report);
                }
            }
        }
        // the terminal's own background takes our ground, so its window padding frames nothing in another colour
        if let ratatui::style::Color::Rgb(r, g, b) = self.theme.ground {
            self.to_client.extend(format!("\x1b]11;#{r:02x}{g:02x}{b:02x}\x07").as_bytes());
        }
        self.dirty = true;
    }

    /// A setting was changed on screen: written to file immediately, so a restart doesn't lose it.
    /// The client's screen no longer shows our images: a new client, or a cleared screen.
    pub fn images_lost(&mut self) {
        self.view.logo_at = None;
        if self.tree.is_some() {
            self.view.tree.forget_image();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TmuxOffer};

    fn inside_tmux(offer: TmuxOffer) -> App {
        let mut app = App::new(Config { tmux: offer, ..Config::default() });
        app.caps.mux = true;
        app.caps.terminal = Some(std::path::PathBuf::from("/Applications/Ghostty.app"));
        // A test server may be told to live elsewhere; the offer itself is what is under test.
        app.default_state_root = true;
        app
    }

    #[test]
    fn a_config_that_does_not_parse_is_never_overwritten() {
        crate::config::tests::with_temp_home(|| {
            let path = crate::paths::config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let good = r#"{"sidebar_width": 33}"#;
            std::fs::write(&path, good).unwrap();
            let mut app = App::new(crate::config::load_or_init());
            let broken = "{ not json";
            std::fs::write(&path, broken).unwrap();
            app.reload_cfg();
            assert!(app.cfg_broken);
            assert_eq!(app.notice.as_deref(), Some(CFG_BROKEN));
            assert_eq!(app.cfg.sidebar_width, 33, "the last good settings stay");
            app.cfg.tree_width = 40;
            app.config_changed();
            app.toggle_wrap();
            app.save_cfg_soon();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
            assert_eq!(app.cfg.sidebar_width, 33);

            std::fs::write(&path, r#"{"sidebar_width": 35}"#).unwrap();
            app.reload_cfg();
            assert!(!app.cfg_broken && app.notice.is_none());
            assert_eq!(app.cfg.sidebar_width, 35);
            app.toggle_wrap();
            assert!(std::fs::read_to_string(&path).unwrap().contains("\"preview_wrap\""), "a change saves again");
        });
    }

    #[test]
    fn a_config_with_odd_bytes_survives_the_reload_tick_and_a_setting_change() {
        crate::config::tests::with_temp_home(|| {
            let path = crate::paths::config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut app = App::new(Config::default());
            let bytes = b"{\"sidebar_width\": 33, \"x\": \"\xff\"}";
            std::fs::write(&path, bytes).unwrap();
            app.cfg_poll_at = Instant::now() - Duration::from_secs(2);
            app.cfg_mtime = None;
            app.tick_cfg();
            assert!(app.cfg_broken, "the tick saw the file and could not read it");
            app.toggle_sidebar();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        });
    }

    #[test]
    fn an_edit_made_before_the_debounced_save_is_kept() {
        crate::config::tests::with_temp_home(|| {
            let path = crate::paths::config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut app = App::new(Config::default());
            app.toggle_sidebar();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            let edit = r#"{"sidebar_width": 41}"#;
            std::fs::write(&path, edit).unwrap();
            let later = std::time::SystemTime::now() + Duration::from_secs(5);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
            app.toggle_sidebar();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), edit, "the editor's file is not overwritten");
            assert_eq!(app.cfg.sidebar_width, 41, "and it is loaded");
            app.toggle_wrap();
            assert!(std::fs::read_to_string(&path).unwrap().contains("\"sidebar_width\": 41"), "the next change saves on top of it");
        });
    }

    #[test]
    fn a_server_started_from_a_broken_config_does_not_write_it() {
        crate::config::tests::with_temp_home(|| {
            let path = crate::paths::config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{ not json").unwrap();
            let (cfg, broken) = crate::config::load_checked();
            let mut app = App::new(cfg);
            assert!(broken);
            app.mark_cfg_broken();
            app.toggle_wrap();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        });
    }

    #[test]
    fn the_sidebar_toggle_is_saved_through_the_debounce_and_never_over_a_broken_file() {
        crate::config::tests::with_temp_home(|| {
            let path = crate::paths::config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut app = App::new(Config::default());
            app.toggle_sidebar();
            assert!(!app.sidebar && !app.cfg.sidebar_visible);
            assert!(!path.exists(), "nothing is written until the debounce runs out");
            app.tick_cfg();
            assert!(!path.exists(), "half a second of quiet has not passed");
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert!(std::fs::read_to_string(&path).unwrap().contains("\"sidebar_visible\": false"));

            let broken = "{ not json";
            std::fs::write(&path, broken).unwrap();
            app.mark_cfg_broken();
            app.toggle_sidebar();
            app.cfg_save_pending = Some(Instant::now() - Duration::from_secs(1));
            app.tick_cfg();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        });
    }

    #[test]
    fn menu_and_key_toggle_the_sidebar_through_one_function() {
        let mut app = with_one_space();
        let shown = app.sidebar;
        app.run_action(Action::ToggleSidebar, &MenuCtx::Global);
        assert_eq!((app.sidebar, app.cfg.sidebar_visible), (!shown, !shown));
        assert!(app.cfg_save_pending.is_some());
        app.cfg_save_pending = None;
        app.key(crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT));
        assert_eq!((app.sidebar, app.cfg.sidebar_visible), (shown, shown));
        assert!(app.cfg_save_pending.is_some());
    }

    #[test]
    fn a_terminal_preset_is_shown_then_applied_and_undone_from_settings() {
        crate::config::tests::with_temp_home(|| {
            let mut app = with_one_space();
            let open = |app: &mut App| {
                app.overlay = app.settings_outcome(crate::settings::Settings::new(&app.cfg), crate::settings::SettingsOutcome::Preset);
                let Overlay::Picker(_, pick) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("a list of terminals") };
                app.overlay = app.picked(1, pick);
            };
            open(&mut app);
            let Overlay::Confirm(c, Pending::Preset(crate::presets::Terminal::Kitty, steps, false)) = &app.overlay else { panic!("kitty, to apply") };
            let lines = c.body_lines(200);
            assert!(lines.iter().any(|l| l.contains("include ")), "the line added to kitty.conf is shown: {lines:?}");
            let steps = steps.clone();
            app.overlay = Overlay::None;
            assert!(!crate::presets::applied(crate::presets::Terminal::Kitty), "nothing before the button");
            app.perform(Pending::Preset(crate::presets::Terminal::Kitty, steps, false));
            assert!(crate::presets::applied(crate::presets::Terminal::Kitty));
            open(&mut app);
            let Overlay::Confirm(_, what @ Pending::Preset(_, _, true)) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("applied: the window offers undo") };
            app.perform(what);
            assert!(!crate::presets::applied(crate::presets::Terminal::Kitty));

            app.caps.bye = true;
            app.overlay = app.preset_picker();
            assert!(matches!(app.overlay, Overlay::None) && app.notice.as_deref().is_some_and(|n| n.contains("nerd --preset")), "a window from another Mac is sent to the command there");
        });
    }

    #[test]
    fn a_wezterm_config_nerd_does_not_edit_gets_the_line_shown_whole_and_copied_not_applied() {
        crate::config::tests::with_temp_home(|| {
            let mine = crate::config::home_dir().join(".wezterm.lua");
            std::fs::write(&mine, "return { font_size = 13 }\n").unwrap();
            let mut app = with_one_space();
            let wezterm = crate::presets::ALL.iter().position(|t| *t == crate::presets::Terminal::Wezterm).unwrap();
            app.overlay = app.picked(wezterm, Pick::Preset);
            let Overlay::Confirm(c, what @ Pending::Preset(_, _, false)) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("the WezTerm window") };
            let (_, line) = crate::presets::needs_line(match &what {
                Pending::Preset(_, s, _) => s,
                _ => unreachable!(),
            })
            .unwrap();
            assert!(c.body.contains("WezTerm needs one line from you") && c.body.ends_with(&line), "the line, whole: {}", c.body);
            assert!(c.body_lines(40).join(" ").contains("if ok then nerd(config) end"), "shown on a narrow window too");
            app.to_client.clear();
            app.perform(what);
            assert!(String::from_utf8_lossy(&app.to_client).starts_with("\x1b]52;c;"), "the button copies the line");
            assert!(app.toast.as_deref().is_some_and(|t| t.contains("not applied yet")), "{:?}", app.toast);
            assert!(!crate::presets::applied(crate::presets::Terminal::Wezterm));
            assert_eq!(crate::presets::status(crate::presets::Terminal::Wezterm), "needs your line");
            assert_eq!(std::fs::read_to_string(&mine).unwrap(), "return { font_size = 13 }\n", "the config is not touched");
        });
    }

    #[test]
    fn a_hooked_agent_is_unhooked_through_a_red_confirmation() {
        crate::config::tests::with_temp_home(|| {
            crate::session::tests::with_temp_state_dir("unhook", || {
                let home = crate::paths::home().unwrap();
                std::fs::create_dir_all(home.join(".gemini")).unwrap();
                crate::hook::install().unwrap();
                assert!(crate::hook::is_installed());
                let press = |app: &mut App, code| app.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
                let mut app = with_one_space();
                let open_hook_item = |app: &mut App| {
                    app.overlay = Overlay::Settings(crate::settings::Settings::new(&app.cfg));
                    // the hook is the row above the last one
                    press(app, KeyCode::Up);
                    press(app, KeyCode::Up);
                    press(app, KeyCode::Enter);
                };
                open_hook_item(&mut app);
                let Overlay::Confirm(c, Pending::RemoveHook) = &app.overlay else { panic!("the row asks first") };
                assert!(c.danger, "red");
                let lines = c.body_lines(80);
                assert!(lines.iter().any(|l| l.contains("Gemini CLI") && l.contains("~/.gemini/settings.json")), "{lines:?}");
                assert!(lines.iter().any(|l| l.contains("keep calling the hook until they restart")), "{lines:?}");
                press(&mut app, KeyCode::Esc);
                assert!(crate::hook::is_installed(), "cancel removes nothing");

                open_hook_item(&mut app);
                press(&mut app, KeyCode::Enter);
                assert!(matches!(app.overlay, Overlay::None));
                assert!(!crate::hook::is_installed());
                assert_eq!(app.toast.as_deref(), Some("hooks removed from Gemini CLI"));
                assert_eq!(crate::settings::Settings::new(&app.cfg).hook_row_value(), "not installed");
            });
        });
    }

    #[test]
    fn edit_config_opens_the_file_with_the_system_and_types_nothing_into_a_pane() {
        // A missing file, so the test never launches an editor.
        crate::config::tests::with_temp_home(|| {
            let mut app = with_one_space();
            let s = crate::settings::Settings::new(&app.cfg);
            let overlay = app.settings_outcome(s, crate::settings::SettingsOutcome::EditConfig);
            assert!(matches!(overlay, Overlay::None));
            assert!(app.notice.as_deref().is_some_and(|n| n.starts_with("path not found")), "{:?}", app.notice);
            assert!(app.to_client.is_empty());
        });
    }

    #[test]
    fn edit_config_from_another_computer_says_where_the_file_is_and_opens_nothing() {
        crate::config::tests::with_temp_home(|| {
            let mut app = with_one_space();
            app.caps.bye = true;
            let s = crate::settings::Settings::new(&app.cfg);
            assert!(matches!(app.settings_outcome(s, crate::settings::SettingsOutcome::EditConfig), Overlay::None));
            let n = app.notice.unwrap_or_default();
            assert!(n.starts_with("config.json is on ") && n.ends_with("config.json"), "{n:?}");
        });
    }

    fn with_one_space() -> App {
        let mut app = App::new(Config::default());
        app.spaces.push(Space {
            name: Some("s".into()),
            cwd: PathBuf::from("/"),
            tabs: vec![
                Tab { name: None, layout: Layout::new(1), panes: HashMap::new(), isolation_new: None },
                Tab { name: None, layout: Layout::new(2), panes: HashMap::new(), isolation_new: None },
            ],
            active_tab: 0,
            isolation_new: None,
        });
        app.screen = Rect::new(0, 0, 120, 40);
        app
    }

    #[test]
    fn the_hook_item_asks_before_hooking_an_agent_for_the_first_time() {
        crate::session::tests::with_temp_state_dir("first-install", || {
            let home = crate::paths::home().unwrap();
            let gemini = home.join(".gemini");
            let _ = std::fs::remove_dir_all(&gemini);
            std::fs::create_dir_all(&gemini).unwrap();
            let press = |app: &mut App, code| app.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
            let mut app = with_one_space();
            let open_hook_item = |app: &mut App| {
                app.overlay = Overlay::Settings(crate::settings::Settings::new(&app.cfg));
                // the hook is the row above the last one
                press(app, KeyCode::Up);
                press(app, KeyCode::Up);
                press(app, KeyCode::Enter);
            };
            open_hook_item(&mut app);
            let Overlay::Confirm(c, Pending::InstallHook(_)) = &app.overlay else { panic!("the item asks first") };
            assert!(c.body_lines(80).contains(&"Gemini CLI  ~/.gemini/settings.json  gemini --resume <id>".to_string()), "{:?}", c.body_lines(80));
            press(&mut app, KeyCode::Esc);
            assert!(matches!(app.overlay, Overlay::None) && !gemini.join("settings.json").exists(), "a no writes nothing");
            open_hook_item(&mut app);
            press(&mut app, KeyCode::Enter);
            assert!(std::fs::read_to_string(gemini.join("settings.json")).unwrap().contains("nerd-agent-state.sh' idle gemini"), "a yes installs");
            open_hook_item(&mut app);
            assert!(matches!(app.overlay, Overlay::Confirm(_, Pending::RemoveHook)), "hooked now: the row offers to remove");
            let _ = std::fs::remove_dir_all(&gemini);
        });
    }

    /// A plugin repository with one release, `v1.0.0`, under the temp state root.
    fn plugin_repo(name: &str) -> std::path::PathBuf {
        let dir = crate::paths::state_root().join("src").join(name);
        if dir.join(".git").is_dir() {
            return dir;
        }
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({ "name": name, "title": "Plug", "version": "1.0.0", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"], "badges": ["tree"] });
        std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
        std::fs::write(dir.join("p.sh"), "exit 0\n").unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["add", "-A"], &["commit", "-qm", "one"], &["tag", "v1.0.0"]] {
            let ok = std::process::Command::new("git").arg("-C").arg(&dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        }
        dir
    }

    #[test]
    fn a_plugin_s_refused_reports_cannot_fill_the_log() {
        crate::session::tests::with_temp_state_dir("report-flood", || {
            let mut app = with_one_space();
            for _ in 0..5000 {
                app.plugin_report("flood", 1, "nonsense");
            }
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default();
            // At most one: the limiter is the process's, so another test may have used this second.
            assert!(log.lines().filter(|l| l.contains("report for pane 1 refused")).count() <= 1, "{}", &log[..log.len().min(400)]);
            assert!(!crate::hook::state_dir().join("1").exists(), "and no state was written");
        });
    }

    /// A click on the question's first button, the one that acts.
    fn click_yes(app: &mut App) {
        if let Overlay::Confirm(c, _) = &mut app.overlay {
            c.aged(std::time::Duration::from_secs(1));
        }
        let _ = screen(app);
        let r = app.view.dialog.rects[0];
        app.mouse(crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE }, app.screen);
    }

    /// What a pane leaves: the plugin fetched and checked, and a `ready.json` naming where it claims to come from.
    fn staged_by_a_pane(name: &str) -> std::path::PathBuf {
        let mut s = crate::install::stage_add(&plugin_repo(name).to_string_lossy(), None, true).unwrap();
        s.keep = true;
        let ready = serde_json::json!({ "op": "add", "source": format!("https://github.com/me/{name}.git"), "tag": "v1.0.0" });
        std::fs::write(s.dir.join("ready.json"), ready.to_string()).unwrap();
        s.dir.clone()
    }

    #[test]
    fn a_plugin_a_pane_fetched_is_installed_only_by_an_answer_in_nerd() {
        crate::session::tests::with_temp_state_dir("plugin-ready", || {
            let press = |app: &mut App, code| app.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
            let mut app = with_one_space();
            let staged = staged_by_a_pane("plug");
            app.ask_staged_plugin();
            let Overlay::Confirm(c, Pending::PluginReady(..)) = &app.overlay else { panic!("it asks") };
            let lines = c.body_lines(100).join("\n");
            assert!(lines.contains("From https://github.com/me/plug.git, release v1.0.0") && lines.contains("Not in the catalog: nobody") && lines.contains("Risk: only shows things") && lines.contains("  Shows badges in the tree"), "{lines}");
            press(&mut app, KeyCode::Char('y'));
            assert!(matches!(app.overlay, Overlay::Confirm(_, Pending::PluginReady(..))), "a key never installs");
            click_yes(&mut app);
            let root = crate::paths::state_root();
            assert!(root.join("plugins/plug/plugin.json").is_file() && !staged.exists(), "{:?} {:?}", app.notice, app.toast);
            assert_eq!(crate::install::read_lock().unwrap()[0].source, "https://github.com/me/plug.git");

            let declined = staged_by_a_pane("other");
            app.ask_staged_plugin();
            assert!(matches!(app.overlay, Overlay::Confirm(_, Pending::PluginReady(..))));
            press(&mut app, KeyCode::Esc);
            assert!(!declined.exists() && !root.join("plugins/other").exists(), "a no deletes what was fetched");

            let moved = staged_by_a_pane("moved");
            app.ask_staged_plugin();
            let Overlay::Confirm(_, Pending::PluginReady(id, _)) = &app.overlay else { panic!("it asks") };
            app.perform(Pending::PluginReady(id.clone(), "0".repeat(40)));
            assert!(app.notice.as_deref().is_some_and(|n| n.contains("changed after the question")) && !moved.exists() && !root.join("plugins/moved").exists(), "{:?}", app.notice);
            app.overlay = Overlay::None;

            // An update a pane staged, claiming another source than the installed plugin's.
            let id = "1-2".to_string();
            let elsewhere = root.join("plugin-staging").join(&id);
            std::fs::create_dir_all(&elsewhere).unwrap();
            let clone = std::process::Command::new("git").args(["clone", "-q", "--branch", "v1.0.0"]).arg(plugin_repo("plug")).arg(elsewhere.join("repo")).output().unwrap();
            assert!(clone.status.success());
            std::fs::write(elsewhere.join("ready.json"), serde_json::json!({ "op": "update", "source": "https://github.com/evil/plug.git", "tag": "v1.0.0" }).to_string()).unwrap();
            let e = crate::install::load_ready(&id, true).map(|_| ()).unwrap_err();
            assert!(e.contains("was installed from https://github.com/me/plug.git"), "{e}");
            crate::install::discard(&id);

            // A folder is taken only for an update of a plugin installed from that same folder.
            let folder = plugin_repo("local").canonicalize().unwrap();
            crate::install::place(crate::install::stage_add(&folder.to_string_lossy(), None, true).unwrap()).unwrap();
            let stage = |id: &str, op: &str| {
                let dir = root.join("plugin-staging").join(id);
                std::fs::create_dir_all(&dir).unwrap();
                assert!(std::process::Command::new("git").args(["clone", "-q", "--branch", "v1.0.0"]).arg(&folder).arg(dir.join("repo")).output().unwrap().status.success());
                std::fs::write(dir.join("ready.json"), serde_json::json!({ "op": op, "source": folder.to_string_lossy(), "tag": "v1.0.0" }).to_string()).unwrap();
            };
            stage("1-3", "update");
            let r = crate::install::load_ready("1-3", true);
            assert!(r.as_ref().is_ok_and(|r| r.update), "an update from the folder it came from: {:?}", r.as_ref().err());
            crate::install::discard("1-3");
            stage("1-4", "add");
            assert!(crate::install::load_ready("1-4", true).map(|_| ()).unwrap_err().contains("names no GitHub repository"), "a new plugin from a folder is the terminal's");
            crate::install::discard("1-4");

            let changed = staged_by_a_pane("changed");
            std::fs::write(changed.join("repo/extra.sh"), "echo hi\n").unwrap();
            app.ask_staged_plugin();
            assert!(matches!(app.overlay, Overlay::None) && app.notice.as_deref().is_some_and(|n| n.contains("changed after")), "{:?}", app.notice);
            assert!(!changed.exists());
        });
    }

    #[test]
    fn a_resume_line_changed_since_it_was_confirmed_is_not_typed() {
        crate::session::tests::with_temp_state_dir("unconfirmed", || {
            // an id of its own: one in any process's command line would read as held
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
            let id = &format!("{:08x}-1d2f-4e6a-9b8c-{:012x}", std::process::id(), nanos & 0xffff_ffff_ffff);
            let seen = crate::paths::state_root().join("hook-confirmed.json");
            let ps = |argv: &[&str]| crate::session::PaneSnapshot {
                id: 4,
                cwd: "/".into(),
                argv: argv.iter().map(|a| a.to_string()).collect(),
                agent_session: Some(id.into()),
                agent: "codex".into(),
                label: None,
                title: None,
                cols: 80,
                rows: 24,
                isolation: Default::default(),
            };
            std::fs::write(&seen, r#"{"codex": "codex resume --yolo <id>"}"#).unwrap();
            assert_eq!(super::restore_launch(&ps(&["/bin/zsh"])), Some(vec!["codex".to_string()]), "the bare program");
            assert_eq!(super::restore_launch(&ps(&["codex", "resume", "old", "-m", "o3"])), Some(vec!["codex".into(), "-m".into(), "o3".into()]), "or its own command, stripped");
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert!(log.lines().any(|l| l.contains(" WARN ") && l.ends_with(&format!("pane 4: codex session {id} not resumed: its resume line changed since it was confirmed; typed: codex"))), "{log}");
            assert_eq!(super::program_only(&["codex".into(), "--api-key=sk-secret".into()]), "codex with 1 argument", "a key in the arguments stays out of the log");
            assert_eq!(super::program_only(&["codex".into(), "-m".into(), "o3".into()]), "codex with 2 arguments");
            std::fs::write(&seen, r#"{"codex": "codex resume <id>"}"#).unwrap();
            assert_eq!(super::restore_launch(&ps(&["/bin/zsh"])), Some(vec!["codex".into(), "resume".into(), id.to_string()]), "confirmed as it is: resumed");
        });
    }

    #[test]
    fn reload_config_reads_the_agent_manifests_again() {
        crate::session::tests::with_temp_state_dir("reload-manifests", || {
            let dir = crate::paths::state_root().join("plugins/reloadme");
            std::fs::create_dir_all(&dir).unwrap();
            let m = serde_json::json!({ "name": "reloadme", "title": "R", "agent": { "names": ["reloadme"] } });
            std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
            let mut app = with_one_space();
            app.run_action(Action::ReloadConfig, &MenuCtx::Global);
            assert!(crate::agents::get("reloadme").is_some(), "a manifest added is read");
            assert_eq!(crate::detect::identify("reloadme", &["reloadme".into()]).as_deref(), Some("reloadme"));
            std::fs::remove_dir_all(&dir).unwrap();
            app.run_action(Action::ReloadConfig, &MenuCtx::Global);
            assert!(crate::agents::get("reloadme").is_none(), "and one removed is gone");
        });
    }

    #[test]
    fn a_plugins_commands_follow_a_separator_and_behind_an_old_broker_say_so() {
        crate::session::tests::with_temp_state_dir("plugin-menu", || {
            let dir = crate::paths::state_root().join("plugins/menuplug");
            std::fs::create_dir_all(&dir).unwrap();
            let m = serde_json::json!({
                "name": "menuplug", "title": "M",
                "exec": { "command": "sh", "args": ["x.sh"] },
                "commands": [
                    { "id": "g", "title": "Global thing", "where": "global" }, { "id": "p", "title": "Pane thing", "where": "pane" },
                    { "id": "t", "title": "Tree thing", "where": "tree" }, { "id": "f", "title": "File thing", "where": "file" }, { "id": "d", "title": "Folder thing", "where": "folder" }
                ],
                "events": ["command"]
            });
            std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
            crate::agents::reload();
            let mut app = with_one_space();
            let item = |o: Overlay, label: &str| {
                let Overlay::Menu(menu, _) = o else { panic!("menu expected") };
                let at = menu.items.iter().position(|i| i.label == label).expect("the plugin's item");
                assert!(!menu.items[at - 1].enabled && menu.items[at - 1].label.is_empty(), "a separator before it");
                assert!(!menu.items[at + 1].enabled && menu.items[at + 1].label.is_empty(), "and one after it");
                menu.items[at].action
            };
            let g = crate::plugins::menu_number("menuplug", "g").unwrap();
            assert_eq!(item(app.global_menu((1, 1)), "Global thing"), Action::Plugin(g));
            assert_eq!(item(app.pane_menu(1, (1, 1)), "Pane thing"), Action::Plugin(crate::plugins::menu_number("menuplug", "p").unwrap()));
            let tree = |is_dir: bool| rows(app.tree_menu(std::env::temp_dir(), is_dir, (1, 1))).into_iter().filter(|l| l.ends_with("thing")).collect::<Vec<_>>();
            assert_eq!(tree(false), ["Tree thing", "File thing"], "a file's menu");
            assert_eq!(tree(true), ["Tree thing", "Folder thing"], "a folder's menu");
            // No broker here, as behind a handover from a build whose broker cannot launch plugins.
            app.run_action(Action::Plugin(g), &MenuCtx::Global);
            assert_eq!(app.notice.as_deref(), Some(crate::plugins::OLD_BROKER));
            std::fs::remove_dir_all(&dir).unwrap();
            crate::agents::reload();
            app.run_action(Action::Plugin(g), &MenuCtx::Global);
            assert_eq!(app.notice.as_deref(), Some("plugin menuplug has no command g any more"), "an item from a menu opened before the reload");
        });
    }

    #[test]
    fn a_plugin_writes_the_state_of_its_own_agents_pane_only() {
        crate::session::tests::with_temp_state_dir("plugin-report", || {
            let mut app = with_one_space();
            let pane = agent_pane(&app, 1, Default::default());
            app.spaces[0].tabs[0].panes.insert(1, pane);
            app.plugin_report("claude", 1, "blocked");
            assert_eq!(crate::hook::read_state(1), Some(AgentState::Blocked));
            app.plugin_report("codex", 1, "working");
            app.plugin_report("claude", 1, "sudo");
            app.plugin_report("claude", 7, "working");
            assert_eq!(crate::hook::read_state(1), Some(AgentState::Blocked), "another plugin's word, or not a state");
            assert_eq!(crate::hook::read_state(7), None, "no such pane");
        });
    }

    fn shift_alt(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::ALT | KeyModifiers::SHIFT)
    }

    fn alt(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    #[test]
    fn the_graphics_probes_late_answer_turns_images_on() {
        let reply = |app: &mut App, body: &str| {
            app.key(alt('_'));
            letter(app, body);
            app.key(alt('\\'));
        };
        let mut app = App::new(Config::default());
        reply(&mut app, "Gi=2;OK");
        assert!(!app.graphics, "an answer to another image is not the probe's");
        reply(&mut app, "Gi=31;ENOENT:no");
        assert!(!app.graphics, "a refusal says the terminal draws none");
        app.dirty = false;
        reply(&mut app, "Gi=31;OK");
        assert!(app.graphics && app.dirty, "the probe's OK after the client stopped waiting: images on, and drawn");
    }

    #[test]
    fn a_swallowed_reply_is_logged_by_its_text_only_when_it_answers_an_image() {
        assert_eq!(super::input::swallowed("Gi=31;OK".into()), "Gi=31;OK");
        assert_eq!(super::input::swallowed("11;rgb:1313/1414/1717".into()), "21 chars", "it could hold what was typed meanwhile");
        assert_eq!(super::input::swallowed("secret".into()), "6 chars");
    }

    #[test]
    fn connect_asks_the_client_and_drops_an_answer_nobody_waits_for() {
        use crate::proto::RemoteInstance;
        crate::session::tests::with_temp_state_dir("connect", || {
            let remotes = serde_json::json!([{"name": "studio", "host": "me@studio"}, {"name": "bad", "host": "-oProxyCommand=x"}]);
            let mut app = App::new(Config { remotes, ..Config::default() });
            let body = |o: Overlay| match o {
                Overlay::Confirm(c, _) => c.body,
                _ => panic!("a dialog expected"),
            };
            assert!(body(app.connect_overlay()).contains("older than the server"), "an old client cannot list");
            app.client = 1;
            app.caps = crate::proto::ClientCaps { bye: true, ..Default::default() };
            assert!(body(app.connect_overlay()).contains("from a nerd window on this Mac"), "a remote client lists the other Mac's machines");

            app.client = 1;
            app.caps = crate::proto::ClientCaps { connects: true, ..Default::default() };
            let Overlay::Picker(_, machines) = app.connect_overlay() else { panic!("the machines expected") };
            assert!(matches!(app.picked(1, machines.clone()), Overlay::None));
            assert!(app.notice.take().unwrap().contains("cannot be used"), "a bad entry never reaches the client");
            assert!(app.to_client.is_empty());

            let one = |id: &str| RemoteInstance { id: id.into(), label: id.into(), running: true, spaces: 1 };
            app.remote_listed("studio".into(), Ok(vec![one("1")]), true, false);
            assert!(!app.overlay.is_open(), "nobody asked yet");

            app.picked(0, machines.clone());
            assert_eq!(std::mem::take(&mut app.to_client), crate::proto::list_remote_marker("studio"));
            app.remote_listed("mini".into(), Ok(vec![one("1")]), true, false);
            assert!(!app.overlay.is_open(), "an answer about another machine is dropped");
            app.remote_listed("studio".into(), Ok(vec![one("1"), one("../x")]), true, false);
            let Overlay::Picker(_, instances) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("the servers expected") };
            let Pick::RemoteInstance { ref list, .. } = instances else { panic!() };
            assert_eq!(list.len(), 1, "an id that is a path is dropped here too");
            assert!(matches!(app.picked(0, instances), Overlay::None));
            assert_eq!(std::mem::take(&mut app.to_client), crate::proto::open_remote_marker("studio", Some("1")));

            // No answer in time: said once, and the late answer opens nothing.
            app.picked(0, machines.clone());
            app.remote_ask = Some(("studio".into(), Instant::now()));
            assert!(app.tick_remote());
            assert_eq!(app.notice.take().as_deref(), Some("no answer from studio in 20 s"));
            app.remote_listed("studio".into(), Ok(vec![one("1")]), true, false);
            assert!(!app.overlay.is_open());

            // A client with no terminal to open gets the command to copy instead.
            app.picked(0, machines.clone());
            app.remote_listed("studio".into(), Err("ssh to studio failed: Permission denied (publickey).".into()), false, true);
            let Overlay::Confirm(c, Pending::CopyCommand(cmd)) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("copy expected") };
            assert!(c.body.contains("Permission denied"));
            assert_eq!(cmd, "nerd --remote studio");

            // An answer that is not ssh failing is said as it is, with no terminal to offer.
            app.picked(0, machines.clone());
            app.remote_listed("studio".into(), Err("studio has no nerd".into()), true, false);
            assert!(!app.overlay.is_open());
            assert_eq!(app.notice.take().as_deref(), Some("studio has no nerd"));

            // Another client took the screen while the picker was open: nothing is asked of it.
            app.client = 2;
            app.caps = Default::default();
            app.to_client.clear();
            assert!(matches!(app.picked(0, machines), Overlay::Confirm(_, Pending::Dismiss)));
            assert!(app.to_client.is_empty());
        });
    }

    #[test]
    fn a_danger_dialog_names_the_mac_it_acts_on() {
        let mut app = with_one_space();
        let about = |app: &mut App, what: Pending| match app.confirm_overlay(what) {
            Overlay::Confirm(c, _) => c.about,
            _ => panic!("a dialog expected"),
        };
        let disconnect = |app: &mut App| match app.disconnect_overlay() {
            Overlay::Confirm(c, _) => c.about,
            _ => panic!("a dialog expected"),
        };
        assert_eq!(about(&mut app, Pending::CloseTab(0)), "1", "on this Mac the name alone");
        assert_eq!(disconnect(&mut app), "");
        app.caps.machine = Some("studio".into());
        for (what, want) in [
            (Pending::ClosePane, "On studio."),
            (Pending::CloseTab(0), "1 on studio"),
            (Pending::CloseSpace(0), "/ on studio"),
            (Pending::CloseGroup(PathBuf::from("/g")), "/g on studio"),
            (Pending::RemoveWorktree(0), "/ on studio"),
            (Pending::DeletePath(PathBuf::from("/tmp/x")), "/tmp/x on studio"),
        ] {
            let tag = format!("{what:?}");
            assert_eq!(about(&mut app, what), want, "{tag}");
        }
        let stop = match app.stop_overlay() {
            Overlay::Confirm(c, _) => c.about,
            _ => panic!("a dialog expected"),
        };
        assert_eq!(stop, "On studio.", "the stop dialog names the server's Mac too");
        assert_eq!(disconnect(&mut app), "On studio.", "an empty About becomes a sentence of its own");
    }

    #[test]
    fn the_top_row_says_which_mac_in_every_layout() {
        use ratatui::backend::TestBackend;
        // The top row as text, its cells, and where the + button went.
        let top = |app: &mut App, w: u16| {
            let mut term = ratatui::Terminal::new(TestBackend::new(w, 24)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, app);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            let row: String = (0..w).map(|x| buf[(x, 0)].symbol().to_string()).collect();
            (row, buf, app.view.hits.new_tab)
        };
        type Layout = (&'static str, u16, fn(&mut App));
        let layouts: [Layout; 5] = [
            ("sidebar", 120, |_| {}),
            ("logo", 120, |a| (a.graphics, a.caps.mux) = (true, false)),
            ("rail", 120, |a| a.sidebar = false),
            ("no sidebar", 28, |a| a.sidebar = false),
            ("tree zoom", 120, |a| {
                a.toggle_tree();
                a.tree_zoom = true;
            }),
        ];
        for (tag, w, set) in layouts {
            let mut app = with_one_space();
            set(&mut app);
            let (row, _, _) = top(&mut app, w);
            assert!(!row.contains('@'), "{tag}: this Mac's own window has no badge: {row:?}");

            app.caps.machine = Some("studio".into());
            let (row, buf, plus) = top(&mut app, w);
            let at = row.find("@ studio").unwrap_or_else(|| panic!("{tag}: no badge in {row:?}"));
            let x = row[..at].chars().count() as u16;
            assert_eq!(x + 8 + crate::ui::TEXT_INSET, w, "{tag}: right-aligned, where the right-hand labels end");
            assert!((x..x + 8).all(|c| buf[(c, 0)].fg == app.theme.warn), "{tag}: in warn");
            if let Some(p) = plus {
                assert!(p.right() < x, "{tag}: the + button {p:?} stops short of the badge at {x}");
            }
            assert!(app.view.hits.tabs.iter().all(|(r, _)| r.right() < x), "{tag}: no tab under the badge");
        }

        // Too narrow for the whole name: cut, never over the sidebar's side of the row.
        let mut app = with_one_space();
        app.sidebar = false;
        app.caps.machine = Some("a-machine-with-a-long-name".into());
        let (row, _, plus) = top(&mut app, 20);
        assert!(row.starts_with("@ a-machine-with-") && plus.is_none(), "{row:?}");
    }

    #[test]
    fn shift_option_and_an_arrow_walk_the_areas_both_ways() {
        let mut app = with_one_space();
        assert_eq!(app.focus, Focus::Panes, "the panes start with the keys");

        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sidebar, "left of the panes is the sidebar");
        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.focus, Focus::Panes, "and right again comes back");

        // nothing to move to above the topmost pane, so the step leaves the panes altogether
        app.key(shift_alt(KeyCode::Up));
        assert_eq!(app.focus, Focus::Tabs, "above the panes is the tab bar");
        app.key(shift_alt(KeyCode::Down));
        assert_eq!(app.focus, Focus::Panes, "and down comes back");
    }

    #[test]
    fn option_and_a_letter_opens_the_thing_it_is_named_after() {
        // latin and cyrillic sit on the same physical keys, so both reach the same thing
        for c in ['t', 'е'] {
            let mut app = with_one_space();
            app.key(alt(c));
            assert!(app.tree.is_some(), "⌥+T opened the tree, pressed as {c:?}");
        }
        for c in ['k', 'л'] {
            let mut app = with_one_space();
            app.key(alt(c));
            assert!(matches!(app.overlay, Overlay::Keys(_)), "⌥+K, pressed as {c:?}");
        }
        for c in ['m', 'ь'] {
            let mut app = with_one_space();
            app.key(alt(c));
            assert!(matches!(app.overlay, Overlay::Menu(..)), "⌥+M, pressed as {c:?}");
        }

        let mut app = with_one_space();
        app.key(alt('2'));
        assert_eq!(app.space_mut().unwrap().active_tab, 1, "option and a digit is still the tab");

        let mut app = with_one_space();
        app.key(alt('y'));
        assert!(app.tree.is_none() && matches!(app.overlay, Overlay::None), "a letter bound to nothing does nothing");
    }

    #[test]
    fn a_pane_with_subagents_running_is_not_idle() {
        use crate::term::AgentState::*;
        assert_eq!(with_subagents(Some(Idle), true), Some(Working), "the turn ended, the work did not");
        assert_eq!(with_subagents(Some(Idle), false), Some(Idle), "nothing running, nothing to hide");
        assert_eq!(with_subagents(Some(Blocked), true), Some(Blocked), "blocked is about the human and wins");
        assert_eq!(with_subagents(Some(Working), true), Some(Working));
        assert_eq!(with_subagents(None, true), None, "no agent in the pane at all");
    }

    /// A pane with an agent in it and no process behind it: its pty reads /dev/null.
    fn agent_pane(app: &App, id: PaneId, activity: crate::hook::Activity) -> Pane {
        use std::os::fd::IntoRawFd;
        let fd = std::fs::File::open("/dev/null").unwrap().into_raw_fd();
        Pane {
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
        }
    }

    #[test]
    fn the_pane_s_cursor_hides_while_a_menu_is_open() {
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, Default::default());
        app.spaces[0].tabs[0].panes.insert(1, pane);
        let visible = |app: &mut App| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, app);
            })
            .unwrap();
            term.backend().cursor_visible()
        };
        assert!(visible(&mut app), "the pane's cursor shows");
        app.overlay = app.global_menu((1, 1));
        assert!(!visible(&mut app), "not through the menu");
    }

    /// Takes the workers' results until `done` holds; a result is waited on, never assumed.
    fn until(app: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) -> bool {
        let t = Instant::now();
        let mut changed = false;
        while !done(app) {
            assert!(t.elapsed() < Duration::from_secs(10), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
            changed |= app.take_results();
        }
        changed
    }

    /// A frame as the loop draws it: its text, and the bytes that go out after it.
    fn frame(app: &mut App) -> (String, Vec<u8>) {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
        app.sync_tree();
        let mut out = Vec::new();
        term.draw(|f| out = crate::ui::frame(f, app).screen).unwrap();
        let buf = term.backend().buffer().clone();
        ((0..30).map(|y| (0..160).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect(), out)
    }

    fn screen(app: &mut App) -> String {
        frame(app).0
    }

    #[test]
    fn a_client_that_shows_pixels_attaching_later_still_gets_the_image() {
        let dir = std::env::temp_dir().join(format!("nerd-app-kitty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut ppm = b"P6 3 2 255\n".to_vec();
        ppm.extend([200u8; 18]);
        std::fs::write(dir.join("a.ppm"), ppm).unwrap();
        let made = std::process::Command::new("/usr/bin/sips").args(["-s", "format", "jpeg"]).arg(dir.join("a.ppm")).arg("--out").arg(dir.join("a.jpg")).output();
        assert!(made.is_ok_and(|o| o.status.success()), "sips makes a jpeg");
        std::fs::remove_file(dir.join("a.ppm")).unwrap();
        let mut app = App::new(Config::default());
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        app.caps.mux = true;
        app.toggle_tree();
        until(&mut app, "blocks", |a| screen(a).contains("jpeg, 3×2") && screen(a).contains("tmux carries no images"));
        // Now a terminal that draws images takes the screen: the PNG is made for it, and sent.
        (app.graphics, app.caps.mux) = (true, false);
        until(&mut app, "kitty bytes", |a| String::from_utf8_lossy(&frame(a).1).contains("a=T,f=100,i=1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_update_card_opens_the_update_window_and_a_skipped_build_is_not_said_again() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        crate::session::tests::with_temp_state_dir("update-card", || {
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 40);
            app.update_available = true;
            app.update_info = "nerd 0.6.0 (d987588, 2026-09-30 23:55)".into();
            app.update_changes = vec![vec!["Added".into(), "a clock setting".into()], vec!["Fixed".into(), "tabs keep their place".into()]];
            app.toast_with_more("Update available".into(), More::Update);
            let s = screen(&mut app);
            assert!(s.contains("Update available … more") && !s.contains("0.6.0"), "short, no version: {s}");
            let card = app.view.hits.notice.expect("a card");
            let click = |r: Rect| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 2, row: r.y + 1, modifiers: KeyModifiers::NONE };
            app.mouse(click(card), app.screen);
            let Overlay::Confirm(c, Pending::Update) = &app.overlay else { panic!("the card opens the update window") };
            assert_eq!(c.title, "Update nerd");
            let body = c.body_lines(200).join("\n");
            assert!(body.contains("Running") && body.contains("Available  nerd 0.6.0 (d987588") && body.contains("Added      a clock setting") && body.contains("Fixed      tabs keep their place"), "{body}");
            let s = screen(&mut app);
            assert!(s.contains("[Enter] update now") && s.contains("[Esc] later") && s.contains("[s] skip this build"), "{s}");

            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
            assert!(matches!(app.overlay, Overlay::None) && app.update_skipped && !app.upgrade);
            assert_eq!(std::fs::read_to_string(crate::paths::state_root().join("update-skipped")).unwrap(), app.update_info);
            let marked = |app: &App| {
                let Overlay::Menu(m, _) = app.global_menu((1, 1)) else { panic!("a menu") };
                m.items.iter().find(|i| i.label == "Update…").expect("Update…").mark
            };
            assert!(!marked(&app), "no mark for a skipped build");
            app.update_skipped = false;
            assert!(marked(&app), "the mark shows for one not skipped");
            app.update_skipped = true;

            // After the update: the version, and what it brought.
            app.updated_changes = Some(vec![vec!["Added".into(), "a clock setting".into()]]);
            app.toast_with_more(format!("Updated to {}", crate::changes::short_version()), More::Updated);
            let s = screen(&mut app);
            assert!(s.contains("Updated to nerd ") && s.contains("… more"), "{s}");
            let card = app.view.hits.notice.expect("a card");
            app.mouse(click(card), app.screen);
            let Overlay::Confirm(c, Pending::Dismiss) = &app.overlay else { panic!("what is new") };
            assert!(c.title.starts_with("Updated to nerd ") && c.body_lines(200).join("\n").contains("a clock setting"));
            let s = screen(&mut app);
            assert!(s.contains("[Esc] close") && !s.contains("[Enter]"), "a window that only tells: {s}");

            // Another card later is a plain one again.
            app.overlay = Overlay::None;
            app.toast = Some("copied".into());
            let s = screen(&mut app);
            assert!(s.contains("copied") && !s.contains("… more"), "{s}");
        });
    }

    #[test]
    fn a_long_notice_is_cut_on_its_card_and_read_whole_in_a_window() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = with_one_space();
        app.screen = Rect::new(0, 0, 160, 30);
        let long = format!("new build installed: {}, choose Update in the menu", "x".repeat(120));
        app.notice = Some(long.clone());
        let s = screen(&mut app);
        let top = s.lines().next().unwrap_or_default();
        assert!(!top.contains("new build"), "the top row keeps its tabs: {top}");
        assert!(s.contains("new build installed: ") && s.contains("… more") && s.contains("×"), "{s}");
        let card = app.view.hits.notice.expect("a card");
        assert!(card.width <= 64 && card.y >= 1, "below the top row: {card:?}");
        let click = |r: Rect| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 2, row: r.y + 1, modifiers: KeyModifiers::NONE };
        app.mouse(click(card), app.screen);
        let Overlay::Confirm(c, Pending::Dismiss) = &app.overlay else { panic!("the card opens the whole text") };
        assert_eq!(c.title, "What went wrong");
        assert_eq!(c.body_lines(200).join(" "), long);
        assert!(app.notice.is_none(), "read, it is gone");

        app.overlay = Overlay::None;
        app.notice = Some("short".into());
        let _ = screen(&mut app);
        let close = app.view.hits.notice_close.expect("a ×");
        app.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: close.x + 1, row: close.y, modifiers: KeyModifiers::NONE }, app.screen);
        assert!(app.notice.is_none() && matches!(app.overlay, Overlay::None), "× closes it");
    }

    #[test]
    fn the_card_leaves_the_pane_frame_in_sight_and_is_there_in_a_narrow_window() {
        for w in [160u16, 50, 36] {
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, w, 20);
            app.notice = Some(if w == 36 { "ok" } else { "config failed to save: disk full" }.into());
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 20)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, &mut app);
            })
            .unwrap();
            let card = app.view.hits.notice.unwrap_or_else(|| panic!("no card at {w} columns"));
            let body = crate::ui::areas(&app, app.screen).body;
            assert!(card.y > body.y, "the pane's top line stays: {card:?} at {w}");
            assert!(card.right() + 3 <= app.screen.right(), "the pane's right edge stays: {card:?} at {w}");
        }
    }

    #[test]
    fn the_pointer_is_a_hand_over_the_notice_card() {
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, Default::default());
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.notice = Some("config failed to save: disk full, and more words to cut it short".into());
        let _ = screen(&mut app);
        let (card, close) = (app.view.hits.notice.expect("a card"), app.view.hits.notice_close.expect("a ×"));
        for (x, y) in [(card.x + 2, card.y + 1), (close.x + 1, close.y), (card.right() - 1, card.y)] {
            assert_eq!(app.pointer_at(app.screen, x, y), crate::pointer::POINTER, "at {x},{y}");
        }
        assert_eq!(app.pointer_at(app.screen, card.x - 3, card.y + 1), crate::pointer::TEXT, "the pane beside it is text");
    }

    #[test]
    fn a_neutral_message_leaves_by_itself() {
        let mut app = with_one_space();
        app.toast = Some("copied".into());
        app.tick_cfg();
        assert!(app.toast.is_some());
        app.toast_since = app.toast_since.take().map(|(t, at)| (t, at - super::lifecycle::TOAST));
        app.tick_cfg();
        assert!(app.toast.is_none());
    }

    #[test]
    fn invisible_and_bidi_characters_are_never_drawn_and_the_real_path_is_kept() {
        let dir = std::env::temp_dir().join(format!("nerd-app-spoof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let name = "a\u{200B}b\u{202E}c.txt";
        std::fs::write(dir.join(name), "x").unwrap();
        std::fs::write(dir.join("\u{1F469}\u{200D}\u{1F4BB}.txt"), "x").unwrap();
        let mut app = App::new(Config::default());
        let tab = Tab { name: None, layout: Layout::new(1), panes: HashMap::new(), isolation_new: None };
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: vec![tab], active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        app.toggle_tree();
        app.notice = Some("x\u{202E}y\u{200B}z".into());
        until(&mut app, "the file", |a| screen(a).contains("abc.txt"));
        let s = screen(&mut app);
        assert!(!s.contains(['\u{200B}', '\u{202E}']), "{s}");
        assert!(s.contains("xyz"), "{s}");
        assert!(s.contains("\u{1F469}\u{200D}\u{1F4BB}"), "an emoji sequence keeps its joiner: {s}");
        let alt_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT);
        let view = std::mem::take(&mut app.view.tree);
        let mut paths = Vec::new();
        // Every row, whatever order the names sort in.
        for key in [KeyCode::Home, KeyCode::Down, KeyCode::Down] {
            app.tree.as_mut().unwrap().key(&view, KeyEvent::new(key, KeyModifiers::NONE));
            if let crate::tree::Outcome::InsertPath(p) = app.tree.as_mut().unwrap().key(&view, alt_i) {
                paths.push(p);
            }
        }
        assert!(paths.contains(&dir.canonicalize().unwrap().join(name)), "the path acted on keeps its name: {paths:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn after_the_prefix_the_tree_takes_its_option_keys_without_shift() {
        let dir = crate::paths::test_dir("prefix-tree");
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let mut app = App::new(Config::default());
        let tab = Tab { name: None, layout: Layout::new(1), panes: HashMap::new(), isolation_new: None };
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: vec![tab], active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        app.toggle_tree();
        until(&mut app, "the file", |a| screen(a).contains("f.txt"));
        app.focus = Focus::Tree;
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        app.key(ctrl_b);
        app.key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert!(app.overlay.is_open(), "⌥+X, delete, asks first");
        app.overlay = Overlay::None;
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!app.overlay.is_open(), "without the prefix the letter filters");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_changes_view_shows_what_the_git_worker_found() {
        let dir = crate::git::tests::init_repo("app-changes");
        std::fs::write(dir.join("README.md"), "test\nchanged\n").unwrap();
        let mut app = App::new(Config::default());
        app.learn_repo(&dir);
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        app.toggle_tree();
        app.tree.as_mut().unwrap().set_view(crate::tree::View::Changes);
        assert!(until(&mut app, "marks", |a| a.tree.as_ref().is_some_and(|t| t.wanted().is_some())), "the marks came back and redraw");
        let _ = screen(&mut app);
        until(&mut app, "diff", |a| screen(a).contains("+changed"));
        assert!(app.repos[&dir].branch.as_deref() == Some("main"), "the branch was read when the space opened");
        // The same marks again change nothing on screen.
        app.refresh_tree_marks();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!app.take_results(), "an answer that moved nothing does not redraw");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preview worker that answers nothing and tells which files it was asked to load.
    fn recording_preview(app: &mut App) -> std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>> {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        app.preview = crate::worker::Worker::spawn(
            "recording",
            app.bell.clone(),
            move |jobs: Vec<PreviewAsk>| {
                for j in jobs {
                    if let PreviewAsk::Load(w, _) = j {
                        log.lock().unwrap().push(w.path);
                    }
                }
                Vec::new()
            },
            |_| None,
        );
        asked
    }

    #[test]
    fn an_empty_diff_loads_the_file_only_while_it_is_still_wanted() {
        let dir = std::env::temp_dir().join(format!("nerd-empty-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.txt", "b.txt"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let mut app = with_one_space();
        app.spaces[0].cwd = dir.clone();
        app.toggle_tree();
        app.tree.as_mut().unwrap().set_view(crate::tree::View::Changes);
        let marks: HashMap<PathBuf, char> = [(dir.join("a.txt"), 'N'), (dir.join("b.txt"), 'N')].into();
        app.apply_git(GitFound::Marks(dir.clone(), marks));
        let asked = recording_preview(&mut app);
        let stale = app.tree.as_ref().unwrap().wanted().unwrap();
        app.tree.as_mut().unwrap().key(&app.view.tree, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let now = app.tree.as_ref().unwrap().wanted().unwrap();
        assert_ne!(stale.path, now.path);
        app.apply_git(GitFound::Diff(stale, String::new()));
        app.apply_git(GitFound::Diff(now.clone(), String::new()));
        let t = Instant::now();
        while asked.lock().unwrap().is_empty() {
            assert!(t.elapsed() < Duration::from_secs(5), "the wanted file was never asked for");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*asked.lock().unwrap(), vec![now.path], "the stale one is not asked for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_preview_whose_load_fails_says_so_and_is_not_waited_on() {
        let dir = std::env::temp_dir().join(format!("nerd-load-fails-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let mut app = App::new(Config::default());
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        app.preview = crate::worker::Worker::spawn("failing", app.bell.clone(), |_: Vec<PreviewAsk>| -> Vec<PreviewFound> { panic!("boom") }, preview_failed);
        app.toggle_tree();
        until(&mut app, "the failure", |a| screen(a).contains("the preview failed"));
        assert!(app.tree.as_mut().unwrap().next_ask().is_none(), "answered: nothing is asked again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A space on a fresh folder holding these files, its tree open.
    fn tree_on(tag: &str, files: &[(&str, &str)]) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nerd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, body) in files {
            std::fs::write(dir.join(f), body).unwrap();
        }
        let dir = dir.canonicalize().unwrap();
        let mut app = App::new(Config::default());
        app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        app.screen = Rect::new(0, 0, 160, 30);
        (app, dir)
    }

    fn select(app: &mut App, p: &std::path::Path) {
        app.tree.as_mut().unwrap().select_path(p);
        screen(app);
    }

    #[test]
    fn a_public_built_in_view_replaces_the_plain_preview_and_other_files_ask_nothing() {
        let (mut app, dir) = tree_on("builtin-view", &[("a.csv", "name,n\na.rs,12\n"), ("b.txt", "plain words"), ("c.CSV", "x,y\n")]);
        std::fs::create_dir(dir.join("folder.csv")).unwrap();
        app.builtins = true;
        app.toggle_tree();
        select(&mut app, &dir.join("folder.csv"));
        assert!(app.preview_asked.is_none(), "a folder is never asked about");
        select(&mut app, &dir.join("a.csv"));
        until(&mut app, "the table", |a| screen(a).contains("a.rs │ 12"));
        assert!(screen(&mut app).contains("a.csv · 15B · csv table"), "{}", screen(&mut app));
        // A search hit shows its line in the plain preview: nothing is asked.
        {
            let view = std::mem::take(&mut app.view.tree);
            let t = app.tree.as_mut().unwrap();
            t.key(&view, KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT));
            for c in "a.rs".chars() {
                t.key(&view, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            t.key(&view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            app.view.tree = view;
        }
        screen(&mut app);
        assert!(!app.tree.as_ref().unwrap().whole_file() && app.tree.as_ref().unwrap().selected_path() == Some(dir.join("a.csv").as_path()), "a hit is selected");
        assert!(app.preview_asked.is_none(), "a search hit is never asked about");
        app.tree.as_mut().unwrap().key(&app.view.tree, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        select(&mut app, &dir.join("b.txt"));
        assert!(app.preview_asked.is_none(), "no source previews .txt");
        until(&mut app, "the text", |a| screen(a).contains("plain words"));
        select(&mut app, &dir.join("c.CSV"));
        until(&mut app, "the upper-case one", |a| screen(a).contains("x │ y"));
        app.builtins = false;
        select(&mut app, &dir.join("a.csv"));
        assert!(app.preview_asked.is_none(), "with the built-ins off, master's preview");
        until(&mut app, "the plain csv", |a| screen(a).contains("name,n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_s_views_show_in_the_header_and_v_asks_for_the_next_one_which_stays_for_the_next_file() {
        crate::session::tests::with_temp_state_dir("preview-views", || {
            let pdir = crate::paths::state_root().join("plugins/viewer");
            std::fs::create_dir_all(&pdir).unwrap();
            let m = serde_json::json!({
                "name": "viewer", "title": "V", "exec": { "command": "sh", "args": ["x.sh"] },
                "events": ["preview_request"], "previews": [{ "extensions": ["xyz"] }]
            });
            std::fs::write(pdir.join("plugin.json"), m.to_string()).unwrap();
            crate::agents::reload();
            let (mut app, dir) = tree_on("plugin-views", &[("a.xyz", "alpha plain"), ("b.xyz", "beta plain")]);
            app.toggle_tree();
            select(&mut app, &dir.join("a.xyz"));
            let views = || vec![crate::proto::PreviewMode { id: "chat".into(), label: "Conversation".into() }, crate::proto::PreviewMode { id: "raw".into(), label: "Records".into() }];
            let answer = |req, text: &str, mode: &str| crate::proto::PluginMsg::Preview {
                req,
                lines: vec![vec![crate::proto::TextSpan { text: text.into(), role: "text".into() }]],
                cut: false,
                modes: views(),
                mode: mode.into(),
            };
            let req = app.preview_asked.as_ref().unwrap().req;
            app.plugins.slot("viewer", answer(req, "as a chat", "chat"));
            let s = screen(&mut app);
            assert!(s.contains("as a chat") && s.contains("viewer · Conversation"), "the view shown is named in the header: {s}");
            assert_eq!(app.next_view_label(), "Next view: Records");

            // The menu item and v run the same; the key's mapping is checked in the tree's own test.
            app.run_action(Action::NextView, &MenuCtx::Global);
            assert_eq!(app.preview_modes.get("viewer").map(String::as_str), Some("raw"), "remembered for the plugin");
            let again = app.preview_asked.as_ref().unwrap().req;
            assert!(again > req, "the file is asked for again, in that view");
            app.plugins.slot("viewer", answer(again, "as records", "raw"));
            let s = screen(&mut app);
            assert!(s.contains("as records") && s.contains("viewer · Records"), "{s}");

            // A plugin that names a view it does not offer is refused, and an answer with one view shows no view at all.
            assert!(crate::slots::over_cap(&answer(again, "x", "table")).is_some());
            let one = crate::proto::PluginMsg::Preview { req: again, lines: vec![], cut: false, modes: vec![views().remove(0)], mode: "chat".into() };
            app.plugins.slot("viewer", one);
            assert!(!screen(&mut app).contains("· Conversation") && app.preview_views().is_none(), "one view is no choice");
            let _ = std::fs::remove_dir_all(&dir);
            crate::agents::reload();
        });
    }

    #[test]
    fn a_plugins_preview_replaces_the_plain_one_only_while_its_file_is_still_wanted() {
        crate::session::tests::with_temp_state_dir("preview-plugin", || {
            let pdir = crate::paths::state_root().join("plugins/viewer");
            std::fs::create_dir_all(&pdir).unwrap();
            let m = serde_json::json!({
                "name": "viewer", "title": "V", "exec": { "command": "sh", "args": ["x.sh"] },
                "events": ["preview_request"], "previews": [{ "extensions": ["xyz"] }]
            });
            std::fs::write(pdir.join("plugin.json"), m.to_string()).unwrap();
            crate::agents::reload();
            let (mut app, dir) = tree_on("plugin-view", &[("a.xyz", "alpha plain"), ("b.xyz", "beta plain")]);
            // Slow plain loads, so an answer can come before one.
            let mut load = preview_work();
            app.preview = crate::worker::Worker::spawn(
                "slow",
                app.bell.clone(),
                move |jobs| {
                    std::thread::sleep(Duration::from_millis(300));
                    load(jobs)
                },
                preview_failed,
            );
            app.toggle_tree();
            let t = Instant::now();
            select(&mut app, &dir.join("a.xyz"));
            assert!(t.elapsed() < Duration::from_millis(250), "the loop asked and went on: {:?}", t.elapsed());
            let asked = app.preview_asked.as_ref().expect("asked");
            let req = asked.req;
            assert_eq!(asked.by, (true, "viewer".to_string()));
            let answer = |req, text: &str| crate::proto::PluginMsg::Preview { req, lines: vec![vec![crate::proto::TextSpan { text: text.into(), role: "ok".into() }]], cut: false, modes: vec![], mode: String::new() };

            app.plugins.slot("viewer", answer(req + 1, "stray"));
            let other = crate::slots::Decl { name: "other".into(), previews: vec!["xyz".into()], ..Default::default() };
            app.plugins.slots.ingest(&other, answer(req, "impostor")).unwrap();
            let s = screen(&mut app);
            assert!(!s.contains("stray") && !s.contains("impostor"), "another request's answer, or another source's, is dropped: {s}");
            app.plugins.slot("viewer", answer(req, "from the plugin"));
            let s = screen(&mut app);
            assert!(s.contains("from the plugin") && s.contains("a.xyz · 11B · viewer") && !s.contains("stray") && !s.contains("impostor"), "{s}");
            std::thread::sleep(Duration::from_millis(600));
            assert!(!app.take_results(), "the plain load that came after changes nothing");
            assert!(screen(&mut app).contains("from the plugin"));

            select(&mut app, &dir.join("b.xyz"));
            let req2 = app.preview_asked.as_ref().unwrap().req;
            assert!(req2 > req);
            app.plugins.slot("viewer", answer(req, "late"));
            until(&mut app, "b's plain preview", |a| screen(a).contains("beta plain"));
            assert!(!screen(&mut app).contains("late"), "an answer for a file left is dropped");
            app.plugins.slot("viewer", answer(req2, "beta view"));
            assert!(screen(&mut app).contains("beta view"));

            // A plugin that never answers: the plain preview comes, and nothing waits on it.
            select(&mut app, &dir.join("a.xyz"));
            until(&mut app, "a's plain preview", |a| screen(a).contains("alpha plain"));
            assert!(app.preview_asked.as_ref().is_some_and(|a| !a.shown));

            // The Changes view shows the diff, or the file when it has none; a plugin is never asked there.
            app.tree.as_mut().unwrap().set_view(crate::tree::View::Changes);
            app.apply_git(GitFound::Marks(dir.clone(), [(dir.join("a.xyz"), 'N'), (dir.join("b.xyz"), 'N')].into()));
            screen(&mut app);
            assert!(app.tree.as_ref().unwrap().wanted().is_some_and(|w| w.diff));
            assert!(app.preview_asked.is_none(), "no request in Changes");
            until(&mut app, "the file, its diff being empty", |a| screen(a).contains("alpha plain") || screen(a).contains("beta plain"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::remove_dir_all(&pdir).unwrap();
            crate::agents::reload();
        });
    }

    #[test]
    fn removing_a_worktree_keeps_what_git_said_for_another_space_on_that_folder() {
        let dir = std::env::temp_dir().join(format!("nerd-two-spaces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(Config::default());
        for _ in 0..2 {
            app.learn_repo(&dir);
            app.spaces.push(Space { name: None, cwd: dir.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        }
        app.repos.get_mut(&dir).unwrap().branch = Some("main".into());
        app.perform(Pending::RemoveWorktree(0));
        assert_eq!(app.spaces.len(), 1);
        assert_eq!(app.repos.get(&dir).and_then(|r| r.branch.clone()).as_deref(), Some("main"), "the other space keeps its entry");
        app.perform(Pending::RemoveWorktree(0));
        assert!(!app.repos.contains_key(&dir), "the last space takes it with it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dialog_answers_a_click_where_the_last_frame_drew_its_buttons() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = with_one_space();
        app.screen = Rect::new(0, 0, 160, 30);
        app.overlay = app.confirm_overlay(Pending::CloseTab(0));
        let _ = screen(&mut app);
        let [yes, no] = app.view.dialog.rects[..] else { panic!("two buttons drawn") };
        let click = |r: Rect| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE };
        app.mouse(click(no), app.screen);
        assert!(!app.overlay.is_open() && app.spaces[0].tabs.len() == 2, "no: nothing closed");
        app.overlay = app.confirm_overlay(Pending::CloseTab(0));
        let _ = screen(&mut app);
        app.mouse(click(yes), app.screen);
        assert_eq!(app.spaces[0].tabs.len(), 1, "yes: the tab closed");
    }

    /// Where the last frame drew `text`, as a click target; the dialog's buttons are found by what they say.
    fn find_on_screen(screen: &str, text: &str) -> Option<(u16, u16)> {
        screen.lines().enumerate().find_map(|(y, l)| l.find(text).map(|byte| (l[..byte].chars().count() as u16, y as u16)))
    }

    #[test]
    fn every_window_draws_a_close_button_and_a_click_on_it_closes_the_window() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        crate::session::tests::with_temp_state_dir("close-buttons", || {
            crate::session::tests::INSTANCE.set(Some("1".into()));
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 30);
            let pane = crate::app::snapshots::agent_pane(&app, 1, AgentState::Idle, 0);
            app.spaces[0].tabs[0].panes.insert(1, pane);
            let accounts = app.settings_outcome(crate::settings::Settings::new(&app.cfg), crate::settings::SettingsOutcome::Accounts);
            let windows = vec![
                ("Settings", app.run_action(Action::Settings, &MenuCtx::Global), "Settings"),
                ("Keys", app.run_action(Action::Keybinds, &MenuCtx::Global), "Keys and mouse"),
                ("Navigator", app.run_action(Action::Navigator, &MenuCtx::Global), "Navigator"),
                ("New space", app.run_action(Action::NewSpace, &MenuCtx::Global), "New space"),
                ("Export logs", app.run_action(Action::Report, &MenuCtx::Global), "Export logs"),
                ("Session history", app.run_action(Action::Sessions, &MenuCtx::Global), "Session history"),
                ("Messages", app.run_action(Action::Messages, &MenuCtx::Global), "Messages"),
                ("Isolation", app.run_action(Action::Isolation, &MenuCtx::Pane(1)), " Isolation "),
                ("Isolation for new panes", app.run_action(Action::IsolationNew, &MenuCtx::Tab(1)), "Isolation for new panes"),
                ("Accounts", accounts, "Accounts"),
            ];
            crate::session::tests::INSTANCE.set(None);
            for (name, overlay, title) in windows {
                app.overlay = overlay;
                let shown = screen(&mut app);
                assert!(shown.contains(title), "{name} is titled {title:?}:\n{shown}");
                // The isolation windows only save or cancel.
                let close = if name.starts_with("Isolation") { "[Esc] cancel" } else { "[Esc] close" };
                let (x, y) = find_on_screen(&shown, close).unwrap_or_else(|| panic!("{name} draws {close}:\n{shown}"));
                let button = app.close_button(app.screen).unwrap_or(app.view.dialog.panel.close);
                assert!(button.contains((x, y).into()), "{name}: the drawn button and the clickable one are the same cells");
                let pointer = app.pointer_at(app.screen, x, y);
                assert_eq!(pointer, crate::pointer::POINTER, "{name}: the pointer says it is a button");
                let down = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
                app.mouse(down, app.screen);
                assert!(!app.overlay.is_open(), "{name}: a click on [Esc] close closes it");
            }
        });
    }

    /// Runs `f` with policy.json in a state folder of its own, the policy reset around it.
    fn with_policy(tag: &str, f: impl FnOnce()) {
        let _l = crate::policy::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::policy::replace(crate::policy::Policy::default());
        crate::session::tests::with_temp_state_dir(tag, f);
        crate::policy::replace(crate::policy::Policy::default());
    }

    /// Space "work": tab main with planner (1) and an unnamed pane (2), tab two with coder (4); space "research": reader (3).
    fn named() -> App {
        let mut app = App::new(Config::default());
        let pane = |app: &App, id, label: Option<&str>| {
            let mut p = crate::app::snapshots::agent_pane(app, id, AgentState::Idle, 0);
            p.label = label.map(Into::into);
            p
        };
        let mut layout = Layout::new(1);
        layout.split(Dir::Horizontal, 2);
        layout.focus(1);
        let main = Tab { name: Some("main".into()), layout, panes: HashMap::from([(1, pane(&app, 1, Some("planner"))), (2, pane(&app, 2, None))]), isolation_new: None };
        let two = Tab { name: Some("two".into()), layout: Layout::new(4), panes: HashMap::from([(4, pane(&app, 4, Some("coder")))]), isolation_new: None };
        let research = Tab { name: None, layout: Layout::new(3), panes: HashMap::from([(3, pane(&app, 3, Some("reader")))]), isolation_new: None };
        app.spaces.push(Space { name: Some("work".into()), cwd: "/".into(), tabs: vec![main, two], active_tab: 0, isolation_new: None });
        app.spaces.push(Space { name: Some("research".into()), cwd: "/".into(), tabs: vec![research], active_tab: 0, isolation_new: None });
        app.next_id = 5;
        app.screen = Rect::new(0, 0, 160, 30);
        app
    }

    fn letter(app: &mut App, s: &str) {
        for c in s.chars() {
            app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn mouse_at(app: &mut App, kind: crossterm::event::MouseEventKind, (x, y): (u16, u16)) {
        app.mouse(crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }, app.screen);
    }

    /// Where the last frame drew this part of the Messages window.
    fn spot(app: &App, h: &crate::messages::Click) -> (u16, u16) {
        let r = app.view.dialog.messages.hits.iter().find(|(_, c)| c == h).unwrap_or_else(|| panic!("no {h:?} drawn")).0;
        (r.x, r.y)
    }

    fn click(app: &mut App, h: &crate::messages::Click) {
        screen(app);
        let at = spot(app, h);
        mouse_at(app, crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), at);
    }

    /// The matrix cell of one pair: the click that gives its next route.
    fn cell(app: &App, from: &str, to: &str) -> crate::messages::Click {
        app.view
            .dialog
            .messages
            .hits
            .iter()
            .map(|(_, c)| c)
            .find(|c| matches!(c, crate::messages::Click::Act(MailAction::SetRoute { from: f, to: t, .. }) if f.text() == from && t.text() == to))
            .cloned()
            .unwrap_or_else(|| panic!("no cell {from} → {to} drawn"))
    }

    fn letter_to(from: (PaneId, &str), to: (Option<PaneId>, &str), state: crate::mail::State) -> crate::mail::Message {
        use crate::mail::{Message, Party, Step};
        let id = crate::mail::new_id();
        let party = |pane, name: &str| Party::Pane { pane, name: name.into(), agent: None, uid: crate::policy::owner_uid() };
        let now = crate::hook::now();
        Message {
            thread: id.clone(),
            id,
            reply_to: None,
            from: party(Some(from.0), from.1),
            to: party(to.0, to.1),
            text: "hi".into(),
            hops: vec![from.1.into(), to.1.into()],
            auto: false,
            created: now,
            events: vec![Step { state: state.clone(), at: now, note: None }],
            state,
            via: None,
            passed: false,
        }
    }

    fn routes() -> Vec<String> {
        crate::policy::current().routes.iter().map(|r| r.said()).collect()
    }

    #[test]
    fn a_click_on_a_cell_cycles_the_pair_none_allow_ask_no_none() {
        with_policy("cell-cycle", || {
            let mut app = named();
            app.overlay = app.run_action(Action::Messages, &MenuCtx::Global);
            for want in [vec!["planner → reader allow"], vec!["planner → reader ask"], vec!["planner → reader no"], vec![]] {
                screen(&mut app);
                let c = cell(&app, "planner", "reader");
                click(&mut app, &c);
                assert_eq!(routes(), want);
                assert!(matches!(app.overlay, Overlay::Messages(_)), "the window stays");
            }
            // a timed allow keeps its time through the click
            let until = crate::hook::now() + 3600;
            app.mail_act(MailAction::SetRoute { from: crate::policy::Scope::Pane("coder".into()), to: crate::policy::Scope::You, action: Some(crate::policy::Action::Allow), until: Some(until) }, false).unwrap();
            screen(&mut app);
            let c = cell(&app, "coder", "you");
            click(&mut app, &c);
            assert_eq!(crate::policy::current().routes[0].until, Some(until));
            assert_eq!(routes(), ["coder → you ask"]);
        });
    }

    #[test]
    fn the_routes_map_opens_from_the_main_menu_and_changes_nothing() {
        use crate::messages::Click;
        with_policy("routes-map", || {
            let mut app = named();
            app.overlay = app.run_action(Action::RoutesMap, &MenuCtx::Global);
            let shown = screen(&mut app);
            assert!(matches!(&app.overlay, Overlay::Messages(w) if w.map_view) && shown.contains("Routes map") && shown.contains("columns 1–"), "{shown}");
            let planner = Click::Card("planner".into());
            click(&mut app, &planner);
            assert!(matches!(&app.overlay, Overlay::Messages(w) if w.selected.as_deref() == Some("planner")), "a press chooses the card");
            press(&mut app, KeyCode::Enter);
            letter(&mut app, "m");
            assert!(routes().is_empty() && crate::policy::current().mode == crate::policy::Mode::Open, "read only");
            press(&mut app, KeyCode::Esc);
            let shown = screen(&mut app);
            assert!(matches!(app.overlay, Overlay::None), "opened from the menu, Escape closes it:\n{shown}");
        });
    }

    #[test]
    fn a_click_on_a_route_chip_cycles_allow_ask_no_then_removes() {
        use crate::messages::Click;
        use crossterm::event::{MouseButton, MouseEventKind};
        with_policy("line-cycle", || {
            let mut app = named();
            app.mail_act(MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Space("research".into()), action: Some(crate::policy::Action::Allow), until: None }, false).unwrap();
            app.overlay = app.run_action(Action::Messages, &MenuCtx::Global);
            for want in ["planner → space:research ask", "planner → space:research no"] {
                screen(&mut app);
                let line = spot(&app, &Click::Line(0));
                mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), line);
                assert_eq!(routes(), [want]);
            }
            screen(&mut app);
            let line = spot(&app, &Click::Line(0));
            mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), line);
            assert!(routes().is_empty(), "removed");
            screen(&mut app);
            assert!(!app.view.dialog.messages.hits.iter().any(|(_, c)| *c == Click::Line(0)), "and no longer drawn");
        });
    }

    #[test]
    fn rename_refuses_a_duplicate_and_keeps_the_prompt_open() {
        with_policy("rename-dup", || {
            let mut app = named();
            app.overlay = app.run_action(Action::RenamePane, &MenuCtx::Pane(2));
            let shown = screen(&mut app);
            assert!(shown.contains("A named pane can receive messages from other agents."), "{shown}");
            letter(&mut app, "reader");
            press(&mut app, KeyCode::Enter);
            assert!(matches!(&app.overlay, Overlay::Prompt(p, Ask::RenamePane(2)) if p.value == "reader"), "the prompt stays open with what was typed");
            assert_eq!(app.notice.as_deref(), Some("A pane in space research, tab 1 is already called reader"));
            assert_eq!(app.pane(2).unwrap().label, None);
            // a label that is not an address is taken, and says it receives nothing
            press(&mut app, KeyCode::Backspace);
            letter(&mut app, "r two");
            press(&mut app, KeyCode::Enter);
            assert!(!app.overlay.is_open());
            assert_eq!(app.pane(2).unwrap().label.as_deref(), Some("reader two"));
            assert_eq!(app.toast.as_deref(), Some("reader two is a label only: it receives no messages"));
        });
    }

    #[test]
    fn rename_into_a_name_with_waiting_messages_asks_keep_or_drop() {
        for (key, keep) in [('k', true), ('d', false)] {
            with_policy("rename-history", || {
                let mut app = named();
                let route = MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Pane("scout".into()), action: Some(crate::policy::Action::Allow), until: None };
                app.mail_act(route, false).unwrap();
                let held = letter_to((1, "planner"), (None, "scout"), crate::mail::State::Held(crate::mail::Hold::NoAgent));
                app.mail.add(held.clone());
                app.overlay = app.run_action(Action::RenamePane, &MenuCtx::Pane(2));
                letter(&mut app, "scout");
                press(&mut app, KeyCode::Enter);
                let shown = screen(&mut app);
                assert!(shown.contains("This name has 1 route and 1 waiting message from before:") && shown.contains("planner → scout allow."), "{shown}");
                assert!(shown.contains("[k] keep them") && shown.contains("[d] drop them") && shown.contains("[Esc] cancel"), "{shown}");
                letter(&mut app, &key.to_string());
                assert!(!app.overlay.is_open());
                assert_eq!(app.pane(2).unwrap().label.as_deref(), Some("scout"));
                let state = app.mail.get(&held.id).unwrap().state.clone();
                if keep {
                    assert_eq!((routes(), state), (vec!["planner → scout allow".to_string()], crate::mail::State::Queued));
                } else {
                    assert_eq!((routes(), state), (Vec::<String>::new(), crate::mail::State::Dropped));
                }
            });
        }
    }

    #[test]
    fn isolation_knows_a_pane_whose_folder_is_the_home_folder() {
        crate::config::tests::with_temp_home(|| {
            let mut app = named();
            let home = crate::paths::home().unwrap();
            std::fs::create_dir_all(&home).unwrap();
            app.pane_mut(1).unwrap().cwd = home.join(".");
            let home_of = |o: Overlay| matches!(o, Overlay::Isolation(w) if w.home);
            assert!(home_of(app.isolation_overlay(IsolationTarget::Pane(1))), "the home folder, however it is spelled");
            assert!(!home_of(app.isolation_overlay(IsolationTarget::Pane(3))), "/ is not the home folder");
            app.spaces[1].cwd = home.clone();
            assert!(home_of(app.isolation_overlay(IsolationTarget::Tab(1, 0))), "a tab's new panes start in its space's folder");
        });
    }

    #[test]
    fn isolation_keeps_a_saved_provider_choice_and_never_turns_what_is_gone_into_the_agent() {
        use crate::isolation_ui::Out;
        use crate::policy::{Files, Isolation, Network};
        let only = |e: &[&str]| Isolation { files: Files::Everything, network: Network::Providers(e.iter().map(|s| s.to_string()).collect()) };
        let open = |app: &App, t| match app.isolation_overlay(t) {
            Overlay::Isolation(w) => w,
            _ => panic!("no window"),
        };
        let enter = |w: &mut crate::isolation_ui::IsolationWin| w.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        crate::config::tests::with_temp_home(|| {
            let mut app = named();
            app.pane_mut(1).unwrap().isolation = only(&["pi/deepseek"]);
            assert_eq!(enter(&mut open(&app, IsolationTarget::Pane(1))), Out::Save(Some(only(&["pi/deepseek"])), false), "a pane's subset, opened and saved");
            app.spaces[0].tabs[0].isolation_new = Some(only(&["pi/openai", "pi/deepseek"]));
            assert_eq!(enter(&mut open(&app, IsolationTarget::Tab(0, 0))), Out::Save(Some(only(&["pi/deepseek", "pi/openai"])), false), "a tab's subset");
            app.pane_mut(1).unwrap().isolation = only(&["pi/xyz", "pi/deepseek"]);
            let mut w = open(&app, IsolationTarget::Pane(1));
            let text: String = w.lines(&crate::render::Theme::dark()).iter().flatten().map(|(t, _, _)| t.as_str()).collect();
            assert!(text.contains("xyz: no longer in pi's manifest") && text.contains("pi: deepseek"), "{text}");
            assert_eq!(enter(&mut w), Out::Save(Some(only(&["pi/deepseek"])), false), "a gone provider is dropped on save");
            for gone in [&["pi/xyz"][..], &["foo"]] {
                app.pane_mut(1).unwrap().isolation = only(gone);
                let mut w = open(&app, IsolationTarget::Pane(1));
                let out = enter(&mut w);
                assert_eq!(out, Out::Save(Some(only(&[])), false), "{gone:?}: never pi");
                app.isolation_outcome(w, out);
                assert_eq!((app.notice.as_deref(), &app.pane(1).unwrap().isolation), (Some("no provider is chosen"), &only(gone)), "{gone:?}: refused, the pane unchanged");
            }
        });
    }

    #[test]
    fn rename_reads_the_label_of_a_pane_in_another_tab() {
        let mut app = named();
        for (id, want) in [(4, "coder"), (3, "reader")] {
            let o = app.run_action(Action::RenamePane, &MenuCtx::Pane(id));
            assert!(matches!(&o, Overlay::Prompt(p, Ask::RenamePane(i)) if p.value == want && *i == id), "pane {id}");
        }
    }

    #[test]
    fn the_sidebar_lists_held_mail_and_puts_no_mark_after_names() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut app = named();
        let held = letter_to((4, "coder"), (Some(1), "planner"), crate::mail::State::Held(crate::mail::Hold::Route));
        app.mail.add(held.clone());
        app.mail.add(letter_to((1, "planner"), (Some(3), "reader"), crate::mail::State::Queued));
        let shown = screen(&mut app);
        let side: Vec<String> = shown.lines().map(|l| l.chars().take(34).collect()).collect();
        assert!(!side.iter().any(|l| l.contains('\u{2709}')), "no envelope anywhere in the sidebar: {side:#?}");
        let row = |s: &str| side.iter().find(|l| l.contains(s)).cloned().unwrap_or_default();
        assert!(row("○ planner").trim_end_matches(['│', ' ']).ends_with("main"), "a name, then its tab: {side:#?}");
        let mail = row("coder → planner");
        assert!(mail.contains("● coder → planner · held"), "a held message needs you, marked as a blocked agent is: {side:#?}");
        let (r, _) = app.view.sidebar_mail[0].clone();
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (r.x + 2, r.y));
        assert!(matches!(&app.overlay, Overlay::Messages(w) if w.at.as_deref() == Some(held.id.as_str())), "a click opens Messages on that row");
    }

    #[test]
    fn settings_messages_rows_call_mail_act() {
        with_policy("settings-mail", || {
            let mut app = named();
            let choose = |app: &mut App, label: &str| {
                let mut s = crate::settings::Settings::new(&app.cfg);
                s.choose(label);
                let mut cfg = app.cfg.clone();
                let out = s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut cfg);
                assert_eq!(serde_json::to_string(&cfg).unwrap(), serde_json::to_string(&app.cfg).unwrap(), "{label}: nothing in config.json");
                app.settings_outcome(s, out)
            };
            assert!(matches!(choose(&mut app, "Messaging"), Overlay::Settings(_)));
            assert!(!crate::policy::current().on, "messaging off");
            choose(&mut app, "Programs in panes may stop or update nerd");
            assert!(crate::policy::current().allow_from_panes);
            assert!(matches!(choose(&mut app, "Set up a topology…"), Overlay::Picker(..)));
            assert!(matches!(choose(&mut app, "Accounts…"), Overlay::Accounts(_)));
            app.client_inside = true;
            choose(&mut app, "Messaging");
            assert_eq!(app.notice.as_deref(), Some(INSIDE_REFUSED));
            assert!(!crate::policy::current().on, "unchanged from inside a pane");
        });
    }

    #[test]
    fn a_topology_is_picked_then_confirmed() {
        with_policy("topology", || {
            let mut app = named();
            app.overlay = app.topology_overlay(None);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            assert!(matches!(&app.overlay, Overlay::Picker(p, _) if p.title == "Pick the hub"));
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            let shown = screen(&mut app);
            assert!(shown.contains("coder → * allow") && shown.contains("* → coder allow") && shown.contains("* → * no"), "{shown}");
            press(&mut app, KeyCode::Enter);
            assert_eq!(routes(), ["coder → * allow", "* → coder allow", "* → * no"]);
        });
    }

    #[test]
    fn set_up_a_topology_from_the_messages_window_comes_back_to_it() {
        with_policy("topology-window", || {
            let mut app = named();
            let in_window = |app: &App| matches!(&app.overlay, Overlay::Messages(w) if !w.map_view);
            let open = |app: &mut App| {
                app.overlay = app.run_action(Action::Messages, &MenuCtx::Global);
                screen(app);
                letter(app, "t");
                assert!(matches!(&app.overlay, Overlay::Picker(p, _) if p.title == "Set up a topology"));
            };
            open(&mut app);
            press(&mut app, KeyCode::Esc);
            assert!(in_window(&app), "Escape at the presets");
            open(&mut app);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            assert!(matches!(&app.overlay, Overlay::Picker(p, _) if p.title == "Pick the hub"));
            press(&mut app, KeyCode::Esc);
            assert!(in_window(&app), "Escape at a name");
            open(&mut app);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            assert!(matches!(app.overlay, Overlay::Confirm(..)));
            press(&mut app, KeyCode::Esc);
            assert!(in_window(&app) && routes().is_empty(), "Escape at the confirmation writes nothing");
            open(&mut app);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Enter);
            assert!(in_window(&app), "a full pass");
            assert_eq!(routes(), ["coder → * allow", "* → coder allow", "* → * no"]);
            // from Settings the way out is unchanged
            app.overlay = app.topology_overlay(None);
            press(&mut app, KeyCode::Esc);
            assert!(!app.overlay.is_open());
        });
    }

    #[test]
    fn every_protected_action_is_refused_from_a_window_inside_a_pane() {
        use crate::messages::{Click, Messages};
        use crossterm::event::{MouseButton, MouseEventKind};
        with_policy("inside", || {
            let mut app = named();
            app.mail_act(MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Pane("reader".into()), action: Some(crate::policy::Action::Ask), until: None }, false).unwrap();
            app.mail_act(MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Space("research".into()), action: Some(crate::policy::Action::Ask), until: None }, false).unwrap();
            app.edit_policy(|p| p.accounts.push(crate::policy::Account { name: "scout".into(), uid: 502 })).unwrap();
            let held = letter_to((4, "coder"), (Some(3), "reader"), crate::mail::State::Held(crate::mail::Hold::Route));
            let request = letter_to((3, "reader"), (Some(1), "planner"), crate::mail::State::Held(crate::mail::Hold::RouteRequest));
            app.mail.add(held.clone());
            app.mail.add(request.clone());
            app.client_inside = true;
            let before = crate::policy::current();
            let refused = |app: &mut App, what: &str| {
                assert_eq!(app.notice.take().as_deref(), Some(INSIDE_REFUSED), "{what}");
                assert_eq!(*crate::policy::current(), *before, "{what}");
            };
            let open = |app: &mut App, w: Messages| {
                app.overlay = Overlay::Messages(w);
                screen(app);
            };
            let click = |app: &mut App, h: Click| {
                let at = spot(app, &h);
                mouse_at(app, MouseEventKind::Down(MouseButton::Left), at);
            };

            app.overlay = app.run_action(Action::RenamePane, &MenuCtx::Pane(1));
            app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            letter(&mut app, "boss");
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "Rename pane…");

            open(&mut app, Messages::new());
            let c = cell(&app, "planner", "coder");
            click(&mut app, c);
            refused(&mut app, "a click on a cell");
            click(&mut app, Click::Line(1));
            refused(&mut app, "a click on a route chip");
            open(&mut app, Messages::on(Some("planner".into()), false));
            // the rows are under the matrix
            press(&mut app, KeyCode::PageDown);
            press(&mut app, KeyCode::PageDown);
            screen(&mut app);
            let row = MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Pane("coder".into()), action: Some(crate::policy::Action::No), until: None };
            click(&mut app, Click::Act(row));
            refused(&mut app, "a row's button");
            let mode = MailAction::SetRoute { from: crate::policy::Scope::Pane("planner".into()), to: crate::policy::Scope::Pane("reader".into()), action: None, until: None };
            click(&mut app, Click::Act(mode));
            refused(&mut app, "[mode]");
            letter(&mut app, "m");
            refused(&mut app, "the mode");
            for (k, what) in [("d", "deliver"), ("h", "deliver, allow 1 hour"), ("x", "drop")] {
                open(&mut app, Messages::at(held.id.clone()));
                letter(&mut app, k);
                refused(&mut app, what);
            }
            open(&mut app, Messages::at(held.id.clone()));
            letter(&mut app, "r");
            letter(&mut app, "ok");
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "reply");
            assert!(matches!(app.overlay, Overlay::Messages(_)), "back in the window after the prompt");
            open(&mut app, Messages::at(request.id.clone()));
            letter(&mut app, "a");
            refused(&mut app, "a route request's answer");
            letter(&mut app, "b");
            letter(&mut app, "all");
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "Broadcast…");

            app.overlay = app.run_action(Action::SendMessage, &MenuCtx::Pane(3));
            letter(&mut app, "hi");
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "Send message…");

            app.overlay = app.run_action(Action::Isolation, &MenuCtx::Pane(1));
            screen(&mut app);
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "Isolation…");
            assert!(matches!(app.overlay, Overlay::Isolation(_)), "the window stays to say why");
            app.overlay = app.run_action(Action::IsolationNew, &MenuCtx::Tab(0));
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "Isolation for new panes…");

            app.overlay = Overlay::Accounts(crate::isolation_ui::Accounts::for_name("scout"));
            letter(&mut app, "x");
            refused(&mut app, "remove an account");
            letter(&mut app, "a");
            letter(&mut app, "nobody");
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "admit an account");

            for label in ["Messaging", "Programs in panes may stop or update nerd"] {
                let mut s = crate::settings::Settings::new(&app.cfg);
                s.choose(label);
                let mut cfg = app.cfg.clone();
                let out = s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut cfg);
                app.overlay = app.settings_outcome(s, out);
                refused(&mut app, label);
            }
            app.overlay = app.topology_overlay(None);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Enter);
            refused(&mut app, "a topology");

            assert_eq!(app.pane(1).unwrap().label.as_deref(), Some("planner"));
            assert!(app.pane(1).unwrap().isolation.is_open());
            assert_eq!(app.spaces[0].tabs[0].isolation_new, None);
            assert_eq!(app.mail.get(&held.id).unwrap().state, crate::mail::State::Held(crate::mail::Hold::Route));
        });
    }

    #[test]
    fn a_prompt_draws_its_action_and_cancel_and_a_click_on_each_does_what_the_key_does() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        crate::session::tests::with_temp_state_dir("prompt-buttons", || {
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 30);
            let click = |app: &mut App, label: &str| {
                let shown = screen(app);
                let (x, y) = find_on_screen(&shown, label).unwrap_or_else(|| panic!("{label} is drawn:\n{shown}"));
                assert_eq!(app.pointer_at(app.screen, x, y), crate::pointer::POINTER, "{label}: the pointer says it is a button");
                app.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }, app.screen);
            };
            let named = |app: &App| app.space().and_then(|s| s.name.clone());
            let ask = |app: &mut App| app.overlay = app.run_action(Action::Rename, &MenuCtx::Space(0));
            ask(&mut app);
            let shown = screen(&mut app);
            assert!(shown.contains("[Enter] rename") && shown.contains("[Esc] cancel") && !shown.contains("[Esc] close"), "{shown}");
            for ch in "zed".chars() {
                app.key(crossterm::event::KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            click(&mut app, "[Esc] cancel");
            assert!(!app.overlay.is_open() && named(&app).as_deref() != Some("zed"), "cancel changes nothing");
            ask(&mut app);
            for ch in "zed".chars() {
                app.key(crossterm::event::KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            click(&mut app, "[Enter] rename");
            assert!(!app.overlay.is_open() && named(&app).is_some_and(|n| n.ends_with("zed")), "the action submits: {:?}", named(&app));
            ask(&mut app);
            app.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 0, row: 0, modifiers: KeyModifiers::NONE }, app.screen);
            assert!(!app.overlay.is_open(), "a click outside closes");
        });
    }

    #[test]
    fn a_confirmation_names_its_action_and_esc_cancels() {
        crate::config::tests::with_temp_home(|| {
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 30);
            app.update_available = true;
            let key = std::path::PathBuf::from("/tmp/x");
            let cases = vec![
                (app.confirm_overlay(Pending::ClosePane), "[Enter] close pane"),
                (app.confirm_overlay(Pending::CloseTab(0)), "[Enter] close tab"),
                (app.confirm_overlay(Pending::CloseSpace(0)), "[Enter] close space"),
                (app.confirm_overlay(Pending::CloseGroup(key.clone())), "[Enter] close group"),
                (app.confirm_overlay(Pending::RemoveWorktree(0)), "[Enter] delete checkout"),
                (app.confirm_overlay(Pending::DeletePath(key)), "[Enter] delete "),
                (app.detach_overlay(), "[Enter] detach"),
                (app.disconnect_overlay(), "[Enter] disconnect everyone"),
                (app.install_hook_overlay(), "[Enter] install hooks"),
                (app.remove_hook_overlay(), "[Enter] remove hooks"),
            ];
            for (overlay, want) in cases {
                app.overlay = overlay;
                let shown = screen(&mut app);
                assert!(shown.contains(want) && shown.contains("[Esc] cancel"), "{want}:\n{shown}");
                assert!(!shown.contains("[Enter] yes") && !shown.contains("[Esc] no "), "{want}: no bare yes and no:\n{shown}");
            }
            app.overlay = app.detach_overlay();
            let shown = screen(&mut app);
            assert!(shown.contains("Close this window? The server and everything in it"), "{shown}");
            let (x, y) = find_on_screen(&shown, "[Esc] cancel").unwrap();
            let down = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
            app.mouse(down, app.screen);
            assert!(!app.overlay.is_open() && app.quit.is_none(), "the cancel button is clickable and detaches nothing");
            app.overlay = app.detach_overlay();
            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
            assert!(app.quit.is_some(), "y still answers yes");
        });
    }

    #[test]
    fn windows_are_titled_after_the_menu_item_that_opens_them() {
        let mut app = with_one_space();
        app.screen = Rect::new(0, 0, 160, 30);
        app.caps.connects = true;
        let Overlay::Confirm(c, _) = app.connect_overlay() else { panic!("no machines: an info window") };
        assert_eq!(c.title, "Connect to another computer");
        app.cfg.remotes = serde_json::json!([{"name": "studio", "host": "me@studio.local"}]);
        let Overlay::Picker(p, _) = app.connect_overlay() else { panic!("a picker") };
        assert_eq!(p.title, "Connect to another computer");
        app.remote_ask = Some(("studio".into(), Instant::now() + std::time::Duration::from_secs(9)));
        app.remote_listed("studio".into(), Ok(vec![crate::proto::RemoteInstance { id: "1".into(), label: "main".into(), running: true, spaces: 1 }]), true, false);
        let Overlay::Picker(p, _) = &app.overlay else { panic!("the servers on it") };
        assert_eq!(p.title, "Servers on studio");
        app.remote_ask = Some(("studio".into(), Instant::now() + std::time::Duration::from_secs(9)));
        app.remote_listed("studio".into(), Err("no route".into()), true, true);
        let Overlay::Confirm(c, _) = &app.overlay else { panic!("an error window") };
        assert_eq!(c.title, "Can't reach studio");
        app.update_available = true;
        let Overlay::Confirm(c, _) = app.update_overlay() else { panic!("a question") };
        assert_eq!(c.title, "Update nerd");
    }

    #[test]
    fn the_multiplexer_is_named_as_it_is_in_the_dialog_and_in_the_menu() {
        let mut app = with_one_space();
        app.caps.mux = true;
        app.screen = Rect::new(0, 0, 160, 30);
        let label = |app: &App| {
            let Overlay::Menu(m, _) = app.global_menu((0, 0)) else { panic!("a menu") };
            m.items.iter().map(|i| i.label.clone()).find(|l| l.starts_with("Open outside")).unwrap()
        };
        assert_eq!(label(&app), "Open outside tmux…");
        app.overlay = app.tmux_offer();
        let shown = screen(&mut app);
        assert!(shown.contains("nerd is inside tmux"), "{shown}");
        app.caps.screen = true;
        assert_eq!(label(&app), "Open outside screen…");
        app.overlay = app.tmux_offer();
        let shown = screen(&mut app);
        assert!(shown.contains("nerd is inside screen") && !shown.contains("tmux"), "{shown}");
    }

    #[test]
    fn the_pointer_says_what_a_click_or_a_drag_will_do_and_is_sent_only_when_it_changes() {
        use crate::pointer::*;
        let mut app = with_one_space();
        let area = app.screen;
        let _ = screen(&mut app);
        let h = &mut app.view.hits;
        h.tabs = vec![(Rect::new(40, 0, 8, 1), 0)];
        h.sidebar_divider = Some(Rect::new(34, 2, 1, 30));
        h.borders = vec![
            crate::layout::Border { dir: crate::layout::Dir::Horizontal, rect: Rect::new(70, 2, 1, 30), path: vec![] },
            crate::layout::Border { dir: crate::layout::Dir::Vertical, rect: Rect::new(36, 20, 30, 1), path: vec![] },
        ];
        let pane = super::snapshots::agent_pane(&app, 1, crate::term::AgentState::Idle, 0);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.view.hits.panes = vec![crate::mouse::PaneHit {
            id: 1,
            rect: Rect::new(36, 2, 34, 18),
            inner: Rect::new(37, 3, 32, 16),
            wants_mouse: false,
            mouse_mode: crate::term::MouseProtocolMode::None,
            wheel: crate::term::WheelRouting::Scrollback,
        }];
        let at = |app: &App, x, y| app.pointer_at(area, x, y);
        assert_eq!(at(&app, 42, 0), POINTER, "a tab");
        assert_eq!(at(&app, 34, 10), EW, "the sidebar's edge");
        assert_eq!(at(&app, 70, 10), EW, "a border between panes side by side");
        assert_eq!(at(&app, 40, 20), NS, "a border between panes one over the other");
        assert_eq!(at(&app, 40, 5), TEXT, "a pane's text");
        assert_eq!(at(&app, 36, 2), DEFAULT, "a pane's frame");
        app.spaces[0].tabs[0].panes.get_mut(&1).unwrap().term.feed(b"\x1b]22;grab\x07");
        assert_eq!(at(&app, 40, 5), "grab", "the program's own shape over its pane");

        app.to_client.clear();
        app.point(area, 42, 0);
        app.point(area, 43, 0);
        assert_eq!(app.to_client, osc(POINTER), "once, not per motion");
        app.point(area, 40, 5);
        assert!(app.to_client.ends_with(&osc("grab")));
        app.caps.mux = true;
        app.to_client.clear();
        app.point(area, 42, 0);
        assert!(app.to_client.is_empty(), "inside tmux nothing is sent");

        app.caps.mux = false;
        app.overlay = app.confirm_overlay(Pending::CloseTab(0));
        let _ = screen(&mut app);
        let yes = app.view.dialog.rects[0];
        assert_eq!(at(&app, yes.x + 1, yes.y), POINTER, "a dialog's button");
        assert_eq!(at(&app, 0, 39), DEFAULT, "outside it, under the dialog");

        app.overlay = Overlay::None;
        app.open_context_menu(40, 5);
        let Overlay::Menu(m, _) = &app.overlay else { panic!("a menu") };
        let r = m.rect(area);
        assert_eq!(at(&app, r.x + 2, r.y + 1), POINTER, "a menu item");
        assert_eq!(at(&app, r.x, r.y), DEFAULT, "the menu's frame");
    }

    #[test]
    fn navigator_from_the_menu_opens_it() {
        let mut app = with_one_space();
        assert!(matches!(app.run_action(Action::Navigator, &MenuCtx::Global), Overlay::Navigator(_)));
    }

    #[test]
    fn marks_for_a_space_no_longer_active_are_ignored() {
        let dir = std::env::temp_dir().join(format!("nerd-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let mut app = with_one_space();
        app.spaces[0].cwd = dir.clone();
        app.toggle_tree();
        app.tree.as_mut().unwrap().set_view(crate::tree::View::Changes);
        let marks: HashMap<PathBuf, char> = [(dir.join("a.txt"), 'M')].into();
        assert!(!app.apply_git(GitFound::Marks(PathBuf::from("/elsewhere"), marks.clone())), "another space's marks");
        assert!(app.tree.as_ref().unwrap().wanted().is_none(), "nothing listed from them");
        assert!(app.apply_git(GitFound::Marks(dir.clone(), marks)), "the active space's marks");
        assert!(app.tree.as_ref().unwrap().wanted().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_process_table_found_is_what_the_pane_shows() {
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, Default::default());
        app.spaces[0].tabs[0].panes.insert(1, pane);
        let shell = crate::detect::Foreground { pid: 9, command: "zsh".into(), argv: vec!["-zsh".into()], agent: None };
        let found = |id, fg: Option<crate::detect::Foreground>| Probed { id, fg, hook: None, session: None, key: [None, None], activity: None, cwd: None };
        assert!(app.apply_foregrounds(vec![found(1, Some(shell.clone()))]), "the agent left");
        assert_eq!(app.pane(1).map(|p| p.label()), Some("zsh".into()));
        assert!(!app.apply_foregrounds(vec![found(1, Some(shell)), found(99, None)]), "nothing new, and a pane gone is skipped");
        // The hook says the agent that came back waits on the human, within the second it asked.
        let claude = crate::detect::Foreground { pid: 10, command: "claude".into(), argv: vec!["claude".into()], agent: Some("claude".into()) };
        assert!(app.apply_foregrounds(vec![Probed { hook: Some(AgentState::Blocked), ..found(1, Some(claude)) }]));
        assert_eq!(app.pane(1).map(|p| (p.label(), p.term.state())), Some(("claude".into(), AgentState::Blocked)));
    }

    #[test]
    fn a_program_killed_with_the_mouse_on_leaves_the_shell_without_it() {
        use super::model::mouse_step;
        // (left on, shell in front, wants the mouse) -> (left on, turn it off)
        assert_eq!(mouse_step(false, false, true), (true, false), "a program in front turned it on: remembered, kept");
        assert_eq!(mouse_step(true, true, true), (false, true), "killed with it on, the shell is back: off");
        assert_eq!(mouse_step(true, false, false), (false, false), "the program turned it off itself before leaving");
        assert_eq!(mouse_step(false, true, true), (false, false), "the shell turned it on itself: kept");
        assert_eq!(mouse_step(true, true, false), (false, false), "already off: nothing to do");
    }

    #[test]
    fn a_copy_keeps_what_was_already_queued_for_the_client() {
        let mut app = with_one_space();
        let mut pane = agent_pane(&app, 1, Default::default());
        pane.term.feed(b"hello");
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.to_client = b"LOGO".to_vec();
        app.copy_selection(&crate::select::Selection { pane: 1, start: (0, 0), end: (0, 4) });
        app.tree_outcome(crate::tree::Outcome::CopyPath("/tmp/x".into()));
        let sent = String::from_utf8_lossy(&app.to_client).into_owned();
        assert!(sent.starts_with("LOGO"), "{sent:?}");
        assert_eq!(sent.matches("\x1b]52;c;").count(), 2, "{sent:?}");
    }

    /// The focused pane of the first tab, with what the app writes to its pty readable back.
    fn listening_pane(app: &mut App) -> std::os::unix::net::UnixStream {
        use std::os::fd::IntoRawFd;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut pane = agent_pane(app, 1, Default::default());
        pane.pty = Pty::adopt(1, theirs.into_raw_fd(), None, 80, 24, app.bell.clone()).unwrap();
        app.spaces[0].tabs[0].panes.insert(1, pane);
        ours.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        ours
    }

    fn written(s: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
        use std::io::Read;
        let mut b = [0u8; 64];
        s.read(&mut b).map(|n| b[..n].to_vec()).unwrap_or_default()
    }

    #[test]
    fn the_prefix_is_a_choice_and_resolves_through_the_option_table() {
        use crate::config::Prefix;
        let mut app = with_one_space();
        let mut pane = listening_pane(&mut app);
        let ctrl = |c| crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let bare = |c| crossterm::event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        assert_eq!(app.cfg.prefix, Prefix::CtrlB, "on by default");
        app.cfg.prefix = Prefix::Off;
        app.key(ctrl('b'));
        assert_eq!(written(&mut pane), [0x02], "off: control+b is the shell's");

        app.cfg.prefix = Prefix::CtrlB;
        let sidebar = app.sidebar;
        app.key(ctrl('и'));
        assert!(app.prefix, "control and the B key arms it on a Russian layout");
        app.key(ctrl('и'));
        assert_eq!(written(&mut pane), [0x02], "and twice reaches the pane there too");
        let tree = app.tree.is_some();
        app.key(ctrl('и'));
        app.key(bare('е'));
        assert_eq!(app.tree.is_some(), !tree, "then the T key, printing е, is option+T");
        app.key(ctrl('b'));
        app.key(bare('T'));
        assert_eq!(app.tree.is_some(), tree, "and its case does not matter");
        app.key(ctrl('b'));
        app.key(bare('s'));
        assert_eq!(app.sidebar, !sidebar, "on: then s is option+S");
        assert!(written(&mut pane).is_empty(), "and neither half reached the pane");

        app.key(ctrl('b'));
        app.key(bare('j'));
        assert_eq!(written(&mut pane), b"j", "a letter bound to nothing is typed, not swallowed");
        app.key(ctrl('b'));
        app.key(bare('x'));
        assert_eq!(written(&mut pane), b"x", "a tree letter is the tree's only while the tree has the keys");

        app.key(ctrl('b'));
        app.key(ctrl('b'));
        assert_eq!(written(&mut pane), [0x02], "twice sends it through");

        app.key(ctrl('b'));
        app.mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 60,
                row: 20,
                modifiers: KeyModifiers::NONE,
            },
            app.screen,
        );
        app.key(bare('s'));
        assert_eq!(app.sidebar, !sidebar, "a click between the halves cancels the chord");
        assert_eq!(written(&mut pane), b"s");

        app.cfg.prefix = Prefix::CtrlA;
        app.key(ctrl('a'));
        app.key(bare('s'));
        assert_eq!(app.sidebar, sidebar, "control+a works the same way");
    }

    #[test]
    fn the_sidebar_shows_no_cursor_once_the_keys_have_left() {
        use ratatui::backend::TestBackend;
        let mut app = with_one_space();
        let _pane = listening_pane(&mut app);
        let slab_or_bar = |app: &mut App| {
            let mut term = ratatui::Terminal::new(TestBackend::new(120, 40)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, app);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            let (s, a) = (app.theme.slab, app.theme.accent);
            (0..40).any(|y| (1..app.cfg.sidebar_width - 1).any(|x| buf[(x, y)].bg == s || buf[(x, y)].bg == a))
        };
        slab_or_bar(&mut app); // the rows are known from the frame before
        app.enter(Focus::Sidebar);
        assert!(slab_or_bar(&mut app), "in the sidebar its cursor is drawn");
        app.enter(Focus::Panes);
        assert!(!slab_or_bar(&mut app), "entering again puts the cursor on the focused pane, so a left-behind row would lie");
    }

    #[test]
    fn option_and_a_sideways_arrow_is_the_shells_word_motion() {
        let mut app = with_one_space();
        let mut pane = listening_pane(&mut app);
        app.key(crossterm::event::KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.focus, Focus::Panes, "the sidebar is to the left, and option alone does not go there");
        assert!(!written(&mut pane).is_empty(), "the pane got the key");
    }

    fn sub_running(now: u64) -> crate::hook::Activity {
        use crate::hook::{Activity, Call, Outcome, Sub};
        let call = |tool: &str, target: &str, outcome| Call { id: String::new(), tool: tool.into(), target: target.into(), at: now, outcome, summary: String::new() };
        Activity {
            main: Some(call("Agent", "Find the callers", Outcome::Running)),
            subs: vec![Sub { id: "a1".into(), kind: "Explore".into(), task: "Find the callers".into(), started: now, last: now, stopped: None, calls: vec![call("Read", "src/client.rs", Outcome::Ok)], alone: false }],
            cut: false,
        }
    }

    #[test]
    fn a_subagent_row_opens_its_run_as_a_section_of_its_pane() {
        use ratatui::backend::TestBackend;
        let now = crate::hook::now();
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, sub_running(now));
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.spaces[0].active_tab = 1;
        app.subagents_open.insert(1);
        let screen = Rect::new(0, 0, 120, 40);
        let draw = |app: &mut App| {
            let mut term = ratatui::Terminal::new(TestBackend::new(120, 40)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, app);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            (0..40).map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect::<String>()
        };
        let press = |x, y| crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let esc = crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let t = draw(&mut app);
        assert!(t.contains("● Explore · Read"), "the subagent's row: {t}");
        assert!(!t.contains("Agent Find the callers"), "no line of the agent's own call under its row: {t}");

        // the row first brings its pane forward, then opens the section in it
        let (r, _, _) = app.view.hits.sidebar_subagent_rows[0].clone();
        app.mouse(press(r.x + 3, r.y), screen);
        assert_eq!(app.spaces[0].active_tab, 0, "the parent pane's tab is shown");
        assert!(app.run.as_ref().is_some_and(|r| r.pane == 1 && r.id == "a1"));
        let t = draw(&mut app);
        let before: Vec<Rect> = app.view.hits.panes.iter().map(|p| p.rect).collect();
        let frame = before[0];
        let lines: Vec<Vec<char>> = t.lines().map(|l| l.chars().collect()).collect();
        let w = app.cfg.tree_width.min(frame.width - 24).max(20) + 4;
        let seam = (frame.right() - w) as usize;
        let top = frame.y as usize;
        assert_eq!(lines[top][seam], '┬', "the seam meets the pane's top edge: {}", t.lines().nth(top).unwrap());
        assert_eq!(lines[frame.bottom() as usize - 1][seam], '┴');
        assert!((top + 1..frame.bottom() as usize - 1).all(|y| lines[y][seam] == '│'), "and runs down the pane");
        assert_eq!(lines[top][frame.right() as usize - 1], '┐', "the pane's own corner stays");
        let head: String = lines[top][seam..].iter().collect();
        // whole seconds since `now`: a second may tick between the setup and the draw
        let fresh = head.contains(" · 0s ") || head.contains(" · 1s ");
        assert!(head.starts_with("┬─ claude · Explore") && fresh && head.contains(" ⊡ · × ─┐"), "{head:?}");
        assert!(t.contains("✓ Read(src/client.rs)") && t.contains("⎿ Done") && !t.contains("[Esc] close"), "{t}");

        // the pane keeps its size; its text simply goes on under the section
        let (x, y) = (seam as u16 + 3, top as u16 + 3);
        app.mouse(press(x, y), screen);
        assert!(app.run.is_some(), "a click inside the section is its own");
        let t = draw(&mut app);
        assert_eq!(app.view.hits.panes.iter().map(|p| p.rect).collect::<Vec<_>>(), before, "nothing on screen moves for it");

        // it follows the log
        let sub = &mut app.pane_mut(1).unwrap().activity.subs[0];
        sub.calls.push(crate::hook::Call { id: "t2".into(), tool: "Bash".into(), target: "cargo test".into(), at: now, outcome: crate::hook::Outcome::Running, summary: String::new() });
        let t2 = draw(&mut app);
        assert_ne!(t, t2);
        assert!(t2.contains("● Bash(cargo test)") && t2.contains("⎿ Running…") && t2.contains("2 tool calls"), "{t2}");

        // Esc closes it while it was the last thing clicked; after a click in the pane, Esc is the pane's
        app.mouse(press(frame.x + 3, frame.y + 3), screen);
        app.key(esc);
        assert!(app.run.is_some(), "Esc went to the agent");
        app.mouse(press(x, y), screen);
        app.key(esc);
        assert!(app.run.is_none());

        // × closes it too
        app.overlay = app.subagents_overlay(1);
        assert!(matches!(app.overlay, Overlay::None) && app.run.is_some(), "the pane menu opens it the same way");
        draw(&mut app);
        let close = (frame.right() - 4, frame.y);
        app.mouse(press(close.0, close.1), screen);
        assert!(app.run.is_none(), "× in the title closes it");

        // it belongs to its pane: the pane's going takes it along
        app.open_run(1, "a1".into());
        app.drop_pane(1, None);
        assert!(app.run.is_none());
    }

    #[test]
    fn the_section_resizes_by_its_seam_zooms_and_copies() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::backend::TestBackend;
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, sub_running(crate::hook::now()));
        app.spaces[0].tabs[0].panes.insert(1, pane);
        let screen = Rect::new(0, 0, 120, 40);
        let draw = |app: &mut App| {
            let mut term = ratatui::Terminal::new(TestBackend::new(120, 40)).unwrap();
            term.draw(|f| {
                crate::ui::frame(f, app);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            (0..40).map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>()
        };
        let at = |kind, x, y| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        app.open_run(1, "a1".into());
        let lines = draw(&mut app);
        let frame = app.view.hits.panes[0].rect;
        let panes = frame;
        let seam_at = |lines: &[String]| lines[frame.y as usize].chars().position(|c| c == '┬').unwrap() as u16;
        let seam = seam_at(&lines);

        // the seam drags like a pane's, and the width stays with the pane
        app.mouse(at(MouseEventKind::Down(MouseButton::Left), seam, frame.y + 5), screen);
        app.mouse(at(MouseEventKind::Drag(MouseButton::Left), seam - 10, frame.y + 5), screen);
        app.mouse(at(MouseEventKind::Up(MouseButton::Left), seam - 10, frame.y + 5), screen);
        let lines = draw(&mut app);
        assert_eq!(seam_at(&lines), seam - 10);
        assert_eq!(app.run_widths.get(&1), Some(&(frame.right() - (seam - 10))));
        assert_eq!(app.view.hits.panes[0].rect, panes, "the pane under it keeps its size");
        app.run = None;
        app.open_run(1, "a1".into());
        assert_eq!(seam_at(&draw(&mut app)), seam - 10, "reopened, it keeps the width it was given");
        // bounds: the pane keeps 20 columns, the section 12
        for (to, want) in [(frame.x, frame.x + 20), (frame.right() - 1, frame.right() - 12)] {
            let s = seam_at(&draw(&mut app));
            app.mouse(at(MouseEventKind::Down(MouseButton::Left), s, frame.y + 5), screen);
            app.mouse(at(MouseEventKind::Drag(MouseButton::Left), to, frame.y + 5), screen);
            app.mouse(at(MouseEventKind::Up(MouseButton::Left), to, frame.y + 5), screen);
            assert_eq!(seam_at(&draw(&mut app)), want);
        }

        // ⊡ takes the whole screen under the top bar; again, or Esc, gives it back
        let lines = draw(&mut app);
        let zoom = lines[frame.y as usize].chars().position(|c| c == '⊡').unwrap() as u16;
        app.mouse(at(MouseEventKind::Down(MouseButton::Left), zoom, frame.y), screen);
        app.mouse(at(MouseEventKind::Up(MouseButton::Left), zoom, frame.y), screen);
        let lines = draw(&mut app);
        assert!(lines[2].starts_with("┌─ claude · Explore") && lines[2].ends_with('┐') && lines[39].starts_with('└'), "{:?}", lines[2]);
        assert!(!lines.iter().any(|l| l.contains("needs you")), "the sidebar is under it");
        app.key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.run_zoom && app.run.is_some(), "Esc leaves the zoom, the section stays");
        assert!(draw(&mut app).iter().any(|l| l.contains("needs you")));

        // a drag over two lines copies them, as a pane's text does
        let lines = draw(&mut app);
        let row = lines.iter().position(|l| l.contains("✓ Read(")).unwrap() as u16;
        let col = lines[row as usize].chars().position(|c| c == '✓').unwrap() as u16;
        app.mouse(at(MouseEventKind::Down(MouseButton::Left), col, row), screen);
        app.mouse(at(MouseEventKind::Drag(MouseButton::Left), 118, row + 1), screen);
        app.mouse(at(MouseEventKind::Up(MouseButton::Left), 118, row + 1), screen);
        let mut term = ratatui::Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut fx = crate::ui::Effects::default();
        term.draw(|f| fx = crate::ui::frame(f, &mut app)).unwrap();
        assert_eq!(fx.toast.as_deref(), Some("copied"));
        assert!(fx.copy.as_deref().is_some_and(|t| t.contains("Read(")), "{:?}", fx.copy);
    }

    #[test]
    fn a_working_agent_redraws_its_times_every_second() {
        let mut app = with_one_space();
        let pane = agent_pane(&app, 1, sub_running(crate::hook::now()));
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.tick_states();
        app.wait_tick = Instant::now() - Duration::from_secs(2);
        assert!(app.tick_states(), "a subagent row's time moves on");
        assert!(!app.tick_states(), "at most once a second");

        // the agent's own call is on no line, and the subagent finished long ago: nothing to count
        app.pane_mut(1).unwrap().activity.subs[0].stopped = Some(1);
        app.wait_tick = Instant::now() - Duration::from_secs(2);
        assert!(!app.tick_states(), "a call of the agent's own in flight draws no frame");
    }

    #[test]
    fn the_action_letters_do_what_they_say() {
        let mut app = with_one_space();
        let side = app.sidebar;
        app.key(alt('s'));
        assert_eq!(app.sidebar, !side, "⌥+S is the sidebar");

        let mut app = with_one_space();
        app.key(alt('w'));
        assert!(matches!(app.overlay, Overlay::Confirm(..)), "⌥+W asks before closing the pane");

        let mut app = with_one_space();
        app.key(alt('u'));
        assert!(app.toast.is_some() || matches!(app.overlay, Overlay::Confirm(..)), "⌥+U is the update");
    }

    #[test]
    fn every_close_and_every_delete_asks_and_names_what_it_would_take() {
        let mut app = with_one_space();
        app.spaces[0].tabs[0].name = Some("build".into());

        let named = |app: &mut App, what: Pending| match app.confirm_overlay(what) {
            Overlay::Confirm(c, _) => (c.title.clone(), c.about.clone(), c.body.clone()),
            _ => panic!("a close or a delete that does not ask"),
        };

        let (title, about, body) = named(&mut app, Pending::CloseTab(0));
        assert_eq!(title, "Close tab");
        assert_eq!(about, "build", "the tab is named by its name");
        assert!(body.contains("will close"), "{body:?}");

        let (_, about, _) = named(&mut app, Pending::CloseSpace(0));
        assert_eq!(about, "/", "the space by its folder");

        let (_, about, _) = named(&mut app, Pending::CloseGroup(PathBuf::from("/tmp/repo")));
        assert_eq!(about, "/tmp/repo", "the group by its folder");

        let (title, about, body) = named(&mut app, Pending::DeletePath(PathBuf::from("/tmp/a/b.txt")));
        assert_eq!(title, "Delete");
        assert_eq!(about, "/tmp/a/b.txt", "a path by its path");
        assert!(!body.contains("pane"), "a file is not a pane: {body:?}");

        // a tab with no pane in it still asks: the dialog is the rule, not a function of
        // the count. What a pane is called comes from `Pane::label`, the sidebar's own name
        // for it, which needs a pty and so is not built here.
        assert!(matches!(app.confirm_overlay(Pending::ClosePane), Overlay::Confirm(..)));
        assert!(matches!(app.confirm_overlay(Pending::CloseTab(1)), Overlay::Confirm(..)));
    }

    #[test]
    fn a_pane_that_was_a_command_goes_back_to_a_prompt_instead_of_closing_the_tab() {
        // The pane's own process is what the human sees leave. A shell leaving is the window
        // closing; an agent leaving is not.
        assert!(pane_was_a_command(&["claude".into(), "--resume".into(), "x".into()], "/bin/zsh"));
        assert!(pane_was_a_command(&["/opt/homebrew/bin/claude".into()], "/bin/zsh"));
        assert!(!pane_was_a_command(&["/bin/zsh".into()], "/bin/zsh"));
        assert!(!pane_was_a_command(&super::shell_argv(), &super::user_shell()), "the login shell a pane starts is still the shell");
        // the same shell named by a different path is still the shell
        assert!(!pane_was_a_command(&["zsh".into()], "/bin/zsh"));
        assert!(!pane_was_a_command(&[], "/bin/zsh"));
    }

    #[test]
    fn stopping_the_server_asks_in_red_and_only_enter_stops_it() {
        let mut app = with_one_space();
        app.screen = Rect::new(0, 0, 160, 30);
        app.overlay = app.run_action(Action::StopServer, &MenuCtx::Global);
        let Overlay::Confirm(c, Pending::StopServer) = &app.overlay else { panic!("it asks") };
        assert!(c.danger && c.body.starts_with("0 panes close, and what runs in them is killed. Start it again on "), "{}", c.body);
        let shown = screen(&mut app);
        assert!(shown.contains("[Enter] stop the server") && shown.contains("[Esc] cancel"), "{shown}");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.overlay.is_open() && !app.stop_server, "cancel stops nothing");
        app.overlay = app.run_action(Action::StopServer, &MenuCtx::Global);
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.stop_server, "confirming asks the daemon to stop");
    }

    #[test]
    fn about_shows_the_version_and_paths_and_copies_the_version() {
        let mut app = with_one_space();
        app.screen = Rect::new(0, 0, 160, 30);
        app.overlay = app.run_action(Action::About, &MenuCtx::Global);
        let shown = screen(&mut app);
        for want in [crate::version_line().as_str(), "This server", "running", "config.json", "daemon.log", "Licence", "MIT", "[Enter] copy version", "[Esc] close"] {
            assert!(shown.contains(want), "{want}:\n{shown}");
        }
        let (x, y) = find_on_screen(&shown, "[Esc] close").unwrap();
        let down = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
        app.mouse(down, app.screen);
        assert!(!app.overlay.is_open() && !String::from_utf8_lossy(&app.to_client).contains("\x1b]52;"), "[Esc] close is clickable and copies nothing");
        app.overlay = app.run_action(Action::About, &MenuCtx::Global);
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(String::from_utf8_lossy(&app.to_client).contains("\x1b]52;"), "the version goes to the clipboard");
        assert!(app.toast.as_deref().is_some_and(|t| t.starts_with("copied")));
    }

    #[test]
    fn disconnecting_everyone_asks_and_counts_only_the_unattached() {
        let mut app = with_one_space();
        let Overlay::Menu(menu, _) = app.global_menu((1, 1)) else { panic!("menu expected") };
        assert!(menu.items.iter().any(|i| i.label == "Disconnect everyone…"), "the menu offers it");
        let mut keep = Vec::new();
        for (n, want) in [(0, ""), (1, "This window and 1 connection that has not attached"), (2, "This window and 2 connections that have not attached")] {
            while app.waiting.lock().unwrap().len() < n {
                let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
                app.waiting.lock().unwrap().insert(n as u64 * 10 + keep.len() as u64, a);
                keep.push(b);
            }
            let Overlay::Confirm(c, Pending::DisconnectAll) = app.run_action(Action::DisconnectAll, &MenuCtx::Global) else { panic!("it asks") };
            assert_eq!(c.about, want);
            assert_eq!(c.body, "Disconnect everyone? This window closes too. Programs keep running.");
        }
        assert!(!app.disconnect_all, "asking does not act");
        app.perform(Pending::DisconnectAll);
        assert!(app.disconnect_all);
    }

    fn right_click(x: u16, y: u16) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right), column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn right_click_on_a_sidebar_row_opens_that_panes_menu() {
        // A pane row under a space, and a "needs you" row, are the same hit list: one rect stands for both.
        let mut app = with_one_space();
        app.view.hits.sidebar_rows = vec![(Rect::new(0, 5, 20, 1), 7)];
        app.mouse(right_click(2, 5), app.screen);
        let Overlay::Menu(_, MenuCtx::Pane(id)) = app.overlay else { panic!("a pane menu expected") };
        assert_eq!(id, 7);
    }

    #[test]
    fn right_click_on_a_subagent_row_a_button_or_empty_sidebar_opens_nothing() {
        let mut app = with_one_space();
        app.view.hits.sidebar_subagent_rows = vec![(Rect::new(0, 6, 20, 1), 7, "a1".into())];
        app.view.hits.sidebar_subagents = vec![(Rect::new(0, 7, 3, 1), 7)];
        app.view.hits.sidebar_new = Some(Rect::new(0, 0, 3, 1));
        for (x, y) in [(2, 6), (1, 7), (1, 0), (50, 50)] {
            app.mouse(right_click(x, y), app.screen);
            assert!(matches!(app.overlay, Overlay::None), "({x}, {y})");
        }
    }

    #[test]
    fn right_click_on_the_menu_button_opens_the_global_menu() {
        let mut app = with_one_space();
        app.view.hits.sidebar_menu = Some(Rect::new(30, 0, 1, 1));
        app.mouse(right_click(30, 0), app.screen);
        assert!(matches!(app.overlay, Overlay::Menu(_, MenuCtx::Global)));
    }

    /// One row per item as the menu draws it: greyed rows start with `~`, keys follow the label, a separator is `—`.
    fn rows(o: Overlay) -> Vec<String> {
        let Overlay::Menu(menu, _) = o else { panic!("a menu expected") };
        menu.items
            .iter()
            .map(|i| match (i.label.is_empty(), i.enabled) {
                (true, _) => "—".to_string(),
                (false, on) => format!("{}{}{}", if on { "" } else { "~" }, i.label, if i.key.is_empty() { String::new() } else { format!(" [{}]", i.key) }),
            })
            .collect()
    }

    fn strings(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_running_server_drops_a_removed_plugin_and_takes_back_one_turned_on() {
        crate::session::tests::with_temp_state_dir("plugins-sync", || {
            let root = crate::paths::state_root();
            let local = root.join("plugins/loc");
            std::fs::create_dir_all(&local).unwrap();
            std::fs::write(local.join("plugin.json"), serde_json::json!({ "name": "loc", "title": "L", "agent": { "names": ["loc"] } }).to_string()).unwrap();
            crate::agents::reload();
            let mut app = with_one_space();
            let loaded = |n: &str| crate::agents::get(n).is_some();
            crate::install::place(crate::install::stage_add(&plugin_repo("plug").to_string_lossy(), None, true).unwrap()).unwrap();
            app.reload_plugins_if_changed();
            assert!(loaded("plug") && loaded("loc"));

            crate::install::turn("loc", false, "off").unwrap();
            app.reload_plugins_if_changed();
            assert!(!loaded("loc"));
            crate::install::turn("loc", true, "").unwrap();
            app.reload_plugins_if_changed();
            assert!(loaded("loc"), "a plugin put there by hand is back once turned on");

            crate::install::remove("plug", false).unwrap();
            app.reload_plugins_if_changed();
            assert!(!loaded("plug"), "a removed slot plugin is gone, so nothing starts it again");
            std::fs::create_dir_all(root.join("plugins/dropped")).unwrap();
            std::fs::write(root.join("plugins/dropped/plugin.json"), serde_json::json!({ "name": "dropped", "title": "D", "agent": { "names": ["dropped"] } }).to_string()).unwrap();
            crate::install::turn("loc", false, "off").unwrap();
            app.reload_plugins_if_changed();
            assert!(!loaded("dropped"), "a folder that appeared on its own waits for Reload plugins");
            crate::install::turn("dropped", false, "off").unwrap();
            app.reload_plugins_if_changed();
            crate::install::turn("dropped", true, "").unwrap();
            app.reload_plugins_if_changed();
            assert!(!loaded("dropped"), "nor does turning it off and on load it");
            crate::agents::reload();
        });
    }

    #[test]
    fn a_pane_s_panel_never_shows_what_another_pane_s_opening_left_and_a_global_one_keeps_its_content() {
        crate::session::tests::with_temp_state_dir("panel-forget", || {
            let dir = crate::paths::state_root().join("plugins/lister");
            std::fs::create_dir_all(&dir).unwrap();
            let m = serde_json::json!({
                "name": "lister", "title": "L", "exec": { "command": "sh", "args": ["x.sh"] }, "events": ["panel_opened"],
                "panels": [{ "id": "here", "title": "Here", "where": "pane" }, { "id": "all", "title": "All", "where": "global" }]
            });
            std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
            crate::agents::reload();
            let mut app = with_one_space();
            let decl = crate::slots::Decl::of(&crate::agents::get("lister").unwrap());
            let src: crate::slots::Src = (true, "lister".into());
            for id in ["here", "all"] {
                let panel = serde_json::json!({ "Panel": { "id": id, "kind": "list", "title": "Old", "items": [{ "id": "x", "label": "from before" }] } });
                app.plugins.slots.ingest(&decl, serde_json::from_value(panel).unwrap()).unwrap();
            }
            app.plugins.slots.apply();
            assert!(app.plugins.slots.panel(&src, "here").is_some() && app.plugins.slots.panel(&src, "all").is_some(), "both were sent ahead");
            app.run_action(Action::Panel(crate::plugins::menu_number("lister", "here").unwrap()), &MenuCtx::Pane(1));
            assert!(app.plugins.slots.panel(&src, "here").is_none(), "opened from a pane, the last pane's list is gone until the plugin answers");
            app.run_action(Action::Panel(crate::plugins::menu_number("lister", "all").unwrap()), &MenuCtx::Global);
            assert!(app.plugins.slots.panel(&src, "all").is_some(), "a global panel keeps what the plugin sent ahead");
            crate::agents::reload();
        });
    }

    #[test]
    fn a_folder_put_in_by_hand_waits_for_reload_and_enter_on_its_row_loads_it() {
        crate::session::tests::with_temp_state_dir("plugins-waits", || {
            let mut app = with_one_space();
            let dir = crate::paths::state_root().join("plugins/hand");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.json"), serde_json::json!({ "name": "hand", "title": "H", "agent": { "names": ["hand"] } }).to_string()).unwrap();
            app.reload_plugins_if_changed();
            let text = |app: &mut App| {
                let Overlay::Plugins(w) = app.run_action(Action::Plugins, &MenuCtx::Global) else { panic!("it opens") };
                let mut buf = ratatui::buffer::Buffer::empty(app.screen);
                w.draw(&mut buf, app.screen, &crate::render::Theme::dark());
                (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect::<String>()
            };
            let before = text(&mut app);
            assert!(before.lines().any(|l| l.contains("hand ") && l.contains("by hand") && l.contains("waits for Reload")), "{before}");
            let Overlay::Plugins(w) = app.run_action(Action::Plugins, &MenuCtx::Global) else { panic!("it opens") };
            app.overlay = app.plugins_outcome(w, crate::plugin_list::Out::Act("hand".into(), crate::plugin_list::Act::Reload));
            assert!(matches!(app.overlay, Overlay::Plugins(_)), "the window stays");
            assert!(crate::agents::get("hand").is_some(), "Enter loaded it");
            let after = text(&mut app);
            assert!(!after.contains("waits for Reload"), "{after}");
            crate::agents::reload();
        });
    }

    #[test]
    fn the_plugins_window_lists_each_plugin_and_its_menu_acts_on_it() {
        crate::session::tests::with_temp_state_dir("plugins-window", || {
            let mut app = with_one_space();
            crate::install::place(crate::install::stage_add(&plugin_repo("plug").to_string_lossy(), None, true).unwrap()).unwrap();
            let Overlay::Plugins(w) = app.run_action(Action::Plugins, &MenuCtx::Global) else { panic!("it opens") };
            let mut buf = ratatui::buffer::Buffer::empty(app.screen);
            w.draw(&mut buf, app.screen, &crate::render::Theme::dark());
            let text: String = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
            assert!(text.contains("One plugin is installed; the catalog was never fetched."), "{text}");
            assert!(text.contains("Add from GitHub…"), "{text}");
            let row = text.lines().find(|l| l.contains("plug ")).unwrap_or_default();
            assert!(row.contains("1.0.0") && row.contains("only shows things") && row.contains("not in catalog") && row.contains("not started"), "{row}");

            assert_eq!(rows(app.plugin_menu("plug", (1, 1))), ["Update plug…", "Show plug's log", "—", "Turn plug off", "—", "Remove plug…"]);
            app.run_action(Action::PluginOff, &MenuCtx::Plugin("plug".into()));
            assert!(crate::install::read_off().unwrap().contains_key("plug"));
            assert_eq!(rows(app.plugin_menu("plug", (1, 1)))[3], "Turn plug on");
            app.run_action(Action::PluginOn, &MenuCtx::Plugin("plug".into()));
            assert!(crate::install::read_off().unwrap().is_empty());

            let Overlay::Confirm(_, what) = app.run_action(Action::PluginRemove, &MenuCtx::Plugin("plug".into())) else { panic!("remove asks") };
            app.perform(what);
            assert!(!crate::paths::state_root().join("plugins/plug").exists(), "{:?}", app.notice);
            assert!(crate::install::read_lock().unwrap().is_empty());
        });
    }

    #[test]
    fn the_main_menu_follows_its_anatomy_inside_and_outside_tmux() {
        let mut app = with_one_space();
        let plain = [
            "New space…", "New tab [⌥+C]", "Navigator… [⌥+N]", "—",
            "Show file tree [⌥+T]", "Collapse sidebar [⌥+S]", "—",
            "Settings… [⌥+,]", "Keys and mouse… [⌥+K]", "Plugins…", "Reload plugins and agents", "—",
            "Messages…", "Routes map…", "Rename this server…", "Connect to another computer…", "Session history…", "—",
            "About nerd…", "Update… [⌥+U]", "Export logs…", "—",
            "Detach… [⌥+Q]", "Disconnect everyone…", "Stop this server…",
        ];
        assert_eq!(rows(app.global_menu((1, 1))), plain);
        app.caps.mux = true;
        let mut inside = strings(&plain);
        inside.insert(17, "Open outside tmux…".into());
        assert_eq!(rows(app.global_menu((1, 1))), inside, "last in its group");
    }

    #[test]
    fn the_plugins_window_stays_open_a_click_chooses_a_row_and_its_buttons_act_on_it() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        crate::session::tests::with_temp_state_dir("plugins-window-stays", || {
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 30);
            crate::install::place(crate::install::stage_add(&plugin_repo("plug").to_string_lossy(), None, true).unwrap()).unwrap();
            app.overlay = app.run_action(Action::Plugins, &MenuCtx::Global);
            let click = |x: u16, y: u16| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
            let s = screen(&mut app);
            let y = s.lines().position(|l| l.contains("plug ") && l.contains("only shows things")).expect("its row");
            let x = s.lines().nth(y).and_then(|l| l.chars().collect::<String>().find("plug ").map(|b| l[..b].chars().count())).expect("its column");
            app.mouse(click(x as u16 + 1, y as u16), app.screen);
            assert!(matches!(app.overlay, Overlay::Plugins(_)), "a click only chooses");
            let s = screen(&mut app);
            assert!(s.contains("[Enter] turn off") && s.contains("[u] update…") && s.contains("[l] show log") && s.contains("[d] remove…") && s.contains("[Esc] close"), "{s}");

            let off = app.view.dialog.panel.buttons[0];
            app.mouse(click(off.x + 1, off.y), app.screen);
            assert!(crate::install::read_off().unwrap().contains_key("plug"));
            assert!(matches!(app.overlay, Overlay::Plugins(_)), "turned off in place");
            let s = screen(&mut app);
            assert!(s.contains("[Enter] turn on"), "the same row is still chosen: {s}");
            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
            assert!(crate::install::read_off().unwrap().is_empty());

            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
            assert!(matches!(app.overlay, Overlay::Confirm(_, Pending::RemovePlugin(_))), "remove asks");
            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
            assert!(!crate::paths::state_root().join("plugins/plug").exists(), "{:?}", app.notice);
            assert!(matches!(app.overlay, Overlay::Plugins(_)), "back to the window once answered");
            assert!(app.plugins_back.is_none());
        });
    }

    #[test]
    fn the_plugins_window_fetches_in_place_asks_over_itself_and_says_why_it_failed_with_no_tab() {
        crate::session::tests::with_temp_state_dir("plugins-in-place", || {
            let press = |app: &mut App, code| app.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
            let corners = |s: &str| -> Vec<(usize, usize)> { s.lines().enumerate().flat_map(|(y, l)| l.chars().enumerate().filter(|(_, c)| "╭╯┌┘".contains(*c)).map(move |(x, _)| (x, y))).collect() };
            let mut app = with_one_space();
            app.screen = Rect::new(0, 0, 160, 30);
            let tabs = app.spaces[0].tabs.len();
            // Installed from an https address, as the window would: a local folder never reaches the window.
            staged_by_a_pane("plug");
            app.ask_staged_plugin();
            click_yes(&mut app);
            let root = crate::paths::state_root();
            assert!(root.join("plugins/plug").is_dir(), "{:?}", app.notice);
            app.overlay = app.plugins_overlay_at("plug");
            app.toast = None;
            let before = corners(&screen(&mut app));
            let exited = |app: &mut App| {
                let id = app.plugin_job.as_ref().expect("a fetch runs").exec;
                app.exec_note(crate::broker::ExecNote::Exited { id, how: None });
            };

            press(&mut app, KeyCode::Char('u'));
            let s = screen(&mut app);
            assert!(matches!(app.overlay, Overlay::Plugins(_)) && app.spaces[0].tabs.len() == tabs, "no tab");
            assert!(s.contains("updating…") && s.contains("Updating plug: fetching with git…") && s.contains("[Enter] cancel"), "{s}");
            assert_eq!(corners(&s), before, "the window keeps its size");

            // What the hidden fetch leaves: a newer release staged, and a ready.json.
            let repo = plugin_repo("plug");
            std::fs::write(repo.join("p.sh"), "exit 1\n").unwrap();
            let manifest = std::fs::read_to_string(repo.join("plugin.json")).unwrap().replace("1.0.0", "1.1.0");
            std::fs::write(repo.join("plugin.json"), manifest).unwrap();
            for args in [&["commit", "-qam", "two"][..], &["tag", "v1.1.0"]] {
                assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap().status.success());
            }
            let staged = root.join("plugin-staging/7-1");
            std::fs::create_dir_all(&staged).unwrap();
            let clone = std::process::Command::new("git").args(["clone", "-q", "--branch", "v1.1.0"]).arg(&repo).arg(staged.join("repo")).output().unwrap();
            assert!(clone.status.success());
            std::fs::write(staged.join("ready.json"), serde_json::json!({ "op": "update", "source": "https://github.com/me/plug.git", "tag": "v1.1.0" }).to_string()).unwrap();
            exited(&mut app);
            let Overlay::Confirm(c, Pending::PluginReady(..)) = &app.overlay else { panic!("the question opens at once: {:?}", app.notice) };
            let lines = c.body_lines(100).join("\n");
            assert!(lines.contains("release v1.1.0") && lines.contains("New commits:") && lines.contains("two"), "{lines}");
            assert!(app.plugin_job.is_none() && app.plugins_back.as_deref() == Some("plug"));
            click_yes(&mut app);
            assert_eq!(crate::install::read_lock().unwrap()[0].tag.as_deref(), Some("v1.1.0"), "{:?}", app.notice);
            let s = screen(&mut app);
            assert!(matches!(app.overlay, Overlay::Plugins(_)) && s.contains("[u] update…") && s.contains("1.1.0"), "back, the same row chosen: {s}");

            press(&mut app, KeyCode::Char('u'));
            let log = crate::install::fetch_log();
            std::fs::write(&log, "fatal: unable to access 'https://github.com/me/plug.git/': Could not resolve host: example.com\nnerd: git clone: fatal: unable to access 'https://github.com/me/plug.git/': Could not resolve host: example.com\n").unwrap();
            exited(&mut app);
            let s = screen(&mut app);
            assert!(s.contains("update failed") && s.contains("Update failed: git could not reach GitHub; is the network") && s.contains("[l] show log"), "{s}");
            assert!(matches!(app.overlay, Overlay::Plugins(_)) && app.spaces[0].tabs.len() == tabs);

            press(&mut app, KeyCode::Char('u'));
            std::fs::write(&log, "plug is up to date.\n").unwrap();
            exited(&mut app);
            let s = screen(&mut app);
            assert!(s.contains("Nothing newer: plug is up to date.") && matches!(app.overlay, Overlay::Plugins(_)), "said in the window, not asked: {s}");

            press(&mut app, KeyCode::Char('u'));
            press(&mut app, KeyCode::Enter);
            assert!(app.plugin_job.is_none() && matches!(app.overlay, Overlay::Plugins(_)), "cancelled in place");

            press(&mut app, KeyCode::Up);
            press(&mut app, KeyCode::Enter);
            assert!(screen(&mut app).contains("Refreshing the catalog: fetching with git…") && app.spaces[0].tabs.len() == tabs);
            std::fs::write(&log, "Catalog fetched: 0 plugins.\n").unwrap();
            exited(&mut app);
            assert!(screen(&mut app).contains("Catalog fetched: 0 plugins."));

            press(&mut app, KeyCode::Up);
            press(&mut app, KeyCode::Enter);
            assert!(matches!(app.overlay, Overlay::Prompt(_, Ask::PluginAddress)), "the address prompt stays");
            for ch in "https://github.com/me/new.git".chars() {
                press(&mut app, KeyCode::Char(ch));
            }
            press(&mut app, KeyCode::Enter);
            let s = screen(&mut app);
            assert!(matches!(app.overlay, Overlay::Plugins(_)) && s.contains("Adding github.com/me/new: fetching with git…") && app.spaces[0].tabs.len() == tabs, "{s}");
            std::fs::write(&log, "nerd: git clone: fatal: could not read Username for 'https://github.com': terminal prompts disabled\n").unwrap();
            exited(&mut app);
            let s = screen(&mut app);
            assert!(s.contains("GitHub has no public repository at that address"), "{s}");
            press(&mut app, KeyCode::Esc);
            assert!(app.plugin_job.is_none(), "what it said was read");
        });
    }

    #[test]
    fn the_main_menu_gives_plugins_their_own_group_and_none_when_there_are_none() {
        crate::session::tests::with_temp_state_dir("main-menu-plugin", || {
            let app = with_one_space();
            assert!(!rows(app.global_menu((1, 1))).contains(&"Global thing".to_string()));
            let dir = crate::paths::state_root().join("plugins/mainplug");
            std::fs::create_dir_all(&dir).unwrap();
            let m = serde_json::json!({
                "name": "mainplug", "title": "M", "exec": { "command": "sh", "args": ["x.sh"] },
                "commands": [{ "id": "g", "title": "Global thing", "where": "global" }], "events": ["command"]
            });
            std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
            crate::agents::reload();
            let r = rows(app.global_menu((1, 1)));
            let at = r.iter().position(|x| x == "Global thing").unwrap();
            assert_eq!((r[at - 2].as_str(), r[at - 1].as_str(), r[at + 1].as_str(), r[at + 2].as_str()), ("Export logs…", "—", "—", "Detach… [⌥+Q]"));
            std::fs::remove_dir_all(&dir).unwrap();
            crate::agents::reload();
        });
    }

    #[test]
    fn the_tab_menu_starts_a_tab_in_the_tabs_space_and_names_what_it_closes() {
        let app = with_one_space();
        assert_eq!(rows(app.tab_menu(0, (1, 1))), ["New tab [⌥+C]", "—", "Isolation for new panes…", "—", "Rename tab…", "Close tab…"]);
    }

    #[test]
    fn the_pane_menu_follows_its_anatomy_for_each_state() {
        // An empty home: no agent's hook is installed, so Claude Code's pane notes it.
        crate::config::tests::with_temp_home(|| {
            let mut app = with_one_space();
            let (p1, p2, p3) = (agent_pane(&app, 1, Default::default()), agent_pane(&app, 2, sub_running(1)), agent_pane(&app, 3, Default::default()));
            app.spaces[0].tabs[0].panes.insert(1, p1);
            app.spaces[0].tabs[1].layout.split(Dir::Horizontal, 3);
            app.spaces[0].tabs[1].panes.insert(2, p2);
            app.spaces[0].tabs[1].panes.insert(3, p3);
            let head = ["Split right [⌥+\\]", "Split down [⌥+-]", "—"];
            let mut one = strings(&head);
            let mail = ["~Send message…", "Show inbox…", "Who can write here…", "Where it can write…", "~Install the hook for exact delivery", "—", "Isolation…", "—"];
            one.extend(strings(&["~Full screen [⌥+Z]", "Search in pane [⌥+/]", "~Subagents…", "—"]));
            one.extend(strings(&mail));
            one.extend(strings(&["Rename pane…", "Close pane… [⌥+W]"]));
            assert_eq!(rows(app.pane_menu(1, (1, 1))), one, "one pane, an agent without subagents");
            let mut two = strings(&head);
            two.extend(strings(&["Full screen [⌥+Z]", "Search in pane [⌥+/]", "Subagents…", "—"]));
            two.extend(strings(&mail));
            two.extend(strings(&["Rename pane…", "Close pane… [⌥+W]"]));
            assert_eq!(rows(app.pane_menu(2, (1, 1))), two, "two panes, subagents running");
            app.spaces[0].tabs[1].layout.toggle_zoom();
            assert!(rows(app.pane_menu(2, (1, 1))).contains(&"Leave full screen [⌥+Z]".to_string()));
            app.spaces[0].tabs[1].panes.get_mut(&3).unwrap().agent = None;
            let none = rows(app.pane_menu(3, (1, 1)));
            assert!(none.iter().all(|r| !r.contains("Subagents")), "no agent, no item: {none:?}");
        });
    }

    #[test]
    fn the_space_menu_follows_its_anatomy_for_each_kind_of_folder() {
        let base = std::env::temp_dir().join(format!("nerd-space-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (plain, main, linked, other) = (base.join("plain"), base.join("main"), base.join("linked"), base.join("other"));
        for d in [&plain, &main, &linked, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir(main.join(".git")).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: x").unwrap();
        let mut app = App::new(Config::default());
        for (i, d) in [&plain, &main, &linked, &other].into_iter().enumerate() {
            app.spaces.push(Space { name: None, cwd: d.clone(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
            app.view.hits.sidebar_spaces.push((Rect::new(0, i as u16, 10, 1), i));
        }
        let menu = |app: &App, i| rows(app.space_menu(i, (1, 1)).unwrap());
        assert_eq!(menu(&app, 0), ["New tab in this space", "—", "Isolation for new panes…", "—", "Rename space…", "~Move up", "Move down", "—", "Close space…"], "top of the list, outside git");
        assert_eq!(
            menu(&app, 1),
            ["New tab in this space", "—", "New worktree…", "Open worktree…", "—", "Isolation for new panes…", "—", "Rename space…", "Move up", "Move down", "—", "Close space…"],
            "a main checkout alone"
        );
        assert_eq!(
            menu(&app, 2),
            ["New tab in this space", "—", "Isolation for new panes…", "—", "Rename space…", "Move up", "Move down", "—", "Close space…", "Delete worktree checkout…"],
            "a linked worktree"
        );
        let key = base.join("common");
        for i in [1, 2] {
            app.repos.insert(app.spaces[i].cwd.clone(), Repo { key: Some(key.clone()), main: i == 1, branch: None });
        }
        assert_eq!(
            menu(&app, 1),
            ["New tab in this space", "—", "New worktree…", "Open worktree…", "Collapse group", "—", "Isolation for new panes…", "—", "Rename space…", "Move up", "Move down", "—", "Close space…", "Close group…"],
            "a group"
        );
        app.collapsed_groups.insert(key);
        assert!(menu(&app, 1).contains(&"Expand group".to_string()));
        assert_eq!(menu(&app, 3).iter().filter(|r| r.starts_with('~')).collect::<Vec<_>>(), ["~Move down"], "bottom of the list");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_new_tab_from_the_space_menu_is_started_in_that_space() {
        let mut app = with_one_space();
        app.spaces.push(Space { name: None, cwd: std::env::temp_dir(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
        app.run_action(Action::NewTab, &MenuCtx::Space(1));
        // No broker in a unit test: the launch is tried and refused, and only the named space could have grown.
        assert!(app.notice.as_deref().is_some_and(|n| n.starts_with("failed to launch")), "{:?}", app.notice);
        assert_eq!((app.active, app.spaces[0].tabs.len()), (0, 2), "the active space is untouched");
    }

    #[test]
    fn the_tree_menu_follows_its_anatomy_for_each_state() {
        let (mut app, dir) = tree_on("tree-menu", &[("a.txt", "x")]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        app.cfg.preview = true;
        app.toggle_tree();
        let menu = |app: &App, path: &std::path::Path| rows(app.tree_menu(path.to_path_buf(), path.is_dir(), (1, 1)));
        let file = ["Open in editor [⌥+E]", "Open in its app [⌥+O]", "Insert path into pane [⌥+I]", "Copy path [⌥+Y]"];
        let folder = ["Go to this folder in the pane [⌥+D]", "Open as a space [⌥+P]", "Make it the root [⌥+=]", "New file or folder… [⌥+A]", "Open in Finder [⌥+O]", "Copy path [⌥+Y]"];
        let on = ["Full screen [z]", "~Next view [v]", "Wrap long lines [w]"];
        let off = ["~Full screen [z]", "~Next view [v]", "~Wrap long lines [w]"];
        let expect = |what: &[&str], view: &str, shape: &str, hidden: &str, zoom: &[&str]| -> Vec<String> {
            let mut v = strings(what);
            v.extend(strings(&["—", view, hidden, "Filter names…", "Search contents… [⌥+G]", shape, "—", "Root one level up [⌫]", "Reload [⌥+R]", "—"]));
            v.extend(strings(zoom));
            v.extend(strings(&["—", "Rename… [⌥+L]", "Delete… [⌥+X]", "—", "Close file tree [⌥+T]"]));
            v
        };
        let show = "Show hidden files [⌥+H]";
        assert_eq!(menu(&app, &dir.join("a.txt")), expect(&file, "Show changes", "~Changes as a list", show, &on), "a file, preview on");
        assert_eq!(menu(&app, &dir.join("sub")), expect(&folder, "Show changes", "~Changes as a list", show, &on), "a folder");
        app.cfg.preview = false;
        assert_eq!(menu(&app, &dir.join("a.txt")), expect(&file, "Show changes", "~Changes as a list", show, &off), "preview off greys both");
        app.tree.as_mut().unwrap().toggle_hidden();
        app.tree.as_mut().unwrap().set_view(crate::tree::View::Changes);
        assert_eq!(menu(&app, &dir.join("a.txt")), expect(&file, "Show files", "Changes as a list", "Hide hidden files [⌥+H]", &off), "changes view");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tree_menu_reaches_hidden_reload_and_root() {
        let (mut app, dir) = tree_on("tree-menu-act", &[]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        app.toggle_tree();
        let ctx = MenuCtx::Tree(dir.join("sub"));
        app.run_action(Action::TreeHidden, &ctx);
        assert!(app.tree.as_ref().unwrap().hidden());
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        app.run_action(Action::TreeReload, &ctx);
        assert!(screen(&mut app).contains("new.txt"), "reread from disk");
        app.run_action(Action::TreeRoot, &ctx);
        assert!(!screen(&mut app).contains("new.txt"), "the empty folder is the root now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pane_menu_has_no_file_tree_item() {
        // It opens at the space's folder, not the pane's: it stays in the global menu and its key.
        let app = with_one_space();
        let Overlay::Menu(menu, _) = app.pane_menu(1, (1, 1)) else { panic!("a pane menu expected") };
        assert!(menu.items.iter().all(|i| !i.label.contains("file tree")));
    }

    #[test]
    fn the_pane_menu_enables_full_screen_only_when_its_own_tab_is_split() {
        let mut app = with_one_space();
        // tab 0 (active) keeps its one pane; tab 1 gets a second one, but stays the inactive tab.
        let (p1, p2, p3) = (agent_pane(&app, 1, Default::default()), agent_pane(&app, 2, Default::default()), agent_pane(&app, 3, Default::default()));
        app.spaces[0].tabs[0].panes.insert(1, p1);
        app.spaces[0].tabs[1].layout.split(Dir::Horizontal, 3);
        app.spaces[0].tabs[1].panes.insert(2, p2);
        app.spaces[0].tabs[1].panes.insert(3, p3);
        app.spaces[0].active_tab = 0;

        let has_zoom = |app: &App, id| {
            let Overlay::Menu(menu, _) = app.pane_menu(id, (1, 1)) else { panic!("a pane menu expected") };
            menu.items.iter().any(|i| i.label == "Full screen" && i.enabled)
        };
        assert!(!has_zoom(&app, 1), "the only pane in its tab has nothing to fill the screen with");
        assert!(has_zoom(&app, 2), "its tab has two panes, even though another tab is active");
        assert!(has_zoom(&app, 3));
    }

    #[test]
    fn a_report_is_picked_by_period_and_saved_in_the_state_root() {
        crate::session::tests::with_temp_state_dir("report-menu", || {
            let mut app = with_one_space();
            let Overlay::Picker(picker, Pick::Report) = app.run_action(Action::Report, &MenuCtx::Global) else { panic!("a picker") };
            assert_eq!(picker.title, "Export logs");
            let Overlay::Confirm(c, Pending::ShowInFinder(path)) = app.picked(1, Pick::Report) else { panic!("it says where") };
            assert!(c.body.contains("nerd-report-") && c.body.ends_with(". Nothing was sent.") && path.exists(), "{}", c.body);
            assert_eq!(c.title, "Logs exported");
            app.caps.bye = true;
            assert!(matches!(app.picked(0, Pick::Report), Overlay::Confirm(_, Pending::Dismiss)), "a window from another Mac gets no Finder");
            let saved: Vec<_> = std::fs::read_dir(crate::paths::state_root().join("reports")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("nerd-report-")).collect();
            assert!(!saved.is_empty());
            assert!(app.notice.is_none());
        });
    }

    #[test]
    fn detaching_asks_first() {
        let mut app = with_one_space();
        app.key(alt('q'));
        assert!(app.quit.is_none(), "one key must not detach on its own");
        assert!(matches!(app.overlay, Overlay::Confirm(_, Pending::Detach)), "it asks");
    }

    #[test]
    fn from_the_tab_bar_shift_option_and_an_arrow_reaches_the_areas_beside_it() {
        let mut app = with_one_space();
        app.tree = Some(crate::tree::Tree::new(PathBuf::from("/")));
        app.view.tree_rect = Some(Rect::new(80, 1, 40, 30));

        app.focus = Focus::Tabs;
        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sidebar, "left of the tab bar is the sidebar");

        app.focus = Focus::Tabs;
        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.focus, Focus::Tree, "right of it is the tree, when the tree is open");

        // and it never switches tabs: that is what a bare arrow does there
        app.focus = Focus::Tabs;
        app.space_mut().unwrap().active_tab = 1;
        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.space_mut().unwrap().active_tab, 1, "the tab did not change");

        // nothing above the tab bar, and the modifier must not fall through to it
        app.focus = Focus::Tabs;
        app.key(shift_alt(KeyCode::Up));
        assert_eq!(app.focus, Focus::Tabs);
        assert_eq!(app.space_mut().unwrap().active_tab, 1);
    }

    #[test]
    fn shift_option_and_an_arrow_leaves_the_tree_and_never_walks_it() {
        let mut app = with_one_space();
        app.tree = Some(crate::tree::Tree::new(PathBuf::from("/")));
        app.view.tree_rect = Some(Rect::new(80, 1, 40, 30));
        app.focus = Focus::Tree;

        // no preview column: one step left hands the keys back to the panes
        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Panes, "option+left leaves the tree");

        app.focus = Focus::Tree;
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Right] {
            app.key(shift_alt(code));
            assert_eq!(app.focus, Focus::Tree, "option+{code:?} stays put");
        }
        // and the tree never saw those arrows: they are the modifier's, not its own
        assert!(app.tree.is_some(), "the tree is still open and still holds the keys");
    }

    #[test]
    fn option_and_sideways_walks_the_panes_before_it_leaves_them() {
        let mut app = with_one_space();
        app.sidebar = false;
        let (first, second) = (1, 7);
        if let Some(t) = app.tab_mut() {
            t.layout = Layout::new(first);
            t.layout.split(Dir::Horizontal, second);
        }
        assert_eq!(app.tab().unwrap().layout.focused(), second, "the new pane takes the keys");

        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Panes, "still in the panes");
        assert_eq!(app.tab().unwrap().layout.focused(), first, "and one pane to the left");

        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.tab().unwrap().layout.focused(), second, "and back to the right");

        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.tab().unwrap().layout.focused(), second, "nothing to the right of the last pane");
        assert_eq!(app.focus, Focus::Panes, "and no tree open to step into");
    }

    #[test]
    fn the_f_keys_belong_to_the_pane_again() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        let mut app = with_one_space();
        for n in 1..=12 {
            app.key(KeyEvent::new(KeyCode::F(n), KeyModifiers::NONE));
        }
        assert!(app.tree.is_none(), "no F key opens anything");
        assert!(matches!(app.overlay, Overlay::None), "and none of them opens an overlay");
    }

    #[test]
    fn shift_option_and_an_arrow_is_only_ever_for_leaving_an_area() {
        let mut app = with_one_space();
        app.key(shift_alt(KeyCode::Up));
        assert_eq!(app.focus, Focus::Tabs, "on the tab bar");

        // no tree open, so there is nothing to the right of the tab bar: the key does nothing
        // rather than falling through and switching tabs, which is what a bare arrow is for
        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.space_mut().unwrap().active_tab, 0, "the tab did not change");
        assert_eq!(app.focus, Focus::Tabs, "and nothing moved");

        // the bare arrow still picks the tab
        app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.space_mut().unwrap().active_tab, 1, "a bare arrow picks the next tab");

        // and the way out is still there
        app.key(shift_alt(KeyCode::Down));
        assert_eq!(app.focus, Focus::Panes);
    }

    #[test]
    fn a_step_at_an_edge_stops_rather_than_wrapping() {
        let mut app = with_one_space();
        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sidebar);
        app.key(shift_alt(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sidebar, "left of the sidebar is nothing, not the far right");

        app.key(shift_alt(KeyCode::Right));
        app.key(shift_alt(KeyCode::Right));
        assert_eq!(app.focus, Focus::Panes, "no tree open, so right of the panes is nothing");
    }

    #[test]
    fn esc_returns_to_the_panes_from_any_area() {
        use crossterm::event::KeyEvent;
        for step in [KeyCode::Left, KeyCode::Up] {
            let mut app = with_one_space();
            app.key(shift_alt(step));
            assert_ne!(app.focus, Focus::Panes, "the step moved somewhere");
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_eq!(app.focus, Focus::Panes, "esc comes back from {:?}", step);
        }
    }

    #[test]
    fn bare_arrows_pick_a_tab_once_the_tab_bar_has_the_keys() {
        use crossterm::event::KeyEvent;
        let bare = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let mut app = with_one_space();
        app.key(shift_alt(KeyCode::Up));
        assert_eq!(app.focus, Focus::Tabs);

        app.key(bare(KeyCode::Right));
        assert_eq!(app.space_mut().unwrap().active_tab, 1, "no modifier needed inside the bar");
        app.key(bare(KeyCode::Right));
        assert_eq!(app.space_mut().unwrap().active_tab, 1, "and it stops at the last one");
        app.key(bare(KeyCode::Left));
        assert_eq!(app.space_mut().unwrap().active_tab, 0);

        app.key(bare(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Panes, "return takes the keys back to the pane");
    }

    #[test]
    fn the_offer_is_made_once_per_client_and_never_outside_a_multiplexer() {
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::Confirm(..)), "a choice is offered inside tmux");

        app.overlay = Overlay::None;
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::None), "the same client is not asked twice");
        app.offer_outside_tmux(2);
        assert!(matches!(app.overlay, Overlay::Confirm(..)), "a new client is asked");

        let mut app = inside_tmux(TmuxOffer::Ask);
        app.caps.mux = false;
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::None), "outside a multiplexer there is nothing to offer");

        let mut app = inside_tmux(TmuxOffer::Stay);
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::None), "stay: no offer");

        // Mid-work overlays are not interrupted.
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.overlay = Overlay::Settings(crate::settings::Settings::new(&app.cfg));
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::Settings(_)));
    }

    #[test]
    fn the_client_is_asked_when_it_can_open_the_window_itself() {
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.caps.opens_windows = true;
        app.default_state_root = false;
        app.open_outside_tmux();
        assert_eq!(app.to_client, crate::proto::OPEN_WINDOW, "the request goes to the client");
        assert!(app.opening.is_some(), "and the wait for it starts");
        assert!(app.notice.is_none(), "a test server is no obstacle: the client passes its own environment on");
    }

    #[test]
    fn a_server_outside_the_default_state_directory_offers_the_command() {
        let mut app = inside_tmux(TmuxOffer::NewWindow);
        app.default_state_root = false;
        app.offer_outside_tmux(1);
        let Overlay::Confirm(dialog, what) = &app.overlay else { panic!("a window cannot be opened, so it asks") };
        assert!(dialog.body.contains("nerd --resume"), "the command is the way out: {}", dialog.body);
        assert!(matches!(what, Pending::CopyResumeCommand));
        assert!(app.opening.is_none());
    }

    #[test]
    fn new_window_without_a_terminal_asks_instead_of_failing() {
        let mut app = inside_tmux(TmuxOffer::NewWindow);
        app.caps.terminal = None;
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::Confirm(..)), "nothing to open: ask");
        assert!(app.notice.is_none(), "no complaint on screen");
    }

    #[test]
    fn the_offer_says_why_it_opened_and_what_it_changes() {
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.offer_outside_tmux(1);
        let Overlay::Confirm(dialog, what) = &app.overlay else { panic!("a dialog") };
        assert_eq!(dialog.title, "nerd is inside tmux");
        assert!(dialog.body.contains("carries no graphics"), "the reason: {}", dialog.body);
        assert!(dialog.body.contains("Ghostty"), "where it will open: {}", dialog.body);
        assert!(dialog.body.contains("stay as they are"), "what it does not touch: {}", dialog.body);
        assert!(matches!(what, Pending::LeaveTmux));
    }

    #[test]
    fn a_client_nerd_opened_itself_asks_instead_of_opening_another() {
        let mut app = inside_tmux(TmuxOffer::NewWindow);
        app.caps.opened_by_nerd = true;
        app.offer_outside_tmux(1);
        assert!(matches!(app.overlay, Overlay::Confirm(..)), "the chain stops at one window");
        assert!(app.opening.is_none());
    }

    #[test]
    fn a_stray_enter_stays_where_it_is() {
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.offer_outside_tmux(1);
        let Overlay::Confirm(mut dialog, _) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("a dialog") };
        let enter = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        assert_eq!(dialog.key(enter), crate::menu::ConfirmOutcome::No, "the harmless answer is the selected one");
    }

    #[test]
    fn naming_the_instance_writes_the_label_and_retitles_the_window() {
        crate::session::tests::with_temp_state_dir("app-rename", || {
            std::fs::create_dir_all(crate::session::instances_dir().join("1")).unwrap();
            let mut app = App::new(Config::default());
            app.run_ask("work".into(), &Ask::RenameInstance("1".into()));
            assert_eq!(crate::session::instance_label("1"), "work");
            assert_eq!(app.to_client, b"\x1b]2;nerd work\x07", "the window title changes at once");
            assert!(app.notice.is_none());

            app.to_client.clear();
            app.run_ask("12".into(), &Ask::RenameInstance("1".into()));
            assert_eq!(crate::session::instance_label("1"), "work", "a refused name leaves the old one");
            assert!(app.notice.as_deref().unwrap_or_default().contains("not renamed"), "{:?}", app.notice);
            assert!(app.to_client.is_empty());

            // Shown on another Mac: the title keeps saying which machine it is.
            app.caps.machine = Some("studio".into());
            app.run_ask("home".into(), &Ask::RenameInstance("1".into()));
            assert_eq!(app.to_client, b"\x1b]2;nerd home @ studio\x07");
        });
    }

    #[test]
    fn the_command_line_is_the_one_open_needs() {
        let argv = open_argv(
            std::path::Path::new("/Applications/Ghostty.app"),
            std::path::Path::new("/usr/local/bin/nerd"),
            "3",
        );
        assert_eq!(
            argv,
            [
                "/usr/bin/open",
                "-na",
                "/Applications/Ghostty.app",
                "--args",
                "-e",
                "/usr/local/bin/nerd",
                "--resume",
                "3",
                "--from-window"
            ]
        );
    }

    #[test]
    fn a_window_that_never_appears_leaves_the_command_on_screen() {
        let mut app = inside_tmux(TmuxOffer::Ask);
        app.opening = Some(Opening { pty: None, until: Instant::now() + Duration::from_secs(5), out: Vec::new(), app: "/Applications/Ghostty.app".into() });
        assert!(!app.tick_opening(), "the wait is not over");
        assert!(app.notice.is_none());

        app.opening = Some(Opening { pty: None, until: Instant::now() - Duration::from_secs(1), out: b"Unable to find application".to_vec(), app: "/Applications/Ghostty.app".into() });
        assert!(app.tick_opening(), "the wait ran out");
        assert!(app.notice.as_deref().unwrap_or_default().contains("nerd --resume"), "{:?}", app.notice);
        assert!(app.opening.is_none());
        assert!(!app.tick_opening(), "said once");
    }

    #[test]
    fn a_restored_agent_is_a_command_typed_into_a_shell() {
        let zsh = vec!["/bin/zsh".to_string()];
        assert_eq!(super::restored_command(&zsh, None, false), None, "a plain shell types nothing");
        let id = "0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c";
        assert_eq!(
            super::restored_command(&zsh, Some(("claude", id)), false),
            Some(vec!["claude".into(), format!("--resume={id}")]),
            "its own session comes back"
        );
        assert_eq!(
            super::restored_command(&zsh, Some(("claude", "--dangerously-skip-permissions")), false),
            Some(vec!["claude".into()]),
            "a stored value that is not a session id never becomes a flag"
        );
        let agent = vec!["claude".to_string(), "a; b".to_string()];
        assert_eq!(super::restored_command(&agent, Some(("claude", "abc")), false), Some(agent.clone()), "a bad id falls back to the stored agent");
        assert_eq!(super::restore_line(&["claude".into(), format!("--resume={id}")]), Some(format!("claude --resume={id}\n")), "typed bare, so an alias still expands");
        assert_eq!(super::restore_line(&agent), Some("claude 'a; b'\n".into()), "one argument stays one argument");
        assert_eq!(super::restore_line(&["claude".into(), "a\x15b".into()]), None, "a control character is never typed");
        assert_eq!(
            super::restored_command(&zsh, Some(("claude", id)), true),
            Some(vec!["claude".into()]),
            "a session held elsewhere is never resumed twice"
        );
        let stale = vec!["claude".to_string(), "--resume".to_string(), "gone".to_string()];
        assert_eq!(
            super::restored_command(&stale, None, false),
            Some(vec!["claude".into()]),
            "an old snapshot's agent starts fresh, not on a session that moved on"
        );
        assert_eq!(super::restored_command(&zsh, Some(("codex", id)), false), Some(vec!["codex".into(), "resume".into(), id.into()]), "each agent by its own line");
        assert_eq!(super::restored_command(&zsh, Some(("opencode", "ses_1")), false), Some(vec!["opencode".into(), "--session".into(), "ses_1".into()]));
        assert_eq!(super::restored_command(&zsh, Some(("cursor", id)), false), Some(vec!["cursor-agent".into()]), "Cursor does not resume: it starts fresh");
        assert_eq!(super::restored_command(&zsh, Some(("gone", id)), false), None, "an agent with no manifest now: the shell only");
        let gem = vec!["gemini".to_string(), "--resume".to_string(), "x".to_string()];
        assert_eq!(super::restored_command(&gem, None, false), Some(vec!["gemini".into()]), "its own flags stripped");
    }

    #[test]
    fn a_restarted_agent_starts_again_without_a_session() {
        let zsh = vec!["/bin/zsh".to_string(), "-l".to_string()];
        assert_eq!(super::restart_command(&zsh, None, Some("pi")), Some(vec!["pi".into()]), "the agent it ran, started fresh");
        assert_eq!(super::restart_command(&zsh, None, None), None, "a plain shell types nothing");
        assert_eq!(super::restart_command(&zsh, None, Some("gone")), None, "an agent with no manifest: the shell only");
        assert_eq!(super::restart_command(&zsh, Some(("pi", "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b")), Some("pi")), Some(vec!["pi".into(), "--session".into(), "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b".into()]), "its session resumed");
        let gem = vec!["gemini".to_string(), "--resume".to_string(), "x".to_string()];
        assert_eq!(super::restart_command(&gem, None, None), Some(vec!["gemini".into()]), "a pane started as the agent");
    }

    #[test]
    fn an_option_letter_means_one_thing_everywhere() {
        // The tree answers `option` letters of its own; none of them may be a global one, or
        // the global would swallow it before the tree ever saw it.
        for r in crate::tree::COMMAND.iter().filter(|r| r.alt) {
            if let KeyCode::Char(c) = r.code {
                assert!(
                    super::global_of(crate::keys::latin(c)).is_none(),
                    "{} is a global letter too",
                    r.label
                );
            }
        }
    }

    #[test]
    fn user_shell_comes_from_environment() {
        let sh = super::user_shell();
        assert!(sh.starts_with('/'), "the shell is given as a full path: {sh}");
    }

    /// A pane whose output the test feeds: the sender plays its reader, `master` takes its writes.
    fn fed_pane(app: &App, id: PaneId, master: Option<std::fs::File>) -> (Pane, std::sync::mpsc::SyncSender<Vec<u8>>) {
        let master = master.unwrap_or_else(|| std::fs::OpenOptions::new().write(true).open("/dev/null").unwrap());
        let (pty, tx) = Pty::fake(id, master);
        let mut pane = agent_pane(app, id, Default::default());
        pane.pty = pty;
        (pane, tx)
    }

    #[test]
    fn a_flooding_pane_does_not_hold_back_a_quiet_one() {
        let mut app = with_one_space();
        let (flood, ftx) = fed_pane(&app, 1, None);
        let (quiet, qtx) = fed_pane(&app, 2, None);
        app.spaces[0].tabs[0].panes.insert(1, flood);
        app.spaces[0].tabs[1].panes.insert(2, quiet);
        for _ in 0..crate::pty::QUEUE {
            ftx.send(vec![b'y'; 8192]).unwrap();
        }
        qtx.send(b"quiet".to_vec()).unwrap();
        assert_eq!(app.pump_pty(Duration::ZERO), (true, true), "one round leaves the flood queued");
        assert!(app.pane(2).unwrap().term.screen().contents().contains("quiet"), "the quiet pane had its turn");
        let p = app.spaces[0].tabs[0].panes.get_mut(&1).unwrap();
        assert_eq!(std::iter::from_fn(|| p.pty.next()).count(), crate::pty::QUEUE - QUANTUM, "the flood gave up one quantum");
    }

    #[test]
    fn each_chunk_is_answered_from_where_it_left_the_cursor() {
        use std::io::Read;
        let mut app = with_one_space();
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (pane, tx) = fed_pane(&app, 1, Some(std::fs::File::from(std::os::fd::OwnedFd::from(theirs))));
        app.spaces[0].tabs[0].panes.insert(1, pane);
        tx.send(b"ab\x1b[6n".to_vec()).unwrap();
        tx.send(b"\r\n\x1b[6n".to_vec()).unwrap();
        app.pump_pty(Duration::ZERO);
        ours.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut got = Vec::new();
        while got.len() < 12 {
            let mut b = [0u8; 64];
            let n = (&ours).read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, b"\x1b[1;3R\x1b[2;1R", "two chunks, two cursors");
    }

    #[test]
    fn an_exit_waits_for_the_last_output_of_its_pane() {
        let mut app = with_one_space();
        let (pane, tx) = fed_pane(&app, 1, None);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        tx.send(b"queued before the notice\r\n".to_vec()).unwrap();
        app.exited((1, None));
        assert!(!app.settle_exits(), "its reader is still there");
        tx.send(b"resume with: claude --resume x".to_vec()).unwrap();
        assert!(!app.settle_exits());
        let text = app.pane(1).unwrap().term.screen().contents();
        assert!(text.contains("queued before the notice") && text.contains("claude --resume x"), "{text}");
        drop(tx);
        assert!(app.settle_exits(), "the reader ended: the pane goes at once");
        assert!(app.pane(1).is_none());

        // A background job can hold the terminal open: its reader never ends, so the wait does.
        let (pane, _held) = fed_pane(&app, 2, None);
        app.spaces[0].tabs[0].panes.insert(2, pane);
        app.exits.push((2, Instant::now()));
        assert!(app.settle_exits());
        assert!(app.pane(2).is_none());
    }

    #[test]
    fn a_turn_reads_each_screen_once_for_the_state() {
        let mut app = with_one_space();
        for (tab, id) in [(0, 1), (1, 2)] {
            let mut pane = agent_pane(&app, id, Default::default());
            pane.term.set_blocked_text(crate::agents::blocked_text(Some("claude")));
            pane.term.feed(b"working");
            app.spaces[0].tabs[tab].panes.insert(id, pane);
        }
        crate::term::CONTENTS.with(|c| c.set(0));
        app.tick_states();
        let mut tui = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        tui.draw(|f| {
            crate::ui::frame(f, &mut app);
        })
        .unwrap();
        assert_eq!(crate::term::CONTENTS.with(|c| c.get()), 2, "one per pane, ticks and frame together");
    }

    #[test]
    fn a_second_notice_for_one_pane_does_not_close_the_shell_that_replaced_it() {
        let mut app = with_one_space();
        let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone = dead.id();
        dead.wait().unwrap();
        let (mut pane, _tx) = fed_pane(&app, 1, None);
        pane.pty.child_pid = Some(gone);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.exited((1, None));
        app.exited((1, None));
        assert_eq!(app.exits.len(), 1, "one pane, one pending exit");

        // The shell given back has a live process: a notice for the one before it is stale.
        app.exits.clear();
        app.spaces[0].tabs[0].panes.get_mut(&1).unwrap().pty.child_pid = Some(std::process::id());
        app.exited((1, None));
        assert!(app.exits.is_empty());
        assert!(!app.settle_exits() && app.pane(1).is_some());
    }

    /// A pane whose process the broker has not answered for yet.
    fn starting_pane(app: &App, id: PaneId, argv: &[&str]) -> Pane {
        Pane { argv: argv.iter().map(|s| s.to_string()).collect(), pty: Pty::waiting(id, 80, 24, app.bell.clone()), ..agent_pane(app, id, Default::default()) }
    }

    fn master() -> (std::os::fd::OwnedFd, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        (ours.into(), theirs)
    }

    #[test]
    fn an_answer_attaches_its_pane_and_an_early_exit_is_decided_then() {
        let mut app = with_one_space();
        let pane = starting_pane(&app, 1, &["/bin/sh"]);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        app.exited((1, None));
        assert!(app.exits.is_empty(), "held: the pid is not known yet");
        assert!(!app.settle_exits() && app.pane(1).is_some(), "a starting pane is left alone");
        let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone = dead.id();
        dead.wait().unwrap();
        app.spawned((1, Ok((Some(gone), master().0))));
        assert!(!app.pane(1).unwrap().pty.starting());
        assert_eq!(app.exits.len(), 1, "the held notice names a dead process");
    }

    #[test]
    fn an_answer_nobody_waits_for_closes_its_master() {
        use std::io::Read;
        let mut app = with_one_space();
        let (fd, mut theirs) = master();
        app.spawned((9, Ok((None, fd))));
        // macOS refuses the option on a socket already closed at the other end; the read then ends at once.
        let _ = theirs.set_read_timeout(Some(Duration::from_secs(2)));
        assert_eq!(theirs.read(&mut [0u8; 8]).unwrap(), 0, "the master was dropped");
        assert!(app.pane(9).is_none() && app.notice.is_none());
    }

    #[test]
    fn a_failed_spawn_closes_the_pane_and_says_why() {
        crate::session::tests::with_temp_state_dir("spawn-failed", || {
            let mut app = with_one_space();
            // An agent pane: a failure must close it, not start a shell in its place.
            let pane = starting_pane(&app, 1, &["claude"]);
            app.spaces[0].tabs[0].panes.insert(1, pane);
            app.spawned((1, Err("no such file".into())));
            assert!(app.pane(1).is_none());
            assert_eq!(app.notice.as_deref(), Some("failed to launch claude: no such file"));
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default();
            assert!(log.contains("pane 1: failed to launch claude: no such file"), "{log}");
            assert!(!log.contains("would not start"), "no shell was tried: {log}");
        });
    }

    #[test]
    fn a_stray_answer_for_a_live_pane_leaves_its_process_alone() {
        let mut app = with_one_space();
        let (pane, _tx) = fed_pane(&app, 1, None);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        assert!(!app.spawned((1, Ok((None, master().0)))), "a live pane's id is never sent away");
        assert!(app.pane(1).is_some());
        assert!(app.spawned((9, Ok((None, master().0)))), "nobody waits for 9");
    }

    #[test]
    fn a_failed_last_pane_does_not_detach_and_a_fresh_space_is_tried_once() {
        let mut app = App::new(Config::default());
        let pane = starting_pane(&app, 1, &["/bin/sh"]);
        app.spaces.push(Space { name: None, cwd: PathBuf::from("/nonexistent"), tabs: vec![Tab { name: None, layout: Layout::new(1), panes: HashMap::from([(1, pane)]), isolation_new: None }], active_tab: 0, isolation_new: None });
        app.next_id = 2;
        app.spawned((1, Err("no such directory".into())));
        assert!(app.quit.is_none(), "the client stays");
        // No broker in a test: the fresh space's shell fails at once, and nothing loops.
        assert_eq!((app.fresh, app.next_id), (Some(2), 3), "one fresh space was tried");
        assert!(app.spaces.is_empty());
        let pane = starting_pane(&app, 2, &["/bin/sh"]);
        app.spaces.push(Space { name: None, cwd: PathBuf::from("/"), tabs: vec![Tab { name: None, layout: Layout::new(2), panes: HashMap::from([(2, pane)]), isolation_new: None }], active_tab: 0, isolation_new: None });
        app.spawned((2, Err("again".into())));
        assert_eq!(app.next_id, 3, "the fresh space failing is not tried again");
        assert!(app.quit.is_none());
    }

    #[test]
    fn ids_continue_past_a_handover_so_a_late_answer_finds_no_new_pane() {
        let mut app = App::new(Config::default());
        let h: crate::session::Handover = serde_json::from_value(serde_json::json!({"snapshot": {"version": 2, "spaces": []}, "panes": [], "broker_fd": 3, "listener_fd": 4, "client": null, "next_id": 10})).unwrap();
        app.adopt(&h);
        assert_eq!(app.alloc_id(), 10);
        let old: crate::session::Handover = serde_json::from_value(serde_json::json!({"snapshot": {"version": 2, "spaces": []}, "panes": [], "broker_fd": 3, "listener_fd": 4, "client": null})).unwrap();
        assert_eq!(old.next_id, 0, "an older build's handover reads as before");
    }

    #[test]
    fn a_silent_broker_fails_starting_panes_and_any_answer_resets_the_wait() {
        let mut app = with_one_space();
        let pane = starting_pane(&app, 1, &["/bin/sh"]);
        app.spaces[0].tabs[0].panes.insert(1, pane);
        let pane = starting_pane(&app, 2, &["/bin/sh"]);
        app.spaces[0].tabs[1].panes.insert(2, pane);
        assert!(!app.tick_spawns(), "within the wait");
        for id in [1, 2] {
            app.pane_mut(id).unwrap().pty.overdue();
        }
        app.spawned((2, Ok((None, master().0))));
        assert!(!app.tick_spawns() && app.pane(1).is_some(), "the broker answered, so pane 1 waits again");
        assert!(app.spawn_wait().is_some());
        app.pane_mut(1).unwrap().pty.overdue();
        assert!(app.tick_spawns());
        assert!(app.pane(1).is_none(), "the silent broker failed it");
        assert!(app.notice.as_deref().unwrap_or_default().contains("the broker did not answer"), "{:?}", app.notice);
        assert!(app.spawn_wait().is_none(), "nothing waits");
    }

    #[test]
    fn the_open_helper_gets_its_answer_by_id() {
        let mut app = with_one_space();
        let op = |pty| Opening { pty: Some(pty), until: Instant::now() + Duration::from_secs(5), out: Vec::new(), app: "/Applications/Ghostty.app".into() };
        app.opening = Some(op(Pty::waiting(5, 1, 1, app.bell.clone())));
        assert!(app.spawn_wait().is_some(), "an Update waits for it too");
        app.spawned((5, Ok((None, master().0))));
        assert!(app.opening.as_ref().and_then(|o| o.pty.as_ref()).is_some_and(|p| !p.starting()));
        app.opening = Some(op(Pty::waiting(6, 1, 1, app.bell.clone())));
        app.spawned((6, Err("denied".into())));
        assert!(app.opening.is_none());
        assert!(app.notice.as_deref().unwrap_or_default().contains("could not start the terminal: denied"), "{:?}", app.notice);
    }

    #[test]
    fn a_program_that_asks_but_never_reads_loses_replies_not_memory() {
        crate::session::tests::with_temp_state_dir("unread", || {
            let mut app = with_one_space();
            let (_unread, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            let (pane, tx) = fed_pane(&app, 1, Some(std::fs::File::from(std::os::fd::OwnedFd::from(theirs))));
            app.spaces[0].tabs[0].panes.insert(1, pane);
            let log = || std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default();
            // Each chunk asks 2000 times: a 12 KB reply, so the socket fills in a few.
            let mut first_drop = None;
            for i in 0..crate::pty::WRITES + 64 {
                tx.send(b"\x1b[6n".repeat(2000)).unwrap();
                app.pump_pty(Duration::ZERO);
                if first_drop.is_none() && log().contains("replies dropped") {
                    first_drop = Some(i);
                }
            }
            let at = first_drop.expect("the queue never filled");
            assert!(at <= crate::pty::WRITES + 8, "the queue held {at} replies");
            assert_eq!(log().matches("replies dropped").count(), 1, "one line per pane");
            app.key(crossterm::event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            app.tick_states();
            assert_eq!(app.notice.as_deref(), Some("pane is not reading input"));
        });
    }

    #[test]
    fn a_pane_that_keeps_printing_does_not_hold_an_update_or_a_stop() {
        crate::session::tests::with_temp_state_dir("refill", || {
            let mut app = with_one_space();
            let (pane, tx) = fed_pane(&app, 1, None);
            app.spaces[0].tabs[0].panes.insert(1, pane);
            let producer = std::thread::spawn(move || while tx.send(b"y\r\n".to_vec()).is_ok() {});
            assert_eq!(app.handover_panes().unwrap().len(), 1);
            app.dump_screens();
            drop(app);
            producer.join().unwrap();
        });
    }
}

/// A pane on screen: border, interior, and space for text.
pub struct PaneGeometry {
    pub id: PaneId,
    pub rect: Rect,
    pub inner: Rect,
    pub text: Rect,
    /// Offset and how many lines above, if a scrollbar is needed.
    pub scrollbar: Option<(usize, usize)>,
}

#[cfg(test)]
mod theme_tests {
    use crate::config::ThemeMode;
    use crate::render::Theme;

    #[test]
    fn the_terminal_background_follows_the_theme() {
        // derived from the theme, so re-picking a palette does not have to be repeated here
        fn osc11(t: Theme) -> String {
            let ratatui::style::Color::Rgb(r, g, b) = t.ground else { panic!("ground is not an exact color") };
            format!("\x1b]11;#{r:02x}{g:02x}{b:02x}\x07")
        }
        let mut app = super::App::new(crate::config::Config::default());
        app.to_client.clear();
        app.apply_theme();
        assert!(String::from_utf8_lossy(&app.to_client).contains(&osc11(Theme::dark())), "dark ground");
        app.cfg.theme = ThemeMode::Light;
        app.to_client.clear();
        app.apply_theme();
        assert!(String::from_utf8_lossy(&app.to_client).contains(&osc11(Theme::light())), "light ground");
    }

    #[test]
    fn auto_follows_the_appearance_and_falls_back_to_dark() {
        let ground = |m, d| {
            let cfg = crate::config::Config { theme: m, ..Default::default() };
            crate::render::pick_theme(&cfg, d).0.ground
        };
        assert_eq!(ground(ThemeMode::Auto, Some(false)), Theme::light().ground);
        assert_eq!(ground(ThemeMode::Auto, Some(true)), Theme::dark().ground);
        assert_eq!(ground(ThemeMode::Auto, None), Theme::dark().ground, "unknown appearance");
        assert_eq!(ground(ThemeMode::Dark, Some(false)), Theme::dark().ground, "a fixed choice wins");
        assert_eq!(ground(ThemeMode::Light, Some(true)), Theme::light().ground);
    }
}

