//! Plugin executables: launched by the broker under a Seatbelt profile the daemon builds, proven by
//! the pid the broker returned, spoken to over protocol 2 (docs/plugins.md).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::agents::{Manifest, Network};
use crate::broker::ExecNote;
use crate::log::{error, info, warn};
use crate::proto::{ClientMsg, PluginMsg};
use crate::transport::{Inbound, Inbox};

const HANDSHAKE: Duration = Duration::from_secs(5);
const CRASHES: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(180);
/// Events held for a plugin still starting; past this the oldest are dropped.
const QUEUE: usize = 256;
/// The `how` of a plugin whose broker went, not the plugin itself.
pub const BROKER_ENDED: &str = "the broker for plugins ended";
pub const OLD_BROKER: &str = "restart the server to enable plugins";

/// A launched plugin: `pid` is its proof of role `plugin`, set when the broker answers and dropped when it says the process left.
struct Proof {
    exec: usize,
    name: String,
    pid: Option<i32>,
}

static PROOF: Mutex<Vec<Proof>> = Mutex::new(Vec::new());

fn proofs() -> std::sync::MutexGuard<'static, Vec<Proof>> {
    PROOF.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called from the broker's reply thread, before the loop hears of it: the plugin may connect first.
pub fn started(exec: usize, pid: i32) {
    if let Some(p) = proofs().iter_mut().find(|p| p.exec == exec) {
        p.pid = Some(pid);
    }
}

#[cfg(test)]
pub fn launched_for_test(exec: usize, name: &str, pid: i32) {
    proofs().push(Proof { exec, name: name.into(), pid: Some(pid) });
}

/// Called right after the broker reaped the process, so its pid proves nothing from here on.
pub fn exited(exec: usize) {
    proofs().retain(|p| p.exec != exec);
}

fn descends(pid: i32, from: i32) -> bool {
    pid == from || crate::transport::inside(pid, from, crate::transport::parent_pid)
}

fn proven(name: &str, pid: i32) -> bool {
    proofs().iter().any(|p| p.name == name && p.pid.is_some_and(|from| descends(pid, from)))
}

/// A plugin is inside nerd, and `allow_from_panes` never lets it act as a human's client.
pub fn within_a_plugin(pid: i32) -> bool {
    proofs().iter().any(|p| p.pid.is_some_and(|from| descends(pid, from)))
}

/// Server to plugin, one JSON object per line.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Connected { protocol: u32 },
    AgentState { pane: usize, agent: String, state: String },
    PaneOpened { pane: usize, cwd: PathBuf },
    /// The pane's shell moved to another folder.
    PaneCwd { pane: usize, cwd: PathBuf },
    PaneClosed { pane: usize },
    FileChosen { path: PathBuf },
    Command {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pane: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    /// The selected file's path, to the one source that previews its extension; the room is in cells. `mode`: the view
    /// the user chose last among those this plugin offered.
    PreviewRequest {
        req: u64,
        path: PathBuf,
        cols: u16,
        rows: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
    /// Its panel was opened from the menu: of a pane, or of the tree's file or folder.
    PanelOpened {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pane: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    /// A row of its panel was chosen.
    PanelItem { panel: String, item: String },
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::Connected { .. } => "connected",
            Event::AgentState { .. } => "agent_state",
            Event::PaneOpened { .. } => "pane_opened",
            Event::PaneCwd { .. } => "pane_cwd",
            Event::PaneClosed { .. } => "pane_closed",
            Event::FileChosen { .. } => "file_chosen",
            Event::Command { .. } => "command",
            Event::PreviewRequest { .. } => "preview_request",
            Event::PanelOpened { .. } => "panel_opened",
            Event::PanelItem { .. } => "panel_item",
        }
    }
}

/// Every plugin's menu commands, in manifest order.
pub fn commands() -> Vec<(String, crate::agents::Command)> {
    crate::agents::all().iter().filter(|m| m.exec.is_some()).flat_map(|m| m.commands.iter().map(|c| (m.name.clone(), c.clone()))).collect()
}

/// Every panel a menu may open: the public built-ins' when they are on, then each plugin's, in manifest order.
pub fn panels(builtins: bool) -> Vec<(crate::slots::Src, (String, String, crate::agents::Place))> {
    let ours = crate::slots::BUILTINS.iter().filter(|_| builtins).flat_map(|n| crate::slots::Decl::builtin(n).panels.into_iter().map(move |p| ((false, n.to_string()), p)));
    let theirs = crate::agents::all().iter().filter(|m| m.exec.is_some()).flat_map(|m| crate::slots::Decl::of(m).panels.into_iter().map(move |p| ((true, m.name.clone()), p))).collect::<Vec<_>>();
    ours.chain(theirs).collect()
}

/// (plugin, command id) by the number a menu item carries; only grows, so a number never changes meaning.
static MENU: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// The number for `Action::Plugin`; `u16::MAX` is the separator's and never handed out.
pub fn menu_number(plugin: &str, command: &str) -> Option<u16> {
    let mut t = MENU.lock().unwrap_or_else(|e| e.into_inner());
    let i = match t.iter().position(|(p, c)| p == plugin && c == command) {
        Some(i) => i,
        None => {
            t.push((plugin.to_string(), command.to_string()));
            t.len() - 1
        }
    };
    u16::try_from(i).ok().filter(|i| *i < u16::MAX)
}

pub fn menu_command(n: u16) -> Option<(String, String)> {
    MENU.lock().unwrap_or_else(|e| e.into_inner()).get(n as usize).cloned()
}

pub fn data_dir(name: &str) -> PathBuf {
    crate::paths::state_root().join("plugin-data").join(name)
}

/// Its data folder as a real folder: the plugin may write it, so it may have put a link or a file at the name; that
/// goes, or its next profile would grant it the link's target.
pub fn real_data_dir(name: &str) -> std::io::Result<PathBuf> {
    let data = data_dir(name);
    if std::fs::symlink_metadata(&data).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(&data)?;
    }
    std::fs::create_dir_all(&data)?;
    // Through the folder itself, opened without following: a link put back meanwhile fails here, never gets the mode.
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::fs::OpenOptions::new().read(true).custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW).open(&data)?;
    dir.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(data)
}

/// Whether `file` is a plain file in a plain data folder, with no link on the way: what nerd opens outside the sandbox.
pub fn plain_in_data(name: &str, file: &str) -> bool {
    let data = data_dir(name);
    std::fs::symlink_metadata(&data).is_ok_and(|m| m.is_dir()) && std::fs::symlink_metadata(data.join(file)).is_ok_and(|m| m.is_file())
}

/// The canonical form of a path that may not exist yet: Seatbelt matches `/private/tmp`, not `/tmp`.
pub(crate) fn canon(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) => canon(d).join(n),
        _ => p.to_path_buf(),
    }
}

/// Folders under HOME never readable by a plugin, nor is nerd's state root beyond its own folders.
pub(crate) const SECRET: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    "Library",
    ".docker",
    ".kube",
    ".azure",
    ".config/gh",
    ".config/gcloud",
    ".zsh_sessions",
    ".bash_sessions",
    ".local/share/fish",
    ".local/share/atuin",
];

/// Files under HOME never readable by a plugin, as a prefix: shells rewrite a history through `.new` or `-<pid>.tmp` beside it.
pub(crate) const SECRET_FILES: &[&str] = &[
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".zsh_history",
    ".bash_history",
    ".sh_history",
    ".history",
    ".python_history",
    ".node_repl_history",
    ".psql_history",
    ".mysql_history",
    ".sqlite_history",
    ".irb_history",
    ".lesshst",
];

/// The filters of `(deny file-read-data …)`: the secrets under `home` and nerd's state root.
pub(crate) fn secret_filters(home: &Path) -> Result<String, String> {
    // A link's target is denied too, unless it is HOME or above (every file) or a device: /dev/null turns MySQL's history off.
    let real = canon(home);
    let denies = |t: &PathBuf| !real.starts_with(t) && (t.starts_with(&real) || std::fs::metadata(t).map_or(true, |m| m.is_file() || m.is_dir()));
    let links = SECRET.iter().filter(|s| **s != "Library").flat_map(|s| links_in(&home.join(s)));
    let mut dirs: Vec<PathBuf> = SECRET.iter().map(|s| home.join(s)).chain(links).map(|l| target(&l)).filter(denies).collect();
    dirs.push(canon(&crate::paths::state_root()));
    // A file that is a link: its target, and the shell's rewrite beside the link.
    let files = SECRET_FILES.iter().flat_map(|f| [real.join(f), target(&home.join(f))]).filter(denies);
    let mut out = String::new();
    for d in dirs {
        out += &format!(" (subpath {})", quote(&d)?);
    }
    for f in files {
        out += &format!(" (prefix {})", quote(&f)?);
    }
    Ok(out)
}

/// Where a chain of links ends, 32 hops at most as in the kernel: canon() leaves a dangling link as itself, and its
/// target, once created, must be denied already.
fn target(l: &Path) -> PathBuf {
    let mut p = l.to_path_buf();
    for _ in 0..32 {
        let Ok(t) = std::fs::read_link(&p) else { break };
        p = canon(&p.parent().unwrap_or(&p).join(t));
    }
    canon(&p)
}

/// The links in `dir`, four levels down and 2000 entries at most: read each time nerd loads the manifests.
fn links_in(dir: &Path) -> Vec<PathBuf> {
    let (mut out, mut todo, mut seen) = (Vec::new(), vec![(dir.to_path_buf(), 0)], 0);
    while let Some((d, depth)) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            seen += 1;
            if seen > 2000 {
                return out;
            }
            match e.file_type() {
                Ok(t) if t.is_symlink() => out.push(e.path()),
                Ok(t) if t.is_dir() && depth < 3 => todo.push((e.path(), depth + 1)),
                _ => {}
            }
        }
    }
    out
}

/// Every terminal device: what is typed into a pane, or into nerd's own window, is no plugin's to see.
pub(crate) const TERMINALS: &str = "(deny file-read* file-write* file-ioctl (regex #\"^/dev/(tty|ttys[0-9]+|ttyp[0-9a-f]|pty[p-z][0-9a-f]|ptmx)$\"))\n";

/// `p` as a string in a sandbox profile.
pub(crate) fn quote(p: &Path) -> Result<String, String> {
    let s = p.to_string_lossy();
    if s.contains(['"', '\\']) || s.chars().any(char::is_control) {
        return Err(format!("path {s:?} cannot go into a sandbox profile"));
    }
    Ok(format!("\"{s}\""))
}

/// The Seatbelt profile for `m`, from its validated manifest; see docs/plugins.md for each rule.
pub fn profile(m: &Manifest) -> Result<String, String> {
    let dir = m.dir.as_deref().ok_or("no folder")?;
    let q = quote;
    let home = crate::paths::home().unwrap_or_default();
    // Relative, the denies below would match nothing.
    if !home.is_absolute() || home == Path::new("/") {
        return Err("nerd has no home folder, so it cannot keep a plugin out of ~/.ssh".into());
    }
    // Never resolved through a link at the folder's own name: the plugin can put one there.
    let mut writes = vec![canon(&crate::paths::state_root()).join("plugin-data").join(&m.name)];
    writes.extend(write_paths(m)?);
    let subpaths = |ps: &[PathBuf]| ps.iter().map(|p| q(p).map(|s| format!(" (subpath {s})"))).collect::<Result<String, String>>();
    let own = subpaths(&[dir.to_path_buf(), writes[0].clone()])?;
    // Hidden entries under a write path (.git/hooks, .envrc, .claude) are config other programs run.
    let hidden: String = writes[1..].iter().map(|p| format!(" (regex #\"^{}/(.*/)?\\.[^/]*\")", regex_escape(&p.to_string_lossy()))).collect();
    let hidden = if hidden.is_empty() { String::new() } else { format!("(deny file-write*{hidden})\n") };
    let writes = subpaths(&writes)?;
    let secret = secret_filters(&home)?;
    let sock = q(&canon(&crate::session::plugin_socket_path()))?;
    // Outbound only, so no listening; and never `network*`, which would open every unix socket of the user, nerd's own among them.
    // Hosts and ports get no rule here: the wrapper appends its proxy's port (proxy.rs).
    let network = match &m.permissions.network {
        Network::Localhost => "(allow network-outbound (remote ip \"localhost:*\"))\n",
        Network::None | Network::Internet { .. } => "",
    };
    // The last matching rule wins: its own folders are readable again inside the denied state root.
    Ok(format!(
        "(version 1)\n(deny default)\n(allow process-fork)\n\
         (allow process-exec (subpath \"/bin\") (subpath \"/usr/bin\") (subpath {dir}) (literal {program}))\n\
         (deny process-exec (literal \"/usr/bin/open\") (literal \"/usr/bin/osascript\") (literal \"/bin/launchctl\"))\n\
         (allow file-read*)\n(deny file-read-data{secret})\n(allow file-read-data{own})\n\
         {TERMINALS}\
         (allow file-write* (literal \"/dev/null\"){writes})\n{hidden}\
         (allow sysctl-read)\n(allow signal (target same-sandbox))\n\
         (allow network-outbound (remote unix-socket (path-literal {sock})))\n{network}",
        dir = q(dir)?,
        program = q(&m.program)?,
    ))
}

/// The folders `m` may write besides its data folder, canonical, as its profile grants them.
pub fn write_paths(m: &Manifest) -> Result<Vec<PathBuf>, String> {
    let root = canon(&crate::paths::state_root());
    let home = canon(&crate::paths::home().unwrap_or_default());
    let path = std::env::var("PATH").unwrap_or_default();
    let mut out = Vec::new();
    for w in &m.permissions.write {
        if !(w.starts_with("~/") || w.starts_with('/')) || w.split('/').any(|c| c == "..") {
            return Err(format!("write path {w:?} must start with / or ~/ and hold no .."));
        }
        let p = canon(&crate::paths::resolve(Path::new("/"), w));
        if p.starts_with(&root) || root.starts_with(&p) {
            return Err(format!("write path {w:?} would reach nerd's own files"));
        }
        if let Some(why) = runs_later(&p, &home, &path) {
            return Err(format!("write path {w:?} {why}"));
        }
        out.push(p);
    }
    Ok(out)
}

/// What a plugin can do, from its validated manifest; `Display` is the words the user is asked with.
/// Kinds, not counts, so an update that adds a second menu item asks nothing new.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ability {
    Runs(PathBuf),
    /// A program inside its own folder, relative to it: the same wherever the folder is.
    RunsOwn(PathBuf),
    Commands,
    Panels,
    Segments,
    Badges(String),
    PaneFolders,
    PaneMoves,
    AgentStates,
    FilesOpened,
    FilesSelected(Vec<String>),
    Writes(PathBuf),
    Network(Network),
    Agent { names: Vec<String>, config: Option<String>, resume: Vec<String>, network: Network },
}

impl std::fmt::Display for Ability {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Ability::Commands => write!(f, "Adds items to nerd's menus"),
            Ability::Panels => write!(f, "Opens panels of its own"),
            Ability::Segments => write!(f, "Shows text in the top row"),
            Ability::Badges(t) if t == "tab" => write!(f, "Shows badges on tabs"),
            Ability::Badges(_) => write!(f, "Shows badges in the tree"),
            Ability::PaneFolders => write!(f, "Learns the folder each pane opens in"),
            Ability::PaneMoves => write!(f, "Learns which folder each pane is in, as it changes"),
            Ability::AgentStates => write!(f, "Learns what each agent is doing"),
            Ability::FilesOpened => write!(f, "Learns which files you open"),
            Ability::FilesSelected(exts) => write!(f, "Learns which {} files you select, and reads them", exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(", ")),
            Ability::Runs(p) => write!(f, "Runs {}", p.display()),
            Ability::RunsOwn(p) => write!(f, "Runs its own {}", p.display()),
            Ability::Writes(p) => write!(f, "Writes to {}; a program that later reads it may act on what it wrote", p.display()),
            Ability::Network(Network::Localhost) => write!(f, "Reaches services on this Mac (localhost) and anything they pass on, and could send them anything it can read, the environment of your other programs included"),
            Ability::Network(Network::Internet { hosts, ports }) => {
                write!(f, "Reaches only {} on {} {}, through nerd's filter: a name at whatever public address its owner gives it, never this Mac or a network it is on; it looks up no names itself", hosts.join(", "), if ports.len() == 1 { "port" } else { "ports" }, crate::agents::join(ports))?;
                let near = near(hosts, crate::proxy::interfaces().ok().as_deref());
                if !near.is_empty() {
                    write!(f, "; reaches your local network or this Mac at {}", near.join(", "))?;
                }
                write!(f, "; could send anything it can read to those hosts, the environment of your other programs included")
            }
            Ability::Network(Network::None) => write!(f, "Reaches no network"),
            Ability::Agent { names, config, resume, network } => {
                write!(f, "Speaks for the agent {}", names.join(", "))?;
                if let Network::Internet { hosts, ports } = network {
                    write!(f, ", lets a pane set to Only these providers reach {} on {} {}", hosts.join(", "), if ports.len() == 1 { "port" } else { "ports" }, crate::agents::join(ports))?;
                }
                if let Some(c) = config {
                    write!(f, ", adding nerd's hooks to {c}")?;
                }
                if !resume.is_empty() {
                    write!(f, ", and restores its panes by typing `{}`", resume.join(" "))?;
                }
                Ok(())
            }
        }
    }
}

/// The manifest's addresses on this Mac or its networks, all of them when this Mac's own cannot be read: the prompt names them.
fn near<'a>(hosts: &'a [String], nets: Option<&[(std::net::IpAddr, std::net::IpAddr)]>) -> Vec<&'a str> {
    hosts.iter().map(String::as_str).filter(|h| h.parse().is_ok_and(|a| !crate::proxy::public(a) || nets.is_none_or(|n| crate::proxy::local(a, n)))).collect()
}

impl Ability {
    /// 0 shows, 1 writes, 2 reaches the network, 3 hooks into an agent.
    pub fn risk(&self) -> u8 {
        match self {
            Ability::Writes(_) => 1,
            Ability::Network(n) if *n != Network::None => 2,
            Ability::Agent { .. } => 3,
            _ => 0,
        }
    }
}

/// What `now` can do that `before` could not; a network inside the one approved before is nothing new.
pub fn new_abilities(now: &std::collections::BTreeSet<Ability>, before: &std::collections::BTreeSet<Ability>) -> Vec<Ability> {
    now.iter()
        .filter(|a| !before.contains(a))
        .filter(|a| !matches!(a, Ability::Network(n) if before.iter().any(|b| matches!(b, Ability::Network(w) if n.within(w)))))
        .cloned()
        .collect()
}

pub fn abilities(m: &Manifest) -> Result<std::collections::BTreeSet<Ability>, String> {
    let mut out = std::collections::BTreeSet::new();
    let has = |e: &str| m.events.iter().any(|x| x == e);
    if m.exec.is_some() {
        let own = m.dir.as_deref().and_then(|d| m.program.strip_prefix(d).ok());
        out.insert(own.map_or_else(|| Ability::Runs(m.program.clone()), |p| Ability::RunsOwn(p.to_path_buf())));
    }
    for (on, a) in [(!m.commands.is_empty(), Ability::Commands), (!m.panels.is_empty(), Ability::Panels), (!m.segments.is_empty(), Ability::Segments)] {
        if on {
            out.insert(a);
        }
    }
    out.extend(m.badges.iter().map(|b| Ability::Badges(b.clone())));
    for (e, a) in [("pane_opened", Ability::PaneFolders), ("pane_cwd", Ability::PaneMoves), ("agent_state", Ability::AgentStates), ("file_chosen", Ability::FilesOpened)] {
        if has(e) {
            out.insert(a);
        }
    }
    let exts: Vec<String> = m.previews.iter().flat_map(|p| p.extensions.clone()).collect();
    if !exts.is_empty() {
        out.insert(Ability::FilesSelected(exts));
    }
    out.extend(write_paths(m)?.into_iter().map(Ability::Writes));
    if m.permissions.network != Network::None {
        out.insert(Ability::Network(m.permissions.network.clone()));
    }
    if !m.agent.names.is_empty() {
        out.insert(Ability::Agent { names: m.agent.names.clone(), config: m.agent.config.as_ref().map(|c| c.path.clone()), resume: m.agent.resume.clone(), network: m.agent.network.net.clone() });
    }
    Ok(out)
}

pub fn risk(abilities: &std::collections::BTreeSet<Ability>) -> u8 {
    abilities.iter().map(Ability::risk).max().unwrap_or(0)
}

/// Why writing `p` would let a plugin run code outside its sandbox later, if it would. Compared lowercased:
/// the disk ignores case, and Seatbelt matches `~/library` as `~/Library`.
fn runs_later(p: &Path, home: &Path, path: &str) -> Option<&'static str> {
    // APFS folds more than ASCII case (ſ is s); a name outside ASCII could name a folder refused below.
    if !p.to_string_lossy().is_ascii() {
        return Some("must be spelled in ASCII letters");
    }
    // Without a real home folder every path would be inside it.
    if home.as_os_str().is_empty() || home == Path::new("/") {
        return Some("needs a home folder, and nerd has none");
    }
    let low = |p: &Path| PathBuf::from(p.to_string_lossy().to_ascii_lowercase());
    let (p, home) = (low(p), low(home));
    let Ok(rest) = p.strip_prefix(&home) else { return Some("must be inside your home folder") };
    let parts: Vec<String> = rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let first = parts.first().cloned().unwrap_or_default();
    // .git/hooks, .envrc, .claude: another program runs what is written there.
    if first.is_empty() || parts.iter().any(|c| c.starts_with('.')) || first == "library" || first == "applications" {
        return Some("would reach settings or programs other apps run: use a folder of your own under ~/");
    }
    let on_path = path.split(':').filter(|d| !d.is_empty()).map(|d| low(&canon(Path::new(d))));
    if on_path.into_iter().any(|d| d.starts_with(&p) || p.starts_with(&d)) {
        return Some("would reach a folder on PATH, whose programs run outside the sandbox");
    }
    None
}

pub(crate) fn regex_escape(s: &str) -> String {
    s.chars().flat_map(|c| if "^$.|?*+()[]{}\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// `nerd --plugin-exec <program> [args]`: the daemon's profile, then the plugin; with `NERD_PLUGIN_NET`, forked off its proxy.
pub fn exec_main(args: &[String]) -> std::io::Result<()> {
    let profile = std::env::var("NERD_PLUGIN_PROFILE").map_err(|_| std::io::Error::other("no sandbox profile given"))?;
    let program = args.first().ok_or_else(|| std::io::Error::other("no program given"))?;
    let mut cmd = crate::spawn::cmd(program);
    // Only these reach it: a token in the broker's environment is not the plugin's to read.
    cmd.args(&args[1..]).env_clear().envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(passed)));
    cmd.env("PATH", plugin_path(&std::env::var("PATH").unwrap_or_default()));
    let Ok(net) = std::env::var("NERD_PLUGIN_NET") else { return become_plugin(cmd, &profile) };
    let net = crate::proxy::Net::parse(&net).ok_or_else(|| std::io::Error::other("NERD_PLUGIN_NET does not parse"))?;
    let listeners = crate::proxy::bind()?;
    let port = listeners.0.local_addr()?.port();
    for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        cmd.env(k, format!("http://127.0.0.1:{port}"));
    }
    cmd.env("NO_PROXY", "").env("no_proxy", "");
    let profile = format!("{profile}(allow network-outbound (remote ip \"localhost:{port}\"))\n");
    // SAFETY: no thread exists yet, so the child may allocate; it never returns into the proxy's code.
    match unsafe { nix::unistd::fork() }? {
        nix::unistd::ForkResult::Child => {
            if let Err(e) = become_plugin(cmd, &profile) {
                let _ = writeln!(std::io::stderr(), "nerd: {e}");
            }
            // SAFETY: ends the child without running the parent's exit handlers.
            unsafe { nix::libc::_exit(127) }
        }
        nix::unistd::ForkResult::Parent { child } => crate::proxy::run(child, net, listeners, None),
    }
}

/// Applies `profile` and execs `cmd`; returns only on failure.
fn become_plugin(mut cmd: std::process::Command, profile: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    crate::sandbox::apply_text(profile).map_err(|e| std::io::Error::other(format!("sandbox not applied: {e}")))?;
    // Opened inside the sandbox and never through a link: the plugin can write its data folder, so the name is its to swap.
    if let Some(log) = std::env::var_os("NERD_PLUGIN_LOG") {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _ = std::fs::remove_file(&log);
        let f = std::fs::OpenOptions::new().write(true).create_new(true).custom_flags(nix::libc::O_NOFOLLOW).mode(0o600).open(log)?;
        cmd.stdout(f.try_clone()?).stderr(f);
    }
    Err(cmd.exec())
}

/// The server's PATH, then the folders a plugin's command was looked for in: under a bare PATH it still finds its own program by name.
fn plugin_path(path: &str) -> String {
    path.split(':').filter(|d| !d.is_empty()).chain(crate::agents::PATH_FALLBACK).collect::<Vec<_>>().join(":")
}

/// The environment a plugin gets: its own variables, and what a program needs to run at all.
fn passed(key: &str) -> bool {
    matches!(key, "NERD_SOCKET" | "NERD_PLUGIN" | "NERD_PLUGIN_DATA" | "TMPDIR" | "PATH" | "HOME" | "USER" | "LOGNAME" | "SHELL" | "LANG" | "TZ") || key.starts_with("LC_")
}

/// Says why in one line, logs it, and ends the connection.
pub fn refuse(out: &UnixStream, why: &str) {
    let pid = nix::sys::socket::getsockopt(out, nix::sys::socket::sockopt::LocalPeerPid).unwrap_or(0);
    crate::log::warn_limited("plugin claim refused", &format!("refused a plugin claim from pid {pid}: {why}"));
    let _ = (&*out).write_all(format!("refused: {why}\n").as_bytes());
    let _ = out.shutdown(std::net::Shutdown::Both);
}

/// The plugin socket: the one socket a plugin's sandbox reaches, and it serves nothing but `Connect`.
pub fn bind(path: &Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Connections the plugin socket holds at once: each takes a thread, and a plugin could open them until the server
/// cannot start one more and dies with every pane.
const CONNECTIONS: usize = 64;
/// Of those, what one plugin may hold, counted by the plugin its peer descends from, so none can lock the others
/// out; peers that descend from no started plugin share one larger allowance.
const PER_PLUGIN: usize = 4;
const UNOWNED: usize = 16;
static HELD: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
/// What a `Connect` may take, read before anything is proven.
const CONNECT_BYTES: u64 = 64 * 1024;
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One of the `CONNECTIONS` and of its owner's share, given back when its thread ends.
struct Slot(String);
impl Drop for Slot {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = HELD.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|(k, _)| *k == self.0) {
            h.1 = h.1.saturating_sub(1);
        }
    }
}

/// A slot for a peer, or why not: the plugin it descends from (empty for none) and how many that one holds.
fn take_slot(pid: i32) -> Result<Slot, String> {
    let owner = proofs().iter().find(|p| p.pid.is_some_and(|from| descends(pid, from))).map(|p| p.name.clone()).unwrap_or_default();
    let cap = if owner.is_empty() { UNOWNED } else { PER_PLUGIN };
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    let n = match held.iter_mut().find(|(k, _)| *k == owner) {
        Some(h) => &mut h.1,
        None => {
            held.push((owner.clone(), 0));
            &mut held.last_mut().unwrap_or_else(|| unreachable!()).1
        }
    };
    let who = if owner.is_empty() { "no started plugin".to_string() } else { format!("plugin {owner}") };
    if *n >= cap {
        return Err(format!("pid {pid} ({who}) already holds {cap}"));
    }
    if LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= CONNECTIONS {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        return Err(format!("{CONNECTIONS} are open; pid {pid} ({who})"));
    }
    *n += 1;
    Ok(Slot(owner))
}

pub fn listen(listener: std::os::unix::net::UnixListener, tx: Inbox) {
    std::thread::spawn(move || {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { nix::libc::geteuid() };
        for (n, stream) in listener.incoming().flatten().enumerate() {
            let peer = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerCred).ok().map(|c| c.uid());
            if !crate::transport::same_user(peer, me) {
                warn(&format!("refused a connection to the plugin socket from uid {peer:?}"));
                continue;
            }
            let pid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerPid).unwrap_or(0);
            let slot = match take_slot(pid) {
                Ok(s) => s,
                Err(why) => {
                    // No pause here: a peer reconnecting in a loop would keep the listen queue full and others out.
                    crate::log::warn_limited("plugin socket full", &format!("refused a connection to the plugin socket: {why}"));
                    continue;
                }
            };
            let tx = tx.clone();
            let started = std::thread::Builder::new().spawn(move || {
                let _slot = slot;
                let Ok(copy) = stream.try_clone() else { return };
                // A deadline for the whole Connect, not per read: dripping a byte at a time does not keep it open.
                let _ = copy.set_read_timeout(Some(Duration::from_millis(200)));
                let mut reader = BufReader::new(copy);
                let first = match crate::trial::next_line(&mut reader, Instant::now() + HANDSHAKE, CONNECT_BYTES) {
                    crate::trial::Read::Line(l) => l,
                    _ => String::new(),
                };
                let _ = reader.get_ref().set_read_timeout(None);
                match ClientMsg::read_from(&mut first.as_bytes()) {
                    Ok(Some(ClientMsg::Connect { protocol, role, plugin, events })) => serve(n as u64 + 1, stream, reader, protocol, &role, plugin, events, &tx),
                    _ => refuse(&stream, "the plugin socket serves only Connect"),
                }
            });
            if started.is_err() {
                crate::log::warn_limited("plugin socket no thread", "refused a connection to the plugin socket: no thread left for it");
            }
        }
    });
}

/// A `Connect` on the plugin socket, on the connection's reader thread: refused unless the broker launched this plugin.
#[allow(clippy::too_many_arguments)]
pub fn serve(id: u64, out: UnixStream, reader: BufReader<UnixStream>, protocol: u32, role: &str, plugin: Option<String>, events: Vec<String>, tx: &Inbox) {
    let pid = nix::sys::socket::getsockopt(&out, nix::sys::socket::sockopt::LocalPeerPid).unwrap_or(0);
    let name = plugin.unwrap_or_default();
    let why = if role != "plugin" {
        Some(format!("role {role:?} is not served"))
    } else if protocol < crate::proto::PROTOCOL {
        Some(format!("protocol {protocol} is below {}", crate::proto::PROTOCOL))
    } else if !proven_soon(&name, pid) {
        Some(format!("plugin {name:?} was not started by this server"))
    } else {
        None
    };
    if let Some(why) = why {
        refuse(&out, &why);
        return;
    }
    if tx.send(Inbound::PluginUp { name: name.clone(), id, stream: out, events }).is_err() {
        return;
    }
    read_loop(&name, id, reader, tx);
    let _ = tx.send(Inbound::PluginDown { name, id });
}

/// Between two batches to the loop: at most 20 a second.
const FORWARD: Duration = Duration::from_millis(50);

/// Slot messages waiting for the next batch: the latest per key, and whether the connection ended.
type Latest = std::sync::Arc<(Mutex<(std::collections::BTreeMap<String, PluginMsg>, bool)>, std::sync::Condvar)>;

/// Reads a plugin's lines until its connection ends. Anything but a `PluginMsg` is ignored: a plugin cannot attach, stop or update.
fn read_loop(name: &str, id: u64, mut reader: BufReader<UnixStream>, tx: &Inbox) {
    let latest: Latest = Default::default();
    let forward = {
        let (latest, tx, who) = (latest.clone(), tx.clone(), name.to_string());
        let started = std::thread::Builder::new().spawn(move || {
            loop {
                let (msgs, done) = {
                    let (m, cv) = &*latest;
                    let mut g = cv.wait_while(m.lock().unwrap_or_else(|e| e.into_inner()), |(q, done)| q.is_empty() && !*done).unwrap_or_else(|e| e.into_inner());
                    (std::mem::take(&mut g.0).into_values().collect::<Vec<_>>(), g.1)
                };
                if !msgs.is_empty() && tx.send(Inbound::PluginSlots { name: who.clone(), id, msgs }).is_err() || done {
                    return;
                }
                std::thread::sleep(FORWARD);
            }
        });
        match started {
            Ok(t) => t,
            Err(_) => return crate::log::warn_limited("plugin socket no thread", &format!("plugin {name}: no thread left for its messages")),
        }
    };
    // One WARN of each kind per connection, however much a plugin sends.
    let (mut warned, mut unknown) = (false, false);
    loop {
        match crate::proto::read_json::<PluginMsg>(&mut reader) {
            Ok(Some(PluginMsg::ReportState { pane, state })) => {
                if tx.send(Inbound::PluginSaid { name: name.to_string(), id, pane, state }).is_err() {
                    break;
                }
            }
            Ok(Some(m)) => {
                if let Some(why) = crate::slots::over_cap(&m) {
                    if !std::mem::replace(&mut warned, true) {
                        crate::log::warn_limited("plugin message over cap", &format!("plugin {name}: {} refused: {why}", m.name()));
                    }
                    continue;
                }
                let key = match &m {
                    PluginMsg::Segment { id, .. } | PluginMsg::SegmentClear { id } => format!("segment {id}"),
                    PluginMsg::Badges { target, .. } => format!("badges {target}"),
                    PluginMsg::Preview { .. } => "preview".into(),
                    PluginMsg::Panel { id, .. } => format!("panel {id}"),
                    PluginMsg::ReportState { .. } => continue,
                };
                let (q, cv) = &*latest;
                q.lock().unwrap_or_else(|e| e.into_inner()).0.insert(key, m);
                cv.notify_one();
            }
            Err(e) if e.kind() == std::io::ErrorKind::Other => {
                if !std::mem::replace(&mut unknown, true) {
                    crate::log::warn_limited("plugin message unrecognized", &format!("plugin {name}: unrecognized message: {}", crate::log::escape(&e.to_string())));
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    let (q, cv) = &*latest;
    q.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
    cv.notify_one();
    let _ = forward.join();
}

/// The broker's answer and the plugin's connect race; the answer is a line away, so a second is plenty.
fn proven_soon(name: &str, pid: i32) -> bool {
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        if proven(name, pid) {
            return true;
        }
        if Instant::now() >= until || !proofs().iter().any(|p| p.name == name && p.pid.is_none()) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Conn {
    id: u64,
    writer: crate::transport::Writer,
    events: HashSet<String>,
}

impl Conn {
    fn send(&self, ev: &Event) {
        if matches!(ev, Event::Connected { .. }) || self.events.contains(ev.name()) {
            let mut line = serde_json::to_vec(ev).unwrap_or_default();
            line.push(b'\n');
            let _ = self.writer.send(line);
        }
    }
}

struct Run {
    exec: usize,
    since: Instant,
    conn: Option<Conn>,
    queue: VecDeque<Event>,
    /// nerd asked it to stop: its exit is not a crash.
    stopping: bool,
    /// It connected once, so the handshake clock no longer applies.
    greeted: bool,
}

/// The loop's view of every plugin; each starts on first use.
pub struct Plugins {
    runs: HashMap<String, Run>,
    crashes: HashMap<String, Vec<Instant>>,
    disabled: HashSet<String>,
    next: usize,
    panes: HashMap<usize, Pane>,
    /// What plugins and public built-ins show; a plugin's part goes when it stops.
    pub slots: crate::slots::Store,
    /// Plugins with slots run from the server's start; off while stopping for good.
    pub autostart: bool,
    /// Slot plugins whose start failed: tried again after Reload plugins and agents.
    tried: HashSet<String>,
}

/// A pane as plugins see it: its folder, and its agent with that agent's state.
type Pane = (PathBuf, Option<(String, String)>);

impl Default for Plugins {
    fn default() -> Self {
        // Ids start at the clock: an old build's exit notice arriving after a handover names no plugin of ours.
        let next = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as usize);
        Self {
            runs: HashMap::new(),
            crashes: HashMap::new(),
            disabled: HashSet::new(),
            next,
            panes: HashMap::new(),
            slots: Default::default(),
            autostart: false,
            tried: HashSet::new(),
        }
    }
}

/// Keeps the crashes inside the window, adds this one, and says whether the budget is spent.
fn over_budget(times: &mut Vec<Instant>, now: Instant) -> bool {
    times.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
    times.push(now);
    times.len() >= CRASHES
}

impl Plugins {
    /// To every plugin that declared this event; a stopped one starts, a connected one that did not subscribe gets nothing.
    pub fn emit(&mut self, ev: Event) {
        for m in crate::agents::all().iter().filter(|m| m.exec.is_some() && m.events.iter().any(|e| e == ev.name())) {
            let _ = self.deliver(m, ev.clone());
        }
    }

    /// An event for one plugin, a menu command or a preview request; the error is for the human.
    pub fn command(&mut self, name: &str, ev: Event) -> Result<(), String> {
        let m = crate::agents::get(name).ok_or_else(|| format!("plugin {name} is gone; Reload plugins and agents"))?;
        self.deliver(&m, ev)
    }

    fn deliver(&mut self, m: &Manifest, ev: Event) -> Result<(), String> {
        if !self.runs.contains_key(&m.name) {
            self.start(m)?;
        }
        let Some(run) = self.runs.get_mut(&m.name) else { return Ok(()) };
        if run.stopping {
            return Err(format!("plugin {} is stopping; choose it again in a moment", m.name));
        }
        match &run.conn {
            Some(c) => c.send(&ev),
            None => {
                if run.queue.len() >= QUEUE {
                    run.queue.pop_front();
                }
                run.queue.push_back(ev);
            }
        }
        Ok(())
    }

    fn start(&mut self, m: &Manifest) -> Result<(), String> {
        if self.disabled.contains(&m.name) {
            return Err(format!("plugin {} is off after {CRASHES} crashes; Reload plugins and agents turns it back on", m.name));
        }
        if crate::broker::exec_coming() {
            return Err(format!("plugin {} is starting; choose it again in a moment", m.name));
        }
        let broker = crate::broker::exec_handle().ok_or(OLD_BROKER)?;
        let (Some(exec), Some(dir)) = (&m.exec, &m.dir) else { return Err(format!("plugin {} has nothing to run", m.name)) };
        let data = real_data_dir(&m.name).map_err(|e| format!("plugin {}: data folder: {e}", m.name))?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let argv = [exe.to_string_lossy().into_owned(), "--plugin-exec".into(), m.program.to_string_lossy().into_owned()].into_iter().chain(exec.args.iter().cloned()).collect();
        let data_s = canon(&data).to_string_lossy().into_owned();
        let mut env = vec![
            ("NERD_PLUGIN_PROFILE".into(), m.profile.clone()),
            ("NERD_PLUGIN_LOG".into(), format!("{data_s}/output.log")),
            // As bound, not canonical: /private makes a long path pass the 104-byte socket limit.
            ("NERD_SOCKET".into(), crate::session::plugin_socket_path().to_string_lossy().into_owned()),
            ("NERD_PLUGIN".into(), m.name.clone()),
            ("NERD_PLUGIN_DATA".into(), data_s.clone()),
            ("TMPDIR".into(), data_s),
        ];
        if let Network::Internet { hosts, ports } = &m.permissions.network {
            env.push(("NERD_PLUGIN_NET".into(), crate::proxy::Net::env(hosts, ports)));
        }
        let id = self.next;
        self.next += 1;
        proofs().push(Proof { exec: id, name: m.name.clone(), pid: None });
        if let Err(e) = broker.exec(id, argv, env, dir.to_path_buf()) {
            exited(id);
            return Err(format!("plugin {}: the broker did not take it: {e}", m.name));
        }
        info(&format!("plugin {} started: runs {}, network {}", m.name, m.program.display(), m.permissions.network.said()));
        self.runs.insert(m.name.clone(), Run { exec: id, since: Instant::now(), conn: None, queue: VecDeque::new(), stopping: false, greeted: false });
        Ok(())
    }

    /// An id for another process the broker runs, from the plugins' own count so no note is taken for the other's.
    pub fn take_id(&mut self) -> usize {
        self.next += 1;
        self.next - 1
    }

    fn by_exec(&self, exec: usize) -> Option<String> {
        self.runs.iter().find(|(_, r)| r.exec == exec).map(|(n, _)| n.clone())
    }

    /// What the broker said; an id this build did not start (one from before a handover) is ignored.
    pub fn note(&mut self, n: ExecNote) {
        match n {
            ExecNote::Started { id, pid } => {
                if let Some(name) = self.by_exec(id) {
                    info(&format!("plugin {name}: started, pid {pid}"));
                }
            }
            ExecNote::Failed { id, message } => {
                exited(id);
                if let Some(name) = self.by_exec(id) {
                    warn(&format!("plugin {name}: did not start: {message}"));
                    self.runs.remove(&name);
                    self.crashed(&name);
                }
            }
            ExecNote::Exited { id, how } => {
                exited(id);
                let Some(name) = self.by_exec(id) else { return };
                let Some(run) = self.runs.remove(&name) else { return };
                if let Some(c) = run.conn {
                    c.writer.retire();
                }
                self.slots.clear(&name);
                if run.stopping {
                    info(&format!("plugin {name}: stopped"));
                } else {
                    match how.as_deref() {
                        Some(BROKER_ENDED) => warn(&format!("plugin {name}: ended with the broker for plugins")),
                        how => warn(&format!("plugin {name}: exited on its own ({})", how.unwrap_or("an older broker does not say how"))),
                    }
                    self.crashed(&name);
                }
            }
        }
    }

    fn crashed(&mut self, name: &str) {
        let times = self.crashes.entry(name.to_string()).or_default();
        let spent = over_budget(times, Instant::now());
        warn(&format!("plugin {name}: crash {} of {CRASHES} within {} s", times.len(), CRASH_WINDOW.as_secs()));
        self.slots.clear(name);
        if spent && self.disabled.insert(name.to_string()) {
            error(&format!("plugin {name}: {CRASHES} crashes within {} s, disabled until Reload plugins and agents", CRASH_WINDOW.as_secs()));
        }
    }

    pub fn up(&mut self, name: String, id: u64, stream: UnixStream, events: Vec<String>, bell: crate::bell::Bell) {
        let declared: HashSet<String> = crate::agents::get(&name).map(|m| m.events.into_iter().collect()).unwrap_or_default();
        // A plugin that starts after the panes did hears them as they are now, however late it came.
        let now = pane_events(&HashMap::new(), &self.panes);
        let Some(run) = self.runs.get_mut(&name).filter(|r| !r.greeted && !r.stopping) else {
            refuse(&stream, &format!("plugin {name:?} is already connected"));
            return;
        };
        run.greeted = true;
        let Ok(writer) = crate::transport::Writer::spawn(stream, bell) else { return };
        let events: HashSet<String> = events.into_iter().filter(|e| declared.contains(e)).collect();
        let mut list: Vec<&str> = events.iter().map(String::as_str).collect();
        list.sort();
        info(&format!("plugin {name}: connected, events {}", list.join(",")));
        let c = Conn { id, writer, events };
        c.send(&Event::Connected { protocol: crate::proto::PROTOCOL });
        for ev in &now {
            c.send(ev);
        }
        // What was queued about panes is older than what was just sent.
        for ev in run.queue.drain(..).filter(|e| !matches!(e, Event::PaneOpened { .. } | Event::PaneCwd { .. } | Event::PaneClosed { .. } | Event::AgentState { .. })) {
            c.send(&ev);
        }
        run.conn = Some(c);
    }

    /// Its socket closed: by contract it exits, and nerd makes sure.
    pub fn down(&mut self, name: &str, id: u64) {
        let Some(run) = self.runs.get_mut(name).filter(|r| r.conn.as_ref().is_some_and(|c| c.id == id)) else { return };
        if let Some(c) = run.conn.take() {
            c.writer.retire();
        }
        self.slots.clear(name);
        // Not marked as nerd's stop: whether it leaves by itself or is ended here, its exit counts as a crash.
        if !run.stopping
            && let Some(b) = crate::broker::exec_handle()
        {
            b.stop_exec(run.exec);
        }
    }

    /// The connection that speaks for this plugin now; a report from an older one is dropped.
    pub fn current(&self, name: &str, id: u64) -> bool {
        self.runs.get(name).and_then(|r| r.conn.as_ref()).is_some_and(|c| c.id == id)
    }

    fn stop(&mut self, name: &str) {
        self.slots.clear(name);
        if let Some(run) = self.runs.get_mut(name).filter(|r| !r.stopping) {
            run.stopping = true;
            if let Some(b) = crate::broker::exec_handle() {
                b.stop_exec(run.exec);
            }
        }
    }

    /// Before a handover or an exit: they start again lazily in the next build.
    pub fn stop_all(&mut self) {
        self.autostart = false;
        let names: Vec<String> = self.runs.keys().cloned().collect();
        for n in names {
            self.stop(&n);
        }
    }

    /// In the Plugins window's words.
    pub fn state_of(&self, name: &str) -> &'static str {
        if self.disabled.contains(name) {
            "off after crashes"
        } else if self.runs.contains_key(name) {
            "running"
        } else if crate::broker::exec_coming() {
            "starting"
        } else if crate::broker::handle().is_some() && crate::broker::exec_handle().is_none() {
            // Only when the broker of this build beside the old one did not come up.
            "needs a server restart"
        } else {
            "not started"
        }
    }

    /// One plugin changed on disk: stopped, its crash count and failed start forgotten; it starts again on next use.
    pub fn restart(&mut self, name: &str) {
        self.stop(name);
        self.disabled.remove(name);
        self.crashes.remove(name);
        self.tried.remove(name);
    }

    /// Reload plugins and agents: manifests are read again, so running plugins restart on next use and disabled ones get another try.
    pub fn reload(&mut self) {
        let on = self.autostart;
        self.stop_all();
        self.autostart = on;
        self.disabled.clear();
        self.crashes.clear();
        self.tried.clear();
    }

    /// Plugins that declare a slot run while the server does: started here, and again once a stop or crash has ended them.
    fn start_slots(&mut self) {
        if crate::broker::exec_coming() {
            return;
        }
        let all = crate::agents::all();
        for m in all.iter().filter(|m| m.exec.is_some() && (!m.badges.is_empty() || !m.segments.is_empty() || !m.previews.is_empty() || !m.panels.is_empty())) {
            if self.runs.contains_key(&m.name) || self.disabled.contains(&m.name) || self.tried.contains(&m.name) {
                continue;
            }
            if let Err(e) = self.start(m) {
                self.tried.insert(m.name.clone());
                warn(&format!("plugin {}: not started: {e}", m.name));
            }
        }
    }

    /// A slot message from this plugin's current connection, checked against its manifest.
    pub fn slot(&mut self, name: &str, msg: PluginMsg) {
        match crate::agents::get(name) {
            Some(m) => {
                self.slots.take(&crate::slots::Decl::of(&m), msg);
            }
            None => warn(&format!("plugin {name}: slot message dropped: no manifest")),
        }
    }

    /// Kills a plugin that did not `Connect` in time; that counts as a crash.
    pub fn tick(&mut self) {
        if self.autostart {
            self.start_slots();
        }
        let late: Vec<String> = self.runs.iter().filter(|(_, r)| !r.greeted && !r.stopping && r.since.elapsed() >= HANDSHAKE).map(|(n, _)| n.clone()).collect();
        for name in late {
            warn(&format!("plugin {name}: no Connect within {} s, stopped", HANDSHAKE.as_secs()));
            self.stop(&name);
            self.crashed(&name);
        }
    }

    /// The panes as they are now: what appeared, left, or changed its agent's state since the last call goes out as events.
    pub fn panes(&mut self, now: HashMap<usize, Pane>) {
        let evs = pane_events(&self.panes, &now);
        self.panes = now;
        for ev in evs {
            self.emit(ev);
        }
    }
}

/// What changed between two looks at the panes, as events.
fn pane_events(before: &HashMap<usize, Pane>, now: &HashMap<usize, Pane>) -> Vec<Event> {
    let mut ids: Vec<usize> = now.keys().chain(before.keys()).copied().collect();
    ids.sort();
    ids.dedup();
    let mut evs = Vec::new();
    {
        for id in ids {
            let (was, is) = (before.get(&id), now.get(&id));
            match (was, is) {
                (None, Some((cwd, _))) => evs.push(Event::PaneOpened { pane: id, cwd: cwd.clone() }),
                (Some(_), None) => evs.push(Event::PaneClosed { pane: id }),
                (Some((was, _)), Some((cwd, _))) if was != cwd => evs.push(Event::PaneCwd { pane: id, cwd: cwd.clone() }),
                _ => {}
            }
            if let Some((_, Some((agent, state)))) = is
                && was.and_then(|w| w.1.as_ref()) != Some(&(agent.clone(), state.clone()))
            {
                evs.push(Event::AgentState { pane: id, agent: agent.clone(), state: state.clone() });
            }
        }
    }
    evs
}

/// Who previews files of this extension: the public built-in when it is on, else the first plugin by name.
pub fn preview_owner(ext: &str, builtins: bool, all: &[Manifest]) -> Option<crate::slots::Src> {
    if builtins && crate::previews::EXTENSIONS.contains(&ext) {
        return Some((false, "previews".into()));
    }
    all.iter()
        .filter(|m| m.exec.is_some() && m.previews.iter().any(|p| p.extensions.iter().any(|e| e == ext)))
        .map(|m| (true, m.name.clone()))
        .min()
}

/// `report_state` is taken only for a pane whose agent is this plugin's, and only as one of the hook's words.
pub fn may_report(plugin: &str, pane_agent: Option<&str>, state: &str) -> Result<(), String> {
    if pane_agent != Some(plugin) {
        return Err(format!("the pane's agent is {}, not {plugin}", pane_agent.unwrap_or("none")));
    }
    if !["working", "idle", "blocked", "failed"].contains(&state) {
        return Err(format!("{state:?} is not a state"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_in_a_secret_folder_or_a_linked_file_denies_its_target_unless_home_above_or_a_device() {
        let home = std::env::temp_dir().join(format!("nerd-secret-links-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".ssh/deep")).unwrap();
        std::fs::create_dir_all(home.join("dotfiles")).unwrap();
        std::os::unix::fs::symlink(home.join("dotfiles"), home.join(".ssh/deep/config")).unwrap();
        std::os::unix::fs::symlink(&home, home.join(".ssh/home")).unwrap();
        std::os::unix::fs::symlink("/", home.join(".ssh/root")).unwrap();
        std::os::unix::fs::symlink("/dev/null", home.join(".ssh/null")).unwrap();
        std::fs::write(home.join("dotfiles/zsh_history"), "").unwrap();
        std::os::unix::fs::symlink(home.join("dotfiles/zsh_history"), home.join(".zsh_history")).unwrap();
        std::os::unix::fs::symlink("/dev/null", home.join(".bash_history")).unwrap();
        // Dangling: what is created there later must be denied already.
        std::os::unix::fs::symlink(home.join("gone/sqlite"), home.join(".sqlite_history")).unwrap();
        std::os::unix::fs::symlink(home.join("gone/docker"), home.join(".docker")).unwrap();
        std::fs::create_dir_all(home.join("chain")).unwrap();
        std::os::unix::fs::symlink(home.join("chain/hop2"), home.join("chain/hop1")).unwrap();
        std::os::unix::fs::symlink(home.join("chain/hop1"), home.join(".psql_history")).unwrap();
        std::os::unix::fs::symlink("/", home.join(".azure")).unwrap();
        std::os::unix::fs::symlink("/dev/null", home.join(".kube")).unwrap();
        let f = secret_filters(&home);
        let (f, canon_home) = (f.unwrap(), canon(&home));
        std::fs::remove_dir_all(&home).unwrap();
        assert!(f.contains(&format!("(prefix \"{}\")", canon_home.join("gone/sqlite").display())), "{f}");
        assert!(f.contains(&format!("(prefix \"{}\")", canon_home.join("chain/hop2").display())), "{f}");
        assert!(f.contains(&format!("(subpath \"{}\")", canon_home.join("gone/docker").display())), "{f}");
        assert!(f.contains(&format!("(subpath \"{}\")", canon_home.join("dotfiles").display())), "{f}");
        assert!(!f.contains(&format!("(subpath \"{}\")", canon_home.display())) && !f.contains("(subpath \"/\")"), "{f}");
        for p in [".zsh_history", "dotfiles/zsh_history", ".bash_history"] {
            assert!(f.contains(&format!("(prefix \"{}\")", canon_home.join(p).display())), "{p}: {f}");
        }
        assert!(!f.contains("/dev/null"), "{f}");
    }

    #[test]
    fn the_docs_name_exactly_the_paths_a_plugin_cannot_read() {
        let docs = include_str!("../docs/plugins.md");
        let para = docs.split("**What a plugin can read.**").nth(1).unwrap().split(" A denied file").next().unwrap();
        let mut named: Vec<&str> = para.split('`').filter_map(|s| s.strip_prefix("~/")).collect();
        let mut secret = [SECRET, SECRET_FILES].concat();
        named.sort();
        secret.sort();
        assert_eq!(named, secret, "docs/plugins.md and SECRET disagree");
    }

    #[test]
    fn a_plugin_s_path_ends_with_the_folders_its_command_was_looked_for_in() {
        assert_eq!(plugin_path("/usr/bin:/bin"), "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin");
        assert_eq!(plugin_path("/a::/b"), "/a:/b:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin", "an empty entry is the current folder, and is dropped");
        assert_eq!(plugin_path(""), "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin");
    }

    #[test]
    fn a_pid_proves_a_plugin_only_while_the_broker_says_it_lives() {
        // A child's pid, not this process's: other tests in this process must never read as a plugin.
        let mut child = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let me = child.id() as i32;
        proofs().push(Proof { exec: 9_001, name: "proof-test".into(), pid: None });
        assert!(!proven("proof-test", me), "launched, not yet answered");
        started(9_001, me);
        assert!(proven("proof-test", me));
        assert!(within_a_plugin(me));
        assert!(!proven("other", me), "a pid proves its own plugin only");
        assert!(!proven("proof-test", std::process::id() as i32), "an ancestor of the plugin is not the plugin");
        exited(9_001);
        assert!(!proven("proof-test", me), "the same pid after the exit, as a reused one would be");
        assert!(!within_a_plugin(me));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn five_crashes_within_three_minutes_spend_the_budget() {
        let t = Instant::now();
        let mut times = Vec::new();
        for i in 0..4 {
            assert!(!over_budget(&mut times, t + Duration::from_secs(i * 10)), "crash {}", i + 1);
        }
        assert!(over_budget(&mut times, t + Duration::from_secs(40)), "the fifth");
        let mut slow = Vec::new();
        for i in 0..10 {
            assert!(!over_budget(&mut slow, t + Duration::from_secs(i * 50)), "one every 50 s never spends it");
        }
    }

    #[test]
    fn a_pane_that_moved_to_another_folder_is_a_pane_cwd() {
        let at = |cwd: &str| HashMap::from([(3, (PathBuf::from(cwd), None))]);
        assert_eq!(pane_events(&HashMap::new(), &at("/a")), [Event::PaneOpened { pane: 3, cwd: "/a".into() }]);
        assert_eq!(pane_events(&at("/a"), &at("/a")), []);
        assert_eq!(pane_events(&at("/a"), &at("/a/b")), [Event::PaneCwd { pane: 3, cwd: "/a/b".into() }]);
        assert_eq!(pane_events(&at("/a"), &HashMap::new()), [Event::PaneClosed { pane: 3 }]);
    }

    #[test]
    fn only_subscribed_events_go_out() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let c = Conn { id: 1, writer: crate::transport::Writer::spawn(ours, crate::bell::new().0).unwrap(), events: ["command".to_string()].into() };
        c.send(&Event::Connected { protocol: 2 });
        c.send(&Event::PaneClosed { pane: 3 });
        c.send(&Event::Command { id: "hi".into(), pane: Some(3), path: None });
        c.writer.retire();
        let mut got = String::new();
        std::io::Read::read_to_string(&mut &theirs, &mut got).unwrap();
        assert_eq!(got, "{\"event\":\"connected\",\"protocol\":2}\n{\"event\":\"command\",\"id\":\"hi\",\"pane\":3}\n");
    }

    #[test]
    fn a_report_is_taken_only_for_the_plugins_own_agent() {
        assert!(may_report("fake", Some("fake"), "working").is_ok());
        assert!(may_report("fake", Some("claude"), "working").is_err());
        assert!(may_report("fake", None, "idle").is_err());
        assert!(may_report("fake", Some("fake"), "rm -rf").is_err());
    }

    /// Runs `read_loop` over these lines, then EOF; returns what reached the loop, and when.
    fn read_lines(lines: Vec<String>) -> Vec<(Instant, Inbound)> {
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let inbox = Inbox { tx, bell: crate::bell::new().0 };
        let t = std::thread::spawn(move || read_loop("p", 7, BufReader::new(theirs), &inbox));
        for l in lines {
            ours.write_all(l.as_bytes()).unwrap();
            ours.write_all(b"\n").unwrap();
        }
        // Closed, not shut down: macOS can lose the wakeup of a reader blocked when its peer calls shutdown(Write).
        drop(ours);
        let mut got = Vec::new();
        let end = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(m) => got.push((Instant::now(), m)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(_) => panic!("read_loop did not end 60 s after the peer closed"),
            }
        }
        t.join().unwrap();
        got
    }

    #[test]
    fn a_screen_message_on_the_plugin_socket_does_nothing() {
        use crossterm::event::{Event as Ev, KeyCode, KeyEvent, KeyModifiers};
        let all = [
            ClientMsg::Hello {
                cwd: "/".into(),
                cols: 80,
                rows: 24,
                mux: false,
                screen: false,
                graphics: None,
                terminal: None,
                opened_by_nerd: false,
                opens_windows: false,
                dark: None,
                host: None,
                bye: false,
                connects: false,
                reattach: None,
                machine: None,
                cell_px: None,
                if_free: false,
                switches: false,
                reopened: false,
            },
            ClientMsg::Event(Ev::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))),
            ClientMsg::Appearance(true),
            ClientMsg::CellHeight(35),
            ClientMsg::Detach,
            ClientMsg::Shutdown,
            ClientMsg::Update,
            ClientMsg::DisconnectAll,
            ClientMsg::Probe,
            ClientMsg::Remote { name: "m".into(), result: Ok(vec![]), window: true, ssh_failed: false },
            ClientMsg::Connect { protocol: 2, role: "plugin".into(), plugin: Some("p".into()), events: vec![] },
            ClientMsg::ReportState { pane: 1, state: "idle".into() },
        ];
        let mut lines = Vec::new();
        for m in &all {
            // Every variant is here: a new one fails to compile until it is added above.
            match m {
                ClientMsg::Hello { .. }
                | ClientMsg::Event(_)
                | ClientMsg::Appearance(_)
                | ClientMsg::CellHeight(_)
                | ClientMsg::Detach
                | ClientMsg::Shutdown
                | ClientMsg::Update
                | ClientMsg::DisconnectAll
                | ClientMsg::Probe
                | ClientMsg::Remote { .. }
                | ClientMsg::Connect { .. }
                | ClientMsg::ReportState { .. } => lines.push(serde_json::to_string(m).unwrap()),
            }
        }
        let (got, log) = crate::session::tests::with_temp_state_dir("plugin-screen-msgs", || {
            let got = read_lines(lines);
            (got, std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default())
        });
        assert_eq!(log.matches("plugin p: unrecognized message").count(), 1, "eleven of them, one line: {log}");
        assert_eq!(got.len(), 1, "only ReportState, which a plugin may send, reached the loop");
        assert!(matches!(&got[0].1, Inbound::PluginSaid { name, id: 7, pane: 1, state } if name == "p" && state == "idle"));
    }

    #[test]
    fn a_flood_reaches_the_loop_as_the_latest_per_key_at_most_twenty_times_a_second() {
        let seg = |i: usize| serde_json::to_string(&PluginMsg::Segment { id: "a".into(), text: format!("n{i}"), role: String::new(), command: None }).unwrap();
        let badge = |i: usize| {
            let item = crate::proto::BadgeItem { path: Some(format!("/f{}", i % 7).into()), text: (i % 100).to_string(), ..Default::default() };
            serde_json::to_string(&PluginMsg::Badges { target: "tree".into(), items: vec![item] }).unwrap()
        };
        let over = serde_json::to_string(&PluginMsg::Segment { id: "b".into(), text: "x".repeat(25), role: String::new(), command: None }).unwrap();
        let mut lines: Vec<String> = (0..20_000).flat_map(|i| [seg(i), badge(i)]).collect();
        lines.insert(5, over.clone());
        lines.insert(9, over);
        lines.push(seg(99_999));
        let start = Instant::now();
        let (got, log) = crate::session::tests::with_temp_state_dir("plugin-flood", || {
            let got = read_lines(lines);
            (got, std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap_or_default())
        });
        let batches: Vec<&Vec<PluginMsg>> = got.iter().map(|(_, m)| match m {
            Inbound::PluginSlots { msgs, id: 7, .. } => msgs,
            _ => panic!("only slot batches"),
        }).collect();
        let span = got.last().unwrap().0.duration_since(start);
        assert!(batches.len() as u128 <= span.as_millis() / FORWARD.as_millis() + 1, "{} batches in {span:?}", batches.len());
        assert!(batches.iter().all(|b| b.len() <= 2), "one per key");
        assert!(batches.iter().flat_map(|b| b.iter()).all(|m| !matches!(m, PluginMsg::Segment { id, .. } if id == "b")), "over the cap: refused");
        let last = batches.last().unwrap();
        assert!(last.contains(&PluginMsg::Segment { id: "a".into(), text: "n99999".into(), role: String::new(), command: None }), "the final state is never dropped: {last:?}");
        assert_eq!(log.matches("plugin p: Segment refused: segment").count(), 1, "one WARN per plugin: {log}");
    }

    /// The words in backticks on the docs line that starts with `label`.
    fn documented(label: &str) -> Vec<String> {
        let docs = include_str!("../docs/plugins.md");
        let line = docs.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("no {label:?} line in docs/plugins.md"));
        line.split('`').skip(1).step_by(2).map(String::from).collect()
    }

    #[test]
    fn the_public_messages_and_events_are_the_documented_ones() {
        let msgs = [
            PluginMsg::ReportState { pane: 1, state: "idle".into() },
            PluginMsg::Badges { target: "tree".into(), items: vec![] },
            PluginMsg::Segment { id: "a".into(), text: "b".into(), role: String::new(), command: None },
            PluginMsg::SegmentClear { id: "a".into() },
            PluginMsg::Preview { req: 1, lines: vec![], cut: false, modes: vec![], mode: String::new() },
            PluginMsg::Panel { id: "p".into(), kind: "list".into(), title: String::new(), items: vec![], lines: vec![] },
        ];
        // Exhaustive: a new message fails to compile here until it is listed above, and then fails until documented.
        let names: Vec<String> = msgs
            .iter()
            .map(|m| match m {
                PluginMsg::ReportState { .. } | PluginMsg::Badges { .. } | PluginMsg::Segment { .. } | PluginMsg::SegmentClear { .. } | PluginMsg::Preview { .. } | PluginMsg::Panel { .. } => {
                    m.name().to_string()
                }
            })
            .collect();
        assert_eq!(names, documented("Public messages:"));
        for m in &msgs {
            assert!(serde_json::to_string(m).unwrap().starts_with(&format!("{{\"{}\"", m.name())), "a name is its JSON tag");
        }
        let evs = [
            Event::Connected { protocol: 2 },
            Event::AgentState { pane: 1, agent: "a".into(), state: "idle".into() },
            Event::PaneOpened { pane: 1, cwd: "/".into() },
            Event::PaneClosed { pane: 1 },
            Event::FileChosen { path: "/a".into() },
            Event::Command { id: "c".into(), pane: None, path: None },
            Event::PreviewRequest { req: 1, path: "/a".into(), cols: 1, rows: 1, mode: None },
            Event::PanelOpened { id: "p".into(), pane: None, path: None },
            Event::PanelItem { panel: "p".into(), item: "i".into() },
            Event::PaneCwd { pane: 1, cwd: "/".into() },
        ];
        let names: Vec<String> = evs
            .iter()
            .map(|e| match e {
                Event::Connected { .. }
                | Event::AgentState { .. }
                | Event::PaneOpened { .. }
                | Event::PaneClosed { .. }
                | Event::FileChosen { .. }
                | Event::Command { .. }
                | Event::PreviewRequest { .. }
                | Event::PanelOpened { .. }
                | Event::PanelItem { .. }
                | Event::PaneCwd { .. } => e.name().to_string(),
            })
            .collect();
        assert_eq!(names, documented("Public events:"));
        assert_eq!(&names[1..], crate::agents::EVENTS, "what a manifest may declare is every event but connected");
        let opened = |pane, path: Option<&str>| serde_json::to_string(&Event::PanelOpened { id: "p".into(), pane, path: path.map(Into::into) }).unwrap();
        assert_eq!(opened(None, None), r#"{"event":"panel_opened","id":"p"}"#, "from the global menu, as before");
        assert_eq!(opened(Some(3), None), r#"{"event":"panel_opened","id":"p","pane":3}"#);
        assert_eq!(opened(None, Some("/a")), r#"{"event":"panel_opened","id":"p","path":"/a"}"#);
    }

    #[test]
    fn slot_data_goes_when_its_plugin_stops_and_slot_plugins_start_again_after_reload() {
        let mut p = Plugins::default();
        let d = crate::slots::Decl { name: "p".into(), segments: vec!["a".into()], ..Default::default() };
        p.slots.ingest(&d, PluginMsg::Segment { id: "a".into(), text: "x".into(), role: String::new(), command: None }).unwrap();
        p.slots.apply();
        p.stop("p");
        assert_eq!(p.slots.segments().count(), 0, "stopped");
        p.slots.ingest(&d, PluginMsg::Segment { id: "a".into(), text: "x".into(), role: String::new(), command: None }).unwrap();
        p.slots.apply();
        p.crashed("p");
        assert_eq!(p.slots.segments().count(), 0, "crashed");
        p.autostart = true;
        p.reload();
        assert!(p.autostart, "Reload plugins and agents starts them again");
        p.stop_all();
        assert!(!p.autostart, "a stop for good does not");
    }

    #[test]
    fn one_source_previews_an_extension_the_built_in_first_then_plugins_by_name() {
        let m = |name: &str, exec: bool, exts: &[&str]| {
            let mut v = serde_json::json!({ "name": name, "title": name, "previews": [{ "extensions": exts }] });
            if exec {
                v["exec"] = serde_json::json!({ "command": "sh" });
            }
            serde_json::from_value::<Manifest>(v).unwrap()
        };
        let all = [m("zed", true, &["log", "csv"]), m("abc", true, &["log", "md"]), m("aaa", false, &["log"]), m("mid", true, &["txt"])];
        let owner = |ext: &str, builtins: bool| preview_owner(ext, builtins, &all);
        assert_eq!(owner("log", true), Some((true, "abc".into())), "by name, and only one with an exec");
        assert_eq!(owner("csv", true), Some((false, "previews".into())), "the built-in before any plugin");
        assert_eq!(owner("md", true), Some((false, "previews".into())));
        assert_eq!(owner("csv", false), Some((true, "zed".into())), "with the built-ins off, the plugin");
        assert_eq!(owner("txt", false), Some((true, "mid".into())));
        assert_eq!(owner("rs", true), None);
    }

    #[test]
    fn the_prompt_names_a_manifest_address_of_this_mac_or_its_network_as_such() {
        let hosts: Vec<String> = ["wttr.in", "8.8.8.8", "192.168.1.5", "2a02:8308:30f:2200::fcd7", "2a02:8308:30f:2201::1"].map(String::from).to_vec();
        let nets = [("2a02:8308:30f:2200::1".parse().unwrap(), "ffff:ffff:ffff:ffff::".parse().unwrap())];
        assert_eq!(super::near(&hosts, Some(&nets)), ["192.168.1.5", "2a02:8308:30f:2200::fcd7"], "a global IPv6 on this Mac's own network is near too");
        assert_eq!(super::near(&hosts, None), ["8.8.8.8", "192.168.1.5", "2a02:8308:30f:2200::fcd7", "2a02:8308:30f:2201::1"], "unknown networks: every address is named");
    }

    #[test]
    fn a_write_path_never_reaches_what_runs_outside_the_sandbox() {
        let home = Path::new("/Users/me");
        let path = "/opt/homebrew/bin:/Users/me/bin:/usr/bin:";
        for p in [
            "/opt/homebrew/bin",
            "/opt/homebrew",
            "/opt",
            "/Users/me/bin",
            "/Users/me/BIN/x",
            "/Users/me",
            "/Users/me/.local/x",
            "/Users/me/Library/Preferences",
            "/usr/local",
            "/Users/me/p/.git/hooks",
            "/Users/me/P/.GIT",
            "/Users/me/p/.envrc",
            "/Users/me/p/sub/.claude/x",
            "/Users/me/Application\u{17f}/x",
        ] {
            assert!(runs_later(Path::new(p), home, path).is_some(), "{p}");
        }
        for home in ["", "/"] {
            assert!(runs_later(Path::new("/opt/homebrew/share/x"), Path::new(home), "").is_some(), "no home: {home:?}");
        }
        for p in ["/Users/me/notes", "/Users/me/projects/out"] {
            assert_eq!(runs_later(Path::new(p), home, path), None, "{p}");
        }
        assert_eq!(regex_escape("/a.b/c(d)"), "/a\\.b/c\\(d\\)");
    }
}
