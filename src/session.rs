//! Workspace persistence and reconnecting to Claude Code sessions.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::layout::{PaneId, Snapshot};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PaneSnapshot {
    pub id: PaneId,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub agent_session: Option<String>,
    /// Whose session that is; snapshots from before agents had manifests held only Claude's.
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Pane name, set by the human (distinct from `title`).
    #[serde(default)]
    pub label: Option<String>,
    pub title: Option<String>,
    /// Defaults to 80x24 for old snapshots; never 0, a terminal needs a row.
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default, skip_serializing_if = "crate::policy::Isolation::is_open")]
    pub isolation: crate::policy::Isolation,
}

pub fn default_agent() -> String {
    "claude".into()
}

impl PaneSnapshot {
    pub fn session(&self) -> Option<(String, String)> {
        self.agent_session.clone().map(|id| (self.agent.clone(), id))
    }
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabSnapshot {
    pub name: Option<String>,
    pub layout: Snapshot,
    pub panes: Vec<PaneSnapshot>,
    pub focused: PaneId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_new: Option<crate::policy::Isolation>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpaceSnapshot {
    /// Name set by the human; empty means it's taken from the folder.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub tabs: Vec<TabSnapshot>,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_new: Option<crate::policy::Isolation>,
}

/// One pane crossing exec: its master fd stays open, the screen replays from a dump.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PaneHandover {
    pub id: PaneId,
    pub fd: i32,
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub scroll: usize,
    /// Nothing reads it any more; kept on the wire so an older build can still parse a handover.
    pub alive: bool,
    /// Milliseconds since the pane last changed state; None when it has not yet.
    #[serde(default)]
    pub changed_ms_ago: Option<u64>,
    /// Finished work nobody has looked at: its ✓ must survive the update.
    #[serde(default)]
    pub unseen: bool,
    /// Started through `nerd --pane-exec`: its shell is below the wrapper.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrapped: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Handover {
    pub snapshot: WorkspaceSnapshot,
    pub panes: Vec<PaneHandover>,
    pub broker_fd: i32,
    /// 0 from an older build: the daemon's own pid stands in.
    #[serde(default)]
    pub broker_pid: i32,
    /// The adopted broker's protocol, handed on unchanged: 0 from an older build, whose broker cannot launch plugins.
    #[serde(default)]
    pub broker_protocol: u32,
    pub listener_fd: i32,
    /// (client id, fd, cols, rows, mux) of the attached client, if any.
    pub client: Option<(u64, i32, u16, u16, bool)>,
    /// Outside the tuple, flat under the v1 names: a new field there would fail to read a handover from the old build.
    #[serde(flatten)]
    pub caps: crate::proto::ClientCaps,
    #[serde(default)]
    pub clients: Vec<crate::clients::ClientRecord>,
    /// The adopted broker may still answer for ids handed out before, so they are never reused; 0 from an older build.
    #[serde(default)]
    pub next_id: PaneId,
    /// The build that wrote it, as `nerd --version` prints it; empty from an older build.
    #[serde(default)]
    pub from: String,
}

pub fn handover_path() -> PathBuf {
    state_dir().join("handover.json")
}

pub fn write_handover(h: &Handover) -> std::io::Result<()> {
    crate::paths::write_private(&handover_path(), "tmp", &serde_json::to_vec(h)?)
}

/// Screen dumps live next to the handover file, one per pane.
pub fn handover_dump_path(id: PaneId) -> PathBuf {
    state_dir().join("handover.d").join(format!("{id}.term"))
}

pub fn clear_handover() {
    let _ = std::fs::remove_file(handover_path());
    let dumps = state_dir().join("handover.d");
    if let Ok(rd) = std::fs::read_dir(&dumps) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(dumps);
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub spaces: Vec<SpaceSnapshot>,
    #[serde(default)]
    pub active_space: usize,
}

/// Each instance gets its own subfolder, so a plain `nerd` in a second window is a second desktop.
pub fn state_dir() -> PathBuf {
    if std::env::var_os("NERD_STATE_DIR").is_some() {
        return crate::paths::state_root();
    }
    match std::env::var("NERD_INSTANCE") {
        Ok(n) if !n.is_empty() => instances_dir().join(n),
        _ => crate::paths::state_root(),
    }
}

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("NERD_SOCKET") {
        return PathBuf::from(p);
    }
    state_dir().join("nerd.sock")
}

/// Plugins connect here and only here: their sandbox cannot reach `socket_path`.
pub fn plugin_socket_path() -> PathBuf {
    socket_path().with_extension("psock")
}

/// Agents in panes send and read messages here.
pub fn msock_path() -> PathBuf {
    socket_path().with_extension("msock")
}

/// Where the broker for plugins connects back once, after an update from an older broker.
pub fn exec_socket_path() -> PathBuf {
    socket_path().with_extension("xsock")
}

pub fn instances_dir() -> PathBuf {
    crate::paths::state_root().join("instances")
}

/// The instance this process belongs to, as `--resume` spells it; empty for the legacy one.
pub fn instance_name() -> String {
    #[cfg(test)]
    if let Some(n) = tests::INSTANCE.with_borrow(Clone::clone) {
        return n;
    }
    std::env::var("NERD_INSTANCE").unwrap_or_default()
}

/// A human name for an instance, kept beside it: the directory is its id and a running
/// server holds that path for life, so it is never renamed.
fn name_path(id: &str) -> PathBuf {
    instances_dir().join(id).join("name")
}

/// What an instance is called: its name if it has one, else its id; "legacy" for the unnamed one.
pub fn instance_label(id: &str) -> String {
    if id.is_empty() {
        return "legacy".to_string();
    }
    std::fs::read_to_string(name_path(id))
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_string())
}

/// The id a user typed, by id or by name. Empty is the legacy instance and passes through.
pub fn resolve(query: &str) -> Option<String> {
    if query.is_empty() {
        return Some(String::new());
    }
    let ids = instances();
    if ids.iter().any(|id| id == query) {
        return Some(query.to_string());
    }
    // `instances()` is sorted, so a name two ever share goes to the lowest id.
    let named = ids.into_iter().find(|id| instance_label(id) == query && id != query);
    // A server told to live elsewhere (a test server on its own socket, or a state dir with no
    // instances/ in it) has no directory here to find, so what was typed goes through as before.
    let elsewhere = std::env::var_os("NERD_SOCKET").is_some()
        || (std::env::var_os("NERD_STATE_DIR").is_some() && !instances_dir().is_dir());
    named.or_else(|| elsewhere.then(|| query.to_string()))
}

/// Names never look like an id: not all digits, and no path characters.
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("a name is 1 to 32 characters".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
        return Err("a name uses letters, digits, '_' and '-' only".into());
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("a name needs a letter: all digits would read as a server number".into());
    }
    if name.starts_with('-') {
        return Err("a name cannot start with '-': it would read as an option".into());
    }
    if name == "legacy" {
        return Err("legacy is how the unnamed old server is shown".into());
    }
    Ok(())
}

/// An instance id as it may cross ssh: numbers and older word ids, never a path or an option.
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && !id.starts_with('-')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// A label another Mac reported, before a title or a dialog shows it.
pub fn valid_label(label: &str) -> bool {
    (1..=32).contains(&label.len()) && label.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// Names instance `id`, or clears its name when `name` is empty.
pub fn set_name(id: &str, name: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("the legacy server cannot be named; start a new one with nerd".into());
    }
    if !instances_dir().join(id).is_dir() {
        return Err(format!("no server {id}"));
    }
    if name.is_empty() {
        return match std::fs::remove_file(name_path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    valid_name(name)?;
    // Ids made by an older `--resume <word>` are words too: a name must not hide one.
    if instances_dir().join(name).is_dir() && name != id {
        return Err(format!("{name} is already a server"));
    }
    if let Some(other) = instances().into_iter().find(|o| o != id && instance_label(o) == name) {
        return Err(format!("server {other} is already called {name}"));
    }
    crate::paths::write_private(&name_path(id), "tmp", format!("{name}\n").as_bytes()).map_err(|e| e.to_string())
}

/// Known instances, numeric names first in numeric order.
pub fn instances() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(instances_dir())
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| (n.parse::<u32>().unwrap_or(u32::MAX), n.clone()));
    names
}

/// Name for a fresh instance: one past the largest number in use.
pub fn new_instance_name() -> String {
    (instances().iter().filter_map(|n| n.parse::<u32>().ok()).max().unwrap_or(0) + 1).to_string()
}

/// The instance touched last, by snapshot time: what `--resume` without a name means.
pub fn last_instance() -> Option<String> {
    instances()
        .into_iter()
        .max_by_key(|n| {
            std::fs::metadata(instances_dir().join(n).join("session.json"))
                .and_then(|m| m.modified())
                .ok()
        })
}

/// One-time migration: pre-instance layout in the state root becomes instance 1.
pub fn migrate_legacy() {
    if std::env::var_os("NERD_STATE_DIR").is_some() || instances_dir().exists() {
        return;
    }
    let base = crate::paths::state_root();
    if !base.join("session.json").exists() {
        return;
    }
    let one = instances_dir().join("1");
    if std::fs::create_dir_all(&one).is_err() {
        return;
    }
    for f in ["session.json", "session.json.bak", "agents"] {
        let _ = std::fs::rename(base.join(f), one.join(f));
    }
}

pub fn state_path() -> PathBuf {
    state_dir().join("session.json")
}

fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Saves atomically via temp file + rename, so a crash mid-write never leaves a broken JSON file.
pub fn save(snap: &WorkspaceSnapshot) -> std::io::Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    restrict(&dir, 0o700)?;

    let json = serde_json::to_vec_pretty(snap).map_err(std::io::Error::other)?;
    let _ = std::fs::remove_file(dir.join("session.json.tmp"));
    let path = state_path();
    if path.exists() {
        let _ = std::fs::rename(&path, backup_path());
    }
    crate::paths::write_private(&path, "tmp", &json)
}

/// Pane text dumps, one file per pane; `.noindex` keeps Spotlight out.
pub fn screens_dir() -> PathBuf {
    state_dir().join("screens.noindex")
}

pub fn screen_path(id: PaneId) -> PathBuf {
    screens_dir().join(format!("{id}.txt"))
}

/// Excludes the dir from Time Machine once (sticky); not awaited, `tmutil` takes ~11s here.
pub fn ensure_screens_dir() -> std::io::Result<PathBuf> {
    let dir = screens_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        restrict(&dir, 0o700)?;
        exclude_from_time_machine(&dir);
    }
    Ok(dir)
}

/// Sticky on a path, lost when a rename replaces it; not awaited, `tmutil` takes ~11s here.
pub fn exclude_from_time_machine(p: &Path) {
    let _ = crate::spawn::detached(
        crate::spawn::cmd("/usr/bin/tmutil")
            .arg("addexclusion")
            .arg(p)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
}

/// Called on every restore so a screen dump never outlives its outage.
pub fn clear_screens() -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(screens_dir()) {
        for e in rd.flatten() {
            if std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

fn backup_path() -> PathBuf {
    state_dir().join("session.json.bak")
}

fn read_snapshot(path: &Path) -> Option<WorkspaceSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Loads the snapshot, falling back to .bak if missing or unparsable.
pub fn load() -> Option<WorkspaceSnapshot> {
    read_snapshot(&state_path()).or_else(|| read_snapshot(&backup_path()))
}

/// Folders of every space in every instance's saved session, plus the state root's own (the
/// unnamed legacy instance keeps its session there directly). For "New space"'s recent list,
/// deduplicated in the order found; existing folders only.
pub fn recent_spaces() -> Vec<PathBuf> {
    let mut dirs = vec![crate::paths::state_root()];
    dirs.extend(instances().iter().map(|id| instances_dir().join(id)));
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let snap = read_snapshot(&dir.join("session.json")).or_else(|| read_snapshot(&dir.join("session.json.bak")));
        for sp in snap.into_iter().flat_map(|s| s.spaces) {
            if sp.cwd.is_dir() && seen.insert(sp.cwd.clone()) {
                out.push(sp.cwd);
            }
        }
    }
    out
}

// Debounce: layout changes on every resize/drag, disk doesn't need every one.
const SAVE_INTERVAL: Duration = Duration::from_millis(250);

enum SaveMsg {
    Snapshot(WorkspaceSnapshot),
    /// Write what is owed now, then answer.
    Flush(std::sync::mpsc::Sender<()>),
}

/// Hands snapshots to the one thread that writes `session.json`: two writers would share its temp file.
pub struct Saver {
    tx: std::sync::mpsc::Sender<SaveMsg>,
    asked: Option<Instant>,
}

impl Saver {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        // A saver that did not start makes every flush answer false.
        let _ = std::thread::Builder::new().name("saver".into()).spawn(move || save_loop(rx));
        Self { tx, asked: None }
    }

    /// Whether a snapshot is worth building: at most one per interval.
    pub fn due(&self) -> bool {
        self.asked.is_none_or(|t| t.elapsed() >= SAVE_INTERVAL)
    }

    /// Queues state; the saver thread writes it unless it is what the file already holds.
    pub fn request(&mut self, snap: WorkspaceSnapshot) {
        self.asked = Some(Instant::now());
        let _ = self.tx.send(SaveMsg::Snapshot(snap));
    }

    /// Waits (up to 5 s, else false) until the request is written or failed; call before exec or exit.
    pub fn flush(&mut self) -> bool {
        let (ack, done) = std::sync::mpsc::channel();
        self.tx.send(SaveMsg::Flush(ack)).is_ok() && done.recv_timeout(Duration::from_secs(5)).is_ok()
    }
}

/// The saver thread: a snapshot the file already holds is not written again, and a failed write is tried again every interval.
fn save_loop(rx: std::sync::mpsc::Receiver<SaveMsg>) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut last: Option<WorkspaceSnapshot> = None;
    let mut pending: Option<WorkspaceSnapshot> = None;
    loop {
        let msg = match pending {
            Some(_) => rx.recv_timeout(SAVE_INTERVAL),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let ack = match msg {
            Ok(SaveMsg::Snapshot(snap)) => {
                pending = (last.as_ref() != Some(&snap)).then_some(snap);
                None
            }
            Ok(SaveMsg::Flush(ack)) => Some(ack),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if let Some(snap) = pending.take() {
            match save(&snap) {
                Ok(()) => last = Some(snap),
                Err(_) => pending = Some(snap),
            }
        }
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }
}

impl Default for Saver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_handover_from_the_previous_build_has_no_graphics_answer() {
        let snapshot = WorkspaceSnapshot { version: 2, spaces: Vec::new(), active_space: 0 };
        let h = Handover {
            snapshot,
            panes: Vec::new(),
            broker_fd: 3,
            broker_pid: 4321,
            broker_protocol: 1,
            listener_fd: 4,
            client: Some((1, 5, 80, 24, false)),
            caps: crate::proto::ClientCaps {
                mux: false,
                screen: false,
                graphics: Some(true),
                terminal: Some("/Applications/Ghostty.app".into()),
                opened_by_nerd: true,
                opens_windows: true,
                dark: Some(false),
                bye: true,
                connects: true,
                reattach: Some("nerd --remote studio 1".into()),
                machine: Some("studio".into()),
            },
            clients: Vec::new(),
            next_id: 1,
            from: "nerd 0.6.0 (abc1234, 2026-09-30 22:25)".into(),
        };
        let mut v = serde_json::to_value(&h).unwrap();
        for key in ["graphics", "terminal", "opened_by_nerd", "dark", "clients", "bye", "connects", "reattach", "machine"] {
            v.as_object_mut().unwrap().remove(key);
        }
        let old: Handover = serde_json::from_value(v).unwrap();
        assert_eq!((old.caps.graphics, old.caps.terminal, old.caps.opened_by_nerd, old.caps.dark), (None, None, false, None));
        assert_eq!((old.caps.bye, old.caps.connects, old.caps.reattach, old.caps.machine), (false, false, None, None), "an older build's client knows no markers");
        assert_eq!(old.client, Some((1, 5, 80, 24, false)), "the client tuple keeps its shape");
        assert!(old.clients.is_empty());
    }

    #[test]
    fn a_snapshot_without_a_size_restores_at_80x24() {
        let json = r#"{"id": 3, "cwd": "/tmp", "argv": ["zsh"], "agent_session": null, "title": null}"#;
        let ps: PaneSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!((ps.cols, ps.rows), (80, 24), "old snapshots must not yield a 0-row grid");
        assert_eq!(ps.agent, "claude", "a snapshot from before manifests held only Claude's sessions");
    }

    #[test]
    fn instances_answer_to_their_names() {
        with_temp_state_dir("names", || {
            for id in ["1", "2", "work"] {
                std::fs::create_dir_all(instances_dir().join(id)).unwrap();
            }
            assert_eq!(instance_label("1"), "1", "no name yet: the id");
            set_name("1", "main").unwrap();
            assert_eq!(instance_label("1"), "main");
            assert_eq!(resolve("main").as_deref(), Some("1"), "by name");
            assert_eq!(resolve("2").as_deref(), Some("2"), "by id");
            assert_eq!(resolve("nosuch"), None);
            assert_eq!(resolve("").as_deref(), Some(""), "empty is the legacy server");

            for bad in ["", "12", "a/b", "..", ".", &"x".repeat(33)] {
                if !bad.is_empty() {
                    assert!(set_name("2", bad).is_err(), "{bad:?} refused");
                }
            }
            assert!(set_name("2", "main").is_err(), "one name, one server");
            assert!(set_name("2", "work").is_err(), "a directory already answers to work");
            assert!(set_name("", "x").is_err(), "the legacy server has nowhere to keep a name");
            assert!(set_name("9", "x").is_err(), "no such server");

            set_name("1", "").unwrap();
            assert_eq!(instance_label("1"), "1", "a cleared name gives the id back");
            assert_eq!(resolve("main"), None);

            for bad in ["-h", "--from-window", "legacy"] {
                assert!(set_name("2", bad).is_err(), "{bad:?} refused");
            }

            // A word-id with a name of its own still owns its word: the directory check, not
            // the duplicate check, is what refuses it.
            set_name("work", "office").unwrap();
            assert!(set_name("2", "work").is_err(), "work is a directory even when it is called office");

            // Ids are looked up before names: a name file that says "2" does not steal instance 2.
            std::fs::write(instances_dir().join("1").join("name"), "2\n").unwrap();
            assert_eq!(resolve("2").as_deref(), Some("2"));
        });
    }

    #[test]
    fn ids_and_labels_from_another_mac_are_checked() {
        for ok in ["1", "42", "work", "a_b-c"] {
            assert!(valid_id(ok), "{ok:?}");
        }
        for bad in ["", "-h", "../x", "a/b", "a b", "\x1b[2J", &"9".repeat(33)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        for ok in ["legacy", "1", "my box.2"] {
            assert!(valid_label(ok), "{ok:?}");
        }
        for bad in ["", "a\x1bb", "a/b", &"x".repeat(33)] {
            assert!(!valid_label(bad), "{bad:?}");
        }
    }

    // Per thread, so a test names its server without touching the process environment.
    thread_local! { pub(crate) static INSTANCE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) }; }

    // NERD_STATE_DIR is process-wide, so tests serialize on this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn with_temp_state_dir<R>(test: &str, f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join(format!("nerd-session-test-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("NERD_STATE_DIR", &dir) };

        let result = f();

        unsafe { std::env::remove_var("NERD_STATE_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

        fn sample_snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: 2,
            spaces: vec![SpaceSnapshot {
                name: Some("проект".into()),
                cwd: PathBuf::from("/tmp"),
                active_tab: 0,
                isolation_new: None,
                tabs: vec![TabSnapshot {
                    name: Some("main".into()),
                    layout: Snapshot::Pane(0),
                    panes: vec![PaneSnapshot {
                        id: 0,
                        cwd: PathBuf::from("/tmp"),
                        argv: vec!["zsh".into()],
                        agent_session: Some("0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c".into()),
                        agent: default_agent(),
                        label: None,
                        title: None,
                        cols: 80,
                        rows: 24,
                        isolation: Default::default(),
                    }],
                    focused: 0,
                    isolation_new: None,
                }],
            }],
            active_space: 0,
        }
    }

    #[test]
    fn save_load_round_trip() {
        with_temp_state_dir("round-trip", || {
            let snap = sample_snapshot();
            save(&snap).unwrap();
            let loaded = load().expect("a snapshot just saved should load back");
            assert_eq!(loaded.version, snap.version);
            assert_eq!(loaded.spaces.len(), 1);
            assert_eq!(loaded.spaces[0].name.as_deref(), Some("проект"));
            assert_eq!(loaded.spaces[0].tabs.len(), 1);
            assert_eq!(
                loaded.spaces[0].tabs[0].panes[0].agent_session,
                Some("0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c".into())
            );
        });
    }

    #[test]
    fn recent_spaces_dedups_across_instances_and_drops_a_missing_folder() {
        with_temp_state_dir("recent", || {
            let dir = crate::paths::state_root();
            let (a, b) = (dir.join("a"), dir.join("b"));
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            let missing = dir.join("gone");

            let snap_of = |cwds: &[&Path]| WorkspaceSnapshot {
                version: 2,
                active_space: 0,
                spaces: cwds
                    .iter()
                    .map(|cwd| SpaceSnapshot { name: None, cwd: cwd.to_path_buf(), tabs: Vec::new(), active_tab: 0, isolation_new: None })
                    .collect(),
            };
            let write = |dir: &Path, snap: &WorkspaceSnapshot| {
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(dir.join("session.json"), serde_json::to_vec(snap).unwrap()).unwrap();
            };
            write(&instances_dir().join("1"), &snap_of(&[&a, &missing]));
            write(&instances_dir().join("2"), &snap_of(&[&b, &a]));

            let recent = recent_spaces();
            assert_eq!(recent, vec![a.clone(), b.clone()], "deduplicated, in the order found, missing folder dropped");
        });
    }

    #[test]
    fn a_snapshot_is_due_once_an_interval_and_an_unchanged_one_is_not_written() {
        use std::os::unix::fs::MetadataExt;
        with_temp_state_dir("due", || {
            let written = || std::fs::metadata(state_path()).map(|m| m.ino()).ok();
            let mut saver = Saver::new();
            assert!(saver.due());
            saver.request(sample_snapshot());
            assert!(!saver.due(), "the next one waits out the interval");
            saver.flush();
            let first = written();
            assert!(first.is_some(), "flush returns once the file is there");
            saver.request(sample_snapshot());
            saver.flush();
            assert_eq!(written(), first, "the same workspace is not written again");
            let mut other = sample_snapshot();
            other.active_space = 1;
            saver.request(other);
            saver.flush();
            assert_ne!(written(), first, "a changed one is");
            std::thread::sleep(SAVE_INTERVAL);
            assert!(saver.due());
        });
    }

    #[test]
    fn a_save_that_failed_is_tried_again() {
        with_temp_state_dir("retry", || {
            let mut saver = Saver::new();
            std::fs::create_dir_all(state_dir()).unwrap();
            let blocker = state_path().with_extension("tmp");
            std::fs::create_dir(&blocker).unwrap();
            saver.request(sample_snapshot());
            saver.flush();
            assert!(load().is_none(), "the write failed");
            std::fs::remove_dir(&blocker).unwrap();
            let t = Instant::now();
            // Nothing changed, and it is still owed.
            while load().is_none() {
                assert!(t.elapsed() < Duration::from_secs(2), "not tried again");
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    }

    #[test]
    fn a_save_rotates_the_last_one_into_bak_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        with_temp_state_dir("rotation", || {
            save(&sample_snapshot()).unwrap();
            // A temp file of the name older builds wrote is cleared by the next save.
            std::fs::write(state_dir().join("session.json.tmp"), "stale").unwrap();
            let mut second = sample_snapshot();
            second.spaces[0].name = Some("second".into());
            save(&second).unwrap();
            assert_eq!(read_snapshot(&state_path()).unwrap().spaces[0].name.as_deref(), Some("second"));
            assert_eq!(read_snapshot(&backup_path()).unwrap().spaces[0].name.as_deref(), Some("проект"));
            assert_eq!(backup_path(), state_dir().join("session.json.bak"));
            assert_eq!((mode(&state_path()), mode(&backup_path()), mode(&state_dir())), (0o600, 0o600, 0o700));
            assert!(!state_dir().join("session.json.tmp").exists());
            assert!(!state_dir().join("session.tmp").exists());
        });
    }

    #[test]
    fn a_broken_snapshot_falls_back_to_the_backup() {
        with_temp_state_dir("backup", || {
            let snap = sample_snapshot();
            save(&snap).unwrap();
            save(&snap).unwrap();
            std::fs::write(state_path(), "{ broken json").unwrap();
            let loaded = load().expect("should recover from .bak");
            assert_eq!(loaded.spaces[0].name.as_deref(), Some("проект"));
        });
    }

    /// The persisted shapes as `128de23` declared them: a newer build must write what they parse.
    #[allow(dead_code)]
    mod v128de23 {
        use serde::Deserialize;
        use std::path::PathBuf;

        #[derive(Deserialize)]
        pub enum Dir {
            Horizontal,
            Vertical,
        }

        #[derive(Deserialize)]
        pub enum Layout {
            Split { dir: Dir, ratio: f32, first: Box<Layout>, second: Box<Layout> },
            Pane(usize),
        }

        #[derive(Deserialize)]
        pub struct PaneSnapshot {
            pub id: usize,
            pub cwd: PathBuf,
            pub argv: Vec<String>,
            pub agent_session: Option<String>,
            #[serde(default)]
            pub label: Option<String>,
            pub title: Option<String>,
            pub cols: u16,
            pub rows: u16,
        }

        #[derive(Deserialize)]
        pub struct TabSnapshot {
            pub name: Option<String>,
            pub layout: Layout,
            pub panes: Vec<PaneSnapshot>,
            pub focused: usize,
        }

        #[derive(Deserialize)]
        pub struct SpaceSnapshot {
            pub name: Option<String>,
            pub cwd: PathBuf,
            pub tabs: Vec<TabSnapshot>,
            #[serde(default)]
            pub active_tab: usize,
        }

        #[derive(Deserialize)]
        pub struct WorkspaceSnapshot {
            pub version: u32,
            pub spaces: Vec<SpaceSnapshot>,
            #[serde(default)]
            pub active_space: usize,
        }

        #[derive(Deserialize, Debug, PartialEq)]
        pub enum End {
            Detached,
            Closed,
            TakenOver,
            Lost,
            Disconnected,
            #[serde(other)]
            Unknown,
        }

        #[derive(Deserialize)]
        pub struct ClientRecord {
            pub id: u64,
            pub since: u64,
            pub host: Option<String>,
            pub program: Option<String>,
            pub ended: Option<(u64, End)>,
        }

        #[derive(Deserialize)]
        pub struct PaneHandover {
            pub id: usize,
            pub fd: i32,
            pub pid: Option<u32>,
            pub cols: u16,
            pub rows: u16,
            pub scroll: usize,
            pub alive: bool,
            #[serde(default)]
            pub changed_ms_ago: Option<u64>,
            #[serde(default)]
            pub unseen: bool,
        }

        #[derive(Deserialize)]
        pub struct Handover {
            pub snapshot: WorkspaceSnapshot,
            pub panes: Vec<PaneHandover>,
            pub broker_fd: i32,
            pub listener_fd: i32,
            pub client: Option<(u64, i32, u16, u16, bool)>,
            #[serde(default)]
            pub graphics: Option<bool>,
            #[serde(default)]
            pub terminal: Option<PathBuf>,
            #[serde(default)]
            pub opened_by_nerd: bool,
            #[serde(default)]
            pub dark: Option<bool>,
            #[serde(default)]
            pub clients: Vec<ClientRecord>,
            #[serde(default)]
            pub bye: bool,
            #[serde(default)]
            pub connects: bool,
            #[serde(default)]
            pub reattach: Option<String>,
            #[serde(default)]
            pub machine: Option<String>,
        }
    }

    fn golden_workspace() -> WorkspaceSnapshot {
        use crate::layout::{Dir, Snapshot};
        let pane = |id, argv: &[&str], agent: Option<&str>| PaneSnapshot {
            id,
            cwd: "/Users/me/proj".into(),
            argv: argv.iter().map(|a| a.to_string()).collect(),
            agent_session: agent.map(str::to_string),
            agent: default_agent(),
            label: (id == 2).then(|| "agent".to_string()),
            title: Some(format!("title {id}")),
            cols: 100,
            rows: 30,
            isolation: Default::default(),
        };
        let layout = Snapshot::Split { dir: Dir::Horizontal, ratio: 0.5, first: Box::new(Snapshot::Pane(1)), second: Box::new(Snapshot::Pane(2)) };
        let tab = TabSnapshot { name: Some("main".into()), layout, panes: vec![pane(1, &["/bin/zsh"], None), pane(2, &["claude"], Some("0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c"))], focused: 2, isolation_new: None };
        let space = SpaceSnapshot { name: Some("proj".into()), cwd: "/Users/me/proj".into(), tabs: vec![tab], active_tab: 0, isolation_new: None };
        WorkspaceSnapshot { version: 2, spaces: vec![space], active_space: 0 }
    }

    #[test]
    fn the_handover_file_is_private_whatever_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        with_temp_state_dir("handover-mode", || {
            let old = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o022));
            let r = write_handover(&golden_handover());
            nix::sys::stat::umask(old);
            r.unwrap();
            assert_eq!(std::fs::metadata(handover_path()).unwrap().permissions().mode() & 0o777, 0o600);
        });
    }

    fn golden_handover() -> Handover {
        let ph = |id, fd, unseen| PaneHandover { id, fd, pid: Some(4000 + id as u32), cols: 100, rows: 30, scroll: 0, alive: true, changed_ms_ago: Some(1500), unseen, wrapped: false };
        let record = crate::clients::ClientRecord { id: 1, since: 1_700_000_000, host: Some("MacBook-Air".into()), program: Some("Ghostty".into()), ended: Some((1_700_000_100, crate::clients::End::TakenOver)) };
        Handover {
            snapshot: golden_workspace(),
            panes: vec![ph(1, 7, false), ph(2, 8, true)],
            broker_fd: 3,
            broker_pid: 4321,
            broker_protocol: 1,
            listener_fd: 4,
            client: Some((2, 5, 100, 30, false)),
            caps: crate::proto::ClientCaps {
                mux: false,
                screen: false,
                graphics: Some(true),
                terminal: Some("/Applications/Ghostty.app".into()),
                opened_by_nerd: true,
                opens_windows: true,
                dark: Some(true),
                bye: true,
                connects: true,
                reattach: Some("nerd --remote studio 1".into()),
                machine: Some("studio".into()),
            },
            clients: vec![record],
            next_id: 3,
            from: String::new(),
        }
    }

    #[test]
    fn a_wrapped_pane_stays_wrapped_across_an_update() {
        let mut h = golden_handover();
        h.panes[1].wrapped = true;
        let back: Handover = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back.panes.iter().map(|p| p.wrapped).collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn files_written_by_128de23_still_read() {
        let h: Handover = serde_json::from_str(include_str!("../tests/fixtures/handover-128de23.json")).unwrap();
        assert_eq!((h.caps.bye, h.caps.connects, h.caps.machine.as_deref(), h.caps.graphics), (true, true, Some("studio"), Some(true)));
        assert_eq!((h.caps.opened_by_nerd, h.caps.dark, h.caps.reattach.as_deref()), (true, Some(true), Some("nerd --remote studio 1")));
        assert_eq!((h.broker_fd, h.listener_fd, h.client), (3, 4, Some((2, 5, 100, 30, false))));
        assert_eq!(h.broker_pid, 0, "128de23 did not record the broker: the daemon's own pid stands in");
        assert_eq!(h.broker_protocol, 0, "its broker cannot launch plugins: they stay off until a restart");
        assert_eq!(h.caps.terminal.as_deref(), Some(Path::new("/Applications/Ghostty.app")));
        assert_eq!(h.panes.iter().map(|p| (p.id, p.fd, p.pid, p.alive, p.unseen)).collect::<Vec<_>>(), [(1, 7, Some(4001), true, false), (2, 8, Some(4002), true, true)]);
        assert_eq!(h.panes[0].changed_ms_ago, Some(1500));
        assert_eq!(h.clients[0].ended, Some((1_700_000_100, crate::clients::End::TakenOver)));
        assert_eq!(h.snapshot.version, 2);
        assert!(!h.caps.opens_windows, "128de23 did not carry it: the client is asked again");

        let w: WorkspaceSnapshot = serde_json::from_str(include_str!("../tests/fixtures/session-128de23.json")).unwrap();
        assert_eq!(w.version, 2);
        let tab = &w.spaces[0].tabs[0];
        assert_eq!((w.spaces[0].name.as_deref(), tab.name.as_deref(), tab.focused), (Some("proj"), Some("main"), 2));
        assert_eq!(tab.panes.iter().map(|p| p.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!((tab.panes[1].agent_session.as_deref(), tab.panes[1].label.as_deref(), tab.panes[1].cols), (Some("abc-123"), Some("agent"), 100));
        assert_eq!(tab.panes[1].session(), Some(("claude".into(), "abc-123".into())), "an old snapshot's session is Claude's");
        assert!(matches!(&tab.layout, crate::layout::Snapshot::Split { ratio, first, .. } if *ratio == 0.5 && matches!(**first, crate::layout::Snapshot::Pane(1))));
    }

    #[test]
    fn opening_windows_survives_a_handover() {
        let h: Handover = serde_json::from_slice(&serde_json::to_vec(&golden_handover()).unwrap()).unwrap();
        assert!(h.caps.opens_windows);
        assert_eq!(h.broker_pid, 4321, "the broker pid survives a handover");
    }

    #[test]
    fn the_128de23_build_reads_what_this_build_writes() {
        let h: v128de23::Handover = serde_json::from_slice(&serde_json::to_vec(&golden_handover()).unwrap()).unwrap();
        assert_eq!((h.bye, h.connects, h.machine.as_deref(), h.graphics), (true, true, Some("studio"), Some(true)));
        assert_eq!((h.opened_by_nerd, h.dark, h.reattach.as_deref()), (true, Some(true), Some("nerd --remote studio 1")));
        assert_eq!(h.terminal.as_deref(), Some(Path::new("/Applications/Ghostty.app")));
        let v = serde_json::to_value(golden_handover()).unwrap();
        assert!(v.get("caps").is_none() && v.get("mux").is_none() && v["opens_windows"] == true, "the caps are flat, under the v1 names");
        assert_eq!((h.broker_fd, h.listener_fd, h.client), (3, 4, Some((2, 5, 100, 30, false))));
        assert_eq!(h.panes.iter().map(|p| (p.id, p.fd, p.pid, p.alive, p.unseen)).collect::<Vec<_>>(), [(1, 7, Some(4001), true, false), (2, 8, Some(4002), true, true)]);
        assert_eq!(h.clients[0].ended.as_ref().map(|e| &e.1), Some(&v128de23::End::TakenOver));
        assert_eq!(h.snapshot.version, 2);

        let w: v128de23::WorkspaceSnapshot = serde_json::from_slice(&serde_json::to_vec_pretty(&golden_workspace()).unwrap()).unwrap();
        let tab = &w.spaces[0].tabs[0];
        assert_eq!((w.version, tab.focused, tab.panes.iter().map(|p| p.id).collect::<Vec<_>>()), (2, 2, vec![1, 2]));
        assert_eq!(tab.panes[1].agent_session.as_deref(), Some("0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c"));
        assert!(matches!(&tab.layout, v128de23::Layout::Split { first, .. } if matches!(**first, v128de23::Layout::Pane(1))));
    }
}
