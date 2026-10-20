//! Multiplexer config and autostart via launchd.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::config_path;

#[derive(Serialize, Deserialize, Clone, Debug)]
// Field defaults come from Config::default() below, not from each type's zero value.
#[serde(default)]
pub struct Config {
    pub sidebar_width: u16,
    pub pane_gap: u16,
    pub scrollback: usize,
    /// Theme colors by role name, over the built-in palette: `accent`, `ground`, `text`,
    /// `ansi0`..`ansi15` and the rest of `Theme`'s fields. Applied live; anything absent or
    /// misspelled keeps the built-in value.
    pub colors: std::collections::BTreeMap<String, String>,
    pub mouse: bool,
    pub sidebar_visible: bool,
    pub scrollbar: bool,
    /// nerd's clock at the end of the top row: off, the time, or the date and the time as the menu bar writes them;
    /// a click switches the two.
    pub clock: Clock,
    /// Your own patterns for the time and the date (`HH:mm:ss`, `EEE d MMMM`); empty: as the menu bar clock.
    pub clock_time: String,
    pub clock_date: String,
    /// The battery before the clock: off, its charge, or the time left to empty or to full; a click switches the two.
    pub battery: Battery,
    /// On battery at this percent or less the charge turns yellow, and red at `battery_critical`.
    pub battery_low: u8,
    pub battery_critical: u8,
    pub pane_border: BorderMode,
    pub tree_width: u16,
    /// If the preview doesn't fit next to the tree, it goes under it instead.
    pub preview_width: u16,
    pub notify: Notify,
    /// Network-less Seatbelt sandbox for the daemon; doesn't affect panes, which the
    /// broker launches before the restriction applies. Read once at startup only.
    pub sandbox: bool,
    /// Replays each pane's text dump on the next start; off also clears the last dump.
    pub restore_screens: bool,
    /// File preview column next to the tree; off hides it and full screen with it.
    pub preview: bool,
    /// Long lines in the preview wrap; off, they run past the edge and scroll sideways.
    pub preview_wrap: bool,
    pub images: ImageMode,
    pub paths: PathStyle,
    pub tmux: TmuxOffer,
    pub theme: ThemeMode,
    pub prefix: Prefix,
    /// Macs for Connect to... and `nerd --remote`, kept exactly as written: read through `remote_entries`.
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub remotes: serde_json::Value,
    /// Per-plugin settings, kept exactly as written; nerd reads nothing from it yet.
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub plugins: serde_json::Value,
}

/// One machine as the human wrote it; a bad one is shown with its reason and never reaches ssh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteEntry {
    pub name: String,
    pub host: String,
}

impl Config {
    /// Every entry of `remotes`, usable or not; anything but a list gives none.
    pub fn remote_entries(&self) -> Vec<RemoteEntry> {
        let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        self.remotes.as_array().map_or_else(Vec::new, |list| list.iter().map(|v| RemoteEntry { name: text(v, "name"), host: text(v, "host") }).collect())
    }
}

impl RemoteEntry {
    /// Why this entry cannot be used, if it cannot: the host goes into ssh's argv after `--`.
    pub fn check(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty() || n.len() > 16 || !n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
            return Err("a name is 1 to 16 letters, digits, '.', '_' or '-'".into());
        }
        let h = &self.host;
        if h.is_empty() {
            return Err("no host, or a host that is not text".into());
        }
        if h.starts_with('-') {
            return Err("a host cannot start with '-'".into());
        }
        if h.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("a host has no spaces or control characters".into());
        }
        Ok(())
    }
}

/// What to do when nerd starts inside tmux or screen, where images cannot be drawn in pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TmuxOffer {
    /// Offer a window outside the multiplexer, once per attach.
    Ask,
    NewWindow,
    Stay,
}

/// The tmux-style chord: this key with control, then the letter `option` takes.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Prefix {
    Off,
    CtrlB,
    CtrlA,
}

impl Prefix {
    pub fn letter(self) -> Option<char> {
        match self {
            Prefix::Off => None,
            Prefix::CtrlB => Some('b'),
            Prefix::CtrlA => Some('a'),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Prefix::Off => "off",
            Prefix::CtrlB => "⌃+B",
            Prefix::CtrlA => "⌃+A",
        }
    }
}

/// Which variant of the nerd theme draws the screen.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// The macOS appearance, as the client reports it; dark when it is unknown.
    Auto,
    Dark,
    Light,
}

/// `Never` leaves neighbors separated only by the layout gap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderMode {
    Always,
    Multiple,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", from = "ClockRead")]
pub enum Clock {
    Off,
    Time,
    Date,
}

/// What `clock` held up to 0.8.0, `true` or `false`, still reads; it is written back as the word.
#[derive(Deserialize)]
#[serde(untagged)]
enum ClockRead {
    Released(bool),
    Word(ClockWord),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ClockWord {
    Off,
    Time,
    Date,
}

impl From<ClockRead> for Clock {
    fn from(r: ClockRead) -> Clock {
        match r {
            ClockRead::Released(true) | ClockRead::Word(ClockWord::Time) => Clock::Time,
            ClockRead::Released(false) | ClockRead::Word(ClockWord::Off) => Clock::Off,
            ClockRead::Word(ClockWord::Date) => Clock::Date,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Battery {
    Off,
    Percent,
    Time,
}

/// How image files show in the preview.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    /// Pixels where the terminal supports Kitty graphics, blocks elsewhere.
    Auto,
    Blocks,
    Off,
}

/// How a path is shown: from home (`~/dev/nerd`) or in full (`/Users/<name>/dev/nerd`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PathStyle {
    Home,
    Full,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Notify {
    pub sound: bool,
    pub bell: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self { sound: true, bell: false }
    }
}

pub fn home_dir() -> PathBuf {
    crate::paths::home().unwrap_or_else(|| PathBuf::from("/"))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sidebar_width: 34,
            pane_gap: 1,
            scrollback: 10000,
            colors: std::collections::BTreeMap::new(),
            mouse: true,
            sidebar_visible: true,
            scrollbar: true,
            clock: Clock::Time,
            clock_time: String::new(),
            clock_date: String::new(),
            battery: Battery::Percent,
            battery_low: 20,
            battery_critical: 10,
            pane_border: BorderMode::Always,
            tree_width: 34,
            preview_width: 60,
            notify: Notify::default(),
            sandbox: true,
            restore_screens: true,
            preview: true,
            preview_wrap: false,
            images: ImageMode::Auto,
            paths: PathStyle::Home,
            tmux: TmuxOffer::Ask,
            theme: ThemeMode::Auto,
            prefix: Prefix::CtrlB,
            remotes: serde_json::Value::Null,
            plugins: serde_json::Value::Null,
        }
    }
}

/// When the config file was last written; None when it is absent or unreadable.
pub fn mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}

/// Reads the config; if the file doesn't exist, writes defaults and returns them.
pub fn load_or_init() -> Config {
    load_checked().0
}

/// The config, and whether a file that is there failed to parse (the defaults stand in for it).
pub fn load_checked() -> (Config, bool) {
    let (cfg, broken) = match std::fs::read_to_string(config_path()) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(cfg) => (cfg, false),
            Err(_) => (Config::default(), true),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cfg = Config::default();
            let _ = save(&cfg);
            (cfg, false)
        }
        Err(_) => (Config::default(), true),
    };
    (cfg.clamped(), broken)
}

/// Once per config: the first server start with Control + B as the default switches a stored "off" to it,
/// and records that it ran whatever the value, so an "off" chosen later stays. A file that does not
/// parse is left untouched and nothing is recorded.
pub fn migrate_prefix() -> std::io::Result<bool> {
    let record = config_path().with_file_name("prefix-default-done");
    if record.exists() {
        return Ok(false);
    }
    let switched = match std::fs::read_to_string(config_path()) {
        Ok(data) => {
            let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&data) else { return Ok(false) };
            if serde_json::from_value::<Config>(v.clone()).is_err() {
                return Ok(false);
            }
            let off = v.get("prefix").and_then(serde_json::Value::as_str) == Some("off");
            if let Some(obj) = v.as_object_mut().filter(|_| off) {
                obj.insert("prefix".into(), "ctrl_b".into());
                let bytes = serde_json::to_vec_pretty(&v).map_err(std::io::Error::other)?;
                crate::paths::write_private(&config_path(), "json.tmp", &bytes)?;
            }
            off
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    std::fs::create_dir_all(record.parent().expect("config_path always has a parent"))?;
    std::fs::write(&record, "")?;
    Ok(switched)
}

/// (min, max) of each limited value: the file, Settings and the mouse drag all clamp to these.
pub const SIDEBAR_WIDTH: (u16, u16) = (20, 48);
pub const TREE_WIDTH: (u16, u16) = (20, 80);
pub const PREVIEW_WIDTH: (u16, u16) = (30, 120);
pub const PANE_GAP: (u16, u16) = (0, 3);
pub const SCROLLBACK: (usize, usize) = (100, 100_000);

impl Config {
    /// Clamps values from the file: a sidebar width of 0 crashes rendering on overflow.
    pub fn clamped(mut self) -> Self {
        self.sidebar_width = self.sidebar_width.clamp(SIDEBAR_WIDTH.0, SIDEBAR_WIDTH.1);
        self.tree_width = self.tree_width.clamp(TREE_WIDTH.0, TREE_WIDTH.1);
        self.preview_width = self.preview_width.clamp(PREVIEW_WIDTH.0, PREVIEW_WIDTH.1);
        self.pane_gap = self.pane_gap.min(PANE_GAP.1);
        self.scrollback = self.scrollback.clamp(SCROLLBACK.0, SCROLLBACK.1);
        self
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    let dir = path.parent().expect("config_path always has a parent");
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

    // Temp file + rename (atomic within one filesystem) so a concurrent load never sees a half-written file.
    let data = serde_json::to_vec_pretty(cfg).map_err(std::io::Error::other)?;
    crate::paths::write_private(&path, "json.tmp", &data)
}

pub mod autostart {
    use super::home_dir;
    use std::path::{Path, PathBuf};

    pub fn plist_path() -> PathBuf {
        home_dir().join("Library/LaunchAgents/dev.nerd.plist")
    }

    fn escape_xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    /// PATH is a literal: launchd doesn't inherit the shell environment, so without
    /// it neither claude nor brew is found.
    pub fn plist_text(exe: &Path) -> String {
        let exe = escape_xml(&exe.to_string_lossy());
        let home = home_dir();
        let log = escape_xml(&home.join("Library/Logs/nerd.log").to_string_lossy());
        let work_dir = escape_xml(&home.to_string_lossy());
        let path = escape_xml(&format!(
            "{h}/.local/bin:{h}/.claude/local:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            h = home.to_string_lossy()
        ));
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.nerd</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
    </dict>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
        <key>NERD_INSTANCE</key>
        <string>1</string>
    </dict>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>WorkingDirectory</key>
    <string>{work_dir}</string>
</dict>
</plist>
"#
        )
    }

    /// Doesn't touch any pre-existing LaunchAgents.
    pub fn install(exe: &Path) -> std::io::Result<()> {
        let path = plist_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, plist_text(exe))
    }

    pub fn uninstall() -> std::io::Result<()> {
        match std::fs::remove_file(plist_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn is_installed() -> bool {
        plist_path().exists()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn the_configuration_table_lists_every_field_and_no_other() {
        let listed: std::collections::BTreeSet<String> = include_str!("../docs/configuration.md")
            .lines().filter_map(|l| l.strip_prefix("| `")).filter_map(|l| l.split('`').next()).map(str::to_string).collect();
        let serde_json::Value::Object(fields) = serde_json::to_value(super::Config::default()).unwrap() else { panic!("an object") };
        // null by default, so left out of the serialized default
        let code: std::collections::BTreeSet<String> = fields.keys().cloned().chain(["remotes".into(), "plugins".into()]).collect();
        assert_eq!(listed, code, "docs/configuration.md's table and Config");
    }

    use super::*;
    use std::path::Path;

    // HOME is swapped process-wide for the test, so these serialize on this lock.
    pub(crate) fn with_temp_home<R>(f: impl FnOnce() -> R) -> R {
        let dir = crate::paths::test_dir("config-test");
        let old = crate::paths::TEST_HOME.with(|h| h.replace(Some(dir.clone())));

        let result = f();

        crate::paths::TEST_HOME.with(|h| h.replace(old));
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn a_stored_off_becomes_control_b_once() {
        with_temp_home(|| {
            let path = config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, r#"{"sidebar_width": 30, "prefix": "off", "zz": 1}"#).unwrap();
            assert!(migrate_prefix().unwrap());
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(load_checked().0.prefix, Prefix::CtrlB);
            assert!(text.find("sidebar_width") < text.find("zz"), "order and unknown keys kept: {text}");
            std::fs::write(&path, r#"{"prefix": "off"}"#).unwrap();
            assert!(!migrate_prefix().unwrap(), "recorded: an off chosen later stays");
            assert_eq!(load_checked().0.prefix, Prefix::Off);
        });
    }

    #[test]
    fn the_migration_record_is_written_whatever_the_value() {
        with_temp_home(|| {
            let path = config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, r#"{"prefix": "ctrl_a"}"#).unwrap();
            assert!(!migrate_prefix().unwrap());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"prefix": "ctrl_a"}"#, "untouched");
            std::fs::write(&path, r#"{"prefix": "off"}"#).unwrap();
            assert!(!migrate_prefix().unwrap(), "ctrl-a without a record still wrote it");
        });
        with_temp_home(|| {
            assert!(!migrate_prefix().unwrap(), "no file: the defaults already carry Control + B");
            std::fs::write(config_path(), r#"{"prefix": "off"}"#).unwrap();
            assert!(!migrate_prefix().unwrap());
        });
        with_temp_home(|| {
            let path = config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            for broken in [r#"{"prefix": "off""#, r#"{"prefix": "off", "sidebar_width": "wide"}"#] {
                std::fs::write(&path, broken).unwrap();
                assert!(!migrate_prefix().unwrap());
                assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "a file that does not parse is left alone");
                assert!(!path.with_file_name("prefix-default-done").exists(), "and nothing is recorded");
            }
        });
    }

    #[test]
    fn a_config_that_cannot_be_read_as_text_is_left_alone() {
        with_temp_home(|| {
            let path = config_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = b"{\"sidebar_width\": 33, \"x\": \"\xff\"}";
            std::fs::write(&path, bytes).unwrap();
            assert!(load_checked().1, "reported as broken");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        });
    }

    #[test]
    fn a_save_goes_through_config_json_tmp_and_ends_0600() {
        use std::os::unix::fs::PermissionsExt;
        with_temp_home(|| {
            let tmp = config_path().with_extension("json.tmp");
            std::fs::create_dir_all(&tmp).unwrap();
            assert!(save(&Config::default()).is_err(), "a directory in the temp file's place blocks the save");
            std::fs::remove_dir(&tmp).unwrap();
            save(&Config::default()).unwrap();
            let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!((mode(&config_path()), mode(config_path().parent().unwrap())), (0o600, 0o700));
            assert!(!tmp.exists());
        });
    }

    #[test]
    fn one_read_says_whether_the_file_parsed() {
        with_temp_home(|| {
            assert!(!load_checked().1, "no file: the defaults are written, nothing is broken");
            std::fs::write(config_path(), "{ not json").unwrap();
            let (cfg, broken) = load_checked();
            assert!(broken);
            assert_eq!(cfg.sidebar_width, Config::default().sidebar_width, "the defaults stand in");
            std::fs::write(config_path(), r#"{"sidebar_width": 30}"#).unwrap();
            assert_eq!((load_checked().0.sidebar_width, load_checked().1), (30, false));
        });
    }

    #[test]
    fn out_of_range_numbers_are_clamped() {
        let cfg = Config { sidebar_width: 0, tree_width: 500, pane_gap: 99, scrollback: 1, ..Config::default() }.clamped();
        assert_eq!((cfg.sidebar_width, cfg.tree_width, cfg.pane_gap, cfg.scrollback), (20, 80, 3, 100));
        assert_eq!(Config { scrollback: 1_000_000, ..Config::default() }.clamped().scrollback, 100_000);
    }

    #[test]
    fn restore_screens_defaults_on_and_round_trips() {
        let cfg = Config::default();
        assert!(cfg.restore_screens);
        let off: Config = serde_json::from_str(r#"{"restore_screens": false}"#).unwrap();
        assert!(!off.restore_screens);
        assert!(serde_json::to_string(&cfg).unwrap().contains("\"restore_screens\":true"));
    }

    #[test]
    fn a_config_from_the_days_of_system_notifications_still_loads() {
        let cfg: Config = serde_json::from_str(r#"{"notify":{"system":true,"sound":true,"bell":false}}"#).unwrap();
        assert!(cfg.notify.sound);
        assert!(!serde_json::to_string(&cfg).unwrap().contains("system"));
    }

    #[test]
    fn defaults_open_nothing_by_itself() {
        let cfg = Config::default();
        assert!(!serde_json::to_string(&cfg).unwrap().contains("projects"));
        assert_eq!(cfg.sidebar_width, 34, "wide enough for a subagent's task");
        assert!(cfg.colors.is_empty(), "the built-in palette is the default; the map only overrides");
        assert!(cfg.mouse);
    }

        #[test]
    fn a_clock_of_0_8_0_still_reads() {
        let read = |v: &str| serde_json::from_str::<Config>(&format!("{{\"clock\": {v}}}")).map(|c| c.clock).ok();
        assert_eq!((read("true"), read("false")), (Some(Clock::Time), Some(Clock::Off)), "as 0.8.0 wrote it");
        assert_eq!((read("\"time\""), read("\"date\""), read("\"off\"")), (Some(Clock::Time), Some(Clock::Date), Some(Clock::Off)));
        assert_eq!(read("\"week\""), None, "anything else does not parse");
        assert!(serde_json::to_string(&Config { clock: Clock::Time, ..Config::default() }).unwrap().contains("\"clock\":\"time\""), "written as the word");
    }

    #[test]
    fn save_and_load_round_trip() {
        with_temp_home(|| {
            let cfg = Config { plugins: serde_json::json!({ "hello": { "any": ["thing", 1] } }), ..Config::default() };
            save(&cfg).unwrap();
            let loaded = load_or_init();
            assert_eq!(loaded.plugins, cfg.plugins, "plugin settings kept as written");
            assert_eq!(loaded.colors, cfg.colors);
            assert_eq!(loaded.sidebar_width, cfg.sidebar_width);
            std::fs::write(config_path(), r#"{"allow_from_panes": true, "pane_gap": 2}"#).unwrap();
            assert_eq!(load_or_init().pane_gap, 2, "a file with the switch that moved to policy.json still loads");
        });
    }

    #[test]
    fn config_missing_fields_still_loads_and_fills_defaults() {
        with_temp_home(|| {
            let dir = config_path().parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(config_path(), "{\"colors\": {\"accent\": \"#ffffff\"}}").unwrap();

            let cfg = load_or_init();
            assert_eq!(cfg.colors.get("accent").map(String::as_str), Some("#ffffff"));
            assert_eq!(cfg.sidebar_width, Config::default().sidebar_width);
            assert_eq!(cfg.pane_gap, Config::default().pane_gap);
        });
    }

    #[test]
    fn an_old_config_with_sidebar_rows_still_loads() {
        with_temp_home(|| {
            let dir = config_path().parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            // the rows of the old sidebar no longer apply; their key must not break the rest
            std::fs::write(config_path(), r#"{"sidebar": {"spaces": [["workspace"]]}, "pane_gap": 2}"#).unwrap();

            let cfg = load_or_init();
            assert_eq!(cfg.pane_gap, 2);
        });
    }

    #[test]
    fn a_bad_remote_is_refused_at_use_and_kept_as_written() {
        let e = |name: &str, host: &str| RemoteEntry { name: name.into(), host: host.into() };
        assert!(e("studio", "me@studio.local").check().is_ok());
        assert!(e("a.b_c-1", "10.0.0.2").check().is_ok());
        for bad in [
            e("", "h"),
            e("x y", "h"),
            e(&"n".repeat(17), "h"),
            e("n\x1b", "h"),
            e("n", ""),
            e("n", "-oProxyCommand=x"),
            e("n", "a b"),
            e("n", "a\tb"),
            e("n", "a\x1b]2;x"),
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }

        with_temp_home(|| {
            std::fs::create_dir_all(config_path().parent().unwrap()).unwrap();
            let text = r#"{"pane_gap": 2, "remotes": [{"name": "studio"}, {"name": "x", "host": 5, "user": "me"}, {"name": "y", "host": "-oProxyCommand=x"}]}"#;
            std::fs::write(config_path(), text).unwrap();
            let cfg = load_or_init();
            assert_eq!(cfg.pane_gap, 2, "a missing host resets nothing else");
            let entries = cfg.remote_entries();
            assert_eq!(entries.len(), 3);
            assert!(entries.iter().all(|e| e.check().is_err()), "{entries:?}");
            save(&cfg).unwrap();
            let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config_path()).unwrap()).unwrap();
            let was: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(back["remotes"], was["remotes"], "a wrong type and an unknown key are saved as they were");

            std::fs::write(config_path(), r#"{"pane_gap": 3, "remotes": "studio"}"#).unwrap();
            let cfg = load_or_init();
            assert_eq!((cfg.pane_gap, cfg.remote_entries().len()), (3, 0), "a wrong shape lists nothing and resets nothing");
            save(&cfg).unwrap();
            assert!(std::fs::read_to_string(config_path()).unwrap().contains(r#""remotes": "studio""#), "and it is kept");
        });
    }

    #[test]
    fn plist_text_has_required_keys() {
        let text = autostart::plist_text(Path::new("/opt/homebrew/bin/nerd"));
        assert!(text.contains("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"));
        assert!(text.contains("RunAtLoad"));
        assert!(text.contains("Crashed"));
        assert!(text.contains("dev.nerd"));
    }

    #[test]
    fn plist_text_is_well_formed_xml() {
        let text = autostart::plist_text(Path::new("/opt/homebrew/bin/nerd"));
        assert!(tags_balanced(&text));
    }

    #[test]
    fn plist_text_escapes_special_chars_in_paths() {
        let text = autostart::plist_text(Path::new("/Users/a & b/<nerd>"));
        assert!(!text.contains("a & b"));
        assert!(!text.contains("<nerd>"));
        assert!(text.contains("a &amp; b"));
        assert!(text.contains("&lt;nerd&gt;"));
        assert!(tags_balanced(&text));
    }

    /// Rough check that every opened tag is closed; a full XML crate would be overkill here.
    fn tags_balanced(xml: &str) -> bool {
        let mut stack: Vec<&str> = Vec::new();
        let mut rest = xml;
        while let Some(open) = rest.find('<') {
            let Some(close) = rest[open..].find('>') else {
                return false;
            };
            let tag = &rest[open + 1..open + close];
            rest = &rest[open + close + 1..];

            if tag.starts_with('?') || tag.starts_with('!') || tag.ends_with('/') {
                continue;
            }
            // attributes (like in <plist version="1.0">) aren't part of the tag name
            let name = tag.split_whitespace().next().unwrap_or(tag);
            if let Some(name) = name.strip_prefix('/') {
                if stack.pop() != Some(name) {
                    return false;
                }
            } else {
                stack.push(name);
            }
        }
        stack.is_empty()
    }
}
