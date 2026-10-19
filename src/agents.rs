//! Agents as data: each one is a `plugin.json` manifest, built in or dropped into `<state root>/plugins/<name>/`.
//! A manifest says how nerd finds the agent, hooks into its config and resumes its session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub title: String,
    /// Absent for a plugin that is only an executable.
    #[serde(default)]
    pub agent: Agent,
    #[serde(default)]
    pub exec: Option<Exec>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub min_nerd: Option<String>,
    /// `x.y.z` with an optional pre-release; a git install needs it.
    #[serde(default)]
    pub version: Option<String>,
    /// Where it runs; none listed means macOS only.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub commands: Vec<Command>,
    /// The events this plugin may be sent; what it subscribes to on `Connect` is cut down to these.
    #[serde(default)]
    pub events: Vec<String>,
    /// Where it puts badges: `tree`, `tab`.
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default)]
    pub segments: Vec<SegmentDecl>,
    #[serde(default)]
    pub previews: Vec<PreviewDecl>,
    #[serde(default)]
    pub panels: Vec<PanelDecl>,
    #[serde(skip)]
    pub builtin: bool,
    /// The folder the manifest was read from, canonical; a user's only.
    #[serde(skip)]
    pub dir: Option<PathBuf>,
    /// Built from the validated manifest when it was loaded; the shim applies this text and nothing else.
    #[serde(skip)]
    pub profile: String,
    /// `exec.command` resolved when the manifest was loaded; the profile lets exactly this run.
    #[serde(skip)]
    pub program: PathBuf,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Exec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Permissions {
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub write: Vec<String>,
}

/// How far a plugin reaches: nowhere, this Mac's own services, or the internet on named ports (docs/privacy-and-security.md, "What leaves nerd").
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Network {
    #[default]
    None,
    Localhost,
    /// Enforced by the plugin's proxy; both sorted and deduplicated, so a reordered list asks nothing new.
    Internet { hosts: Vec<String>, ports: Vec<u16> },
}

/// A plugin's or a user's manifest; a built-in agent may name every host of its providers (pi has dozens).
const HOSTS: usize = 16;
const BUILTIN_HOSTS: usize = 64;

const NETWORK_FORMS: &str = r#"permissions.network is "localhost" or {"hosts": [...], "ports": [...]}"#;

impl<'de> Deserialize<'de> for Network {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        network(&serde_json::Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

fn network(v: &serde_json::Value) -> Result<Network, String> {
    use serde_json::Value;
    match v {
        Value::Null | Value::Bool(false) => Ok(Network::None),
        Value::String(s) if s == "localhost" => Ok(Network::Localhost),
        Value::Bool(true) => Err(format!("{NETWORK_FORMS}; true is gone: name the hosts and the ports it talks to")),
        Value::Object(o) => {
            if let Some(k) = o.keys().find(|k| *k != "hosts" && *k != "ports") {
                return Err(format!("{NETWORK_FORMS}; {k:?} is not one of its keys"));
            }
            let list = |k: &str| o.get(k).and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| format!("{NETWORK_FORMS}; {k} must be a list with something in it"));
            let mut hosts = list("hosts")?.iter().map(|h| h.as_str().and_then(host).ok_or_else(|| format!("host {h} must be a lowercase domain name such as example.com, or an IP address such as 192.168.1.5"))).collect::<Result<Vec<_>, _>>()?;
            let mut ports = list("ports")?.iter().map(|p| p.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0).ok_or_else(|| format!("port {p} must be a number from 1 to 65535"))).collect::<Result<Vec<_>, _>>()?;
            hosts.sort();
            hosts.dedup();
            ports.sort_unstable();
            ports.dedup();
            if hosts.len() > BUILTIN_HOSTS || ports.len() > 8 {
                return Err(format!("{NETWORK_FORMS}; at most {HOSTS} hosts and 8 ports"));
            }
            Ok(Network::Internet { hosts, ports })
        }
        _ => Err(NETWORK_FORMS.into()),
    }
}

/// A destination IP address in standard form, or a lowercase name whose last label starts with a letter (`127.0x1` is IPv4 to the resolver).
fn host(h: &str) -> Option<String> {
    if let Ok(a) = h.parse::<std::net::IpAddr>().map(|a| a.to_canonical()) {
        let broadcast = matches!(a, std::net::IpAddr::V4(v) if v.is_broadcast());
        return (!a.is_unspecified() && !a.is_multicast() && !broadcast).then(|| a.to_string());
    }
    let labels: Vec<&str> = h.split('.').collect();
    let name = h.len() <= 253 && labels.len() >= 2 && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
    (name && labels.last()?.starts_with(|c: char| c.is_ascii_lowercase())).then(|| h.to_string())
}

/// An agent's hosts: one list, or (built-ins only) named provider groups a pane may choose from, `net` being their union.
/// ceiling: ports are the agent's, so a provider on other ports cannot be a group; a port list per group would lift it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentNetwork {
    pub net: Network,
    /// Sorted by name; empty when the manifest names `hosts`.
    pub providers: Vec<(String, Vec<String>)>,
}

const GROUPS: usize = 64;
const AGENT_NETWORK_FORMS: &str = r#"agent.network is {"hosts": [...], "ports": [...]} (built-in agents may group their hosts as {"providers": {"name": [hosts], ...}, "ports": [...]})"#;

impl<'de> Deserialize<'de> for AgentNetwork {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        agent_network(serde_json::Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

fn agent_network(mut v: serde_json::Value) -> Result<AgentNetwork, String> {
    use serde_json::Value;
    let Some(groups) = v.as_object_mut().and_then(|o| o.remove("providers")) else {
        return Ok(AgentNetwork { net: network(&v)?, providers: Vec::new() });
    };
    let Value::Object(groups) = groups else { return Err(format!("{AGENT_NETWORK_FORMS}; providers must be an object")) };
    if v.get("hosts").is_some() {
        return Err(format!("{AGENT_NETWORK_FORMS}; hosts or providers, not both"));
    }
    if groups.is_empty() || groups.len() > GROUPS {
        return Err(format!("{AGENT_NETWORK_FORMS}; 1 to {GROUPS} providers"));
    }
    let mut providers = Vec::new();
    for (name, hosts) in groups {
        if !token(&name) {
            return Err(format!("provider {name:?} must be lowercase letters, digits and dashes"));
        }
        let list = hosts.as_array().filter(|a| !a.is_empty()).ok_or_else(|| format!("provider {name:?} must list its hosts"))?;
        let mut hosts = list.iter().map(|h| h.as_str().and_then(host).ok_or_else(|| format!("provider {name:?}: host {h} must be a lowercase domain name or an IP address"))).collect::<Result<Vec<_>, _>>()?;
        hosts.sort();
        hosts.dedup();
        providers.push((name, hosts));
    }
    providers.sort();
    let union: Vec<&String> = providers.iter().flat_map(|(_, h)| h).collect();
    v["hosts"] = serde_json::json!(union);
    Ok(AgentNetwork { net: network(&v)?, providers })
}

impl Network {
    /// Whether everything this reaches, `wider` reaches too.
    pub fn within(&self, wider: &Network) -> bool {
        match (self, wider) {
            (Network::None, _) | (Network::Localhost, Network::Localhost) => true,
            (Network::Internet { hosts, ports }, Network::Internet { hosts: h, ports: p }) => ports.iter().all(|x| p.contains(x)) && hosts.iter().all(|x| h.contains(x)),
            _ => false,
        }
    }

    /// How the log says it.
    pub fn said(&self) -> String {
        match self {
            Network::None => "none".into(),
            Network::Localhost => "localhost".into(),
            Network::Internet { hosts, ports } => format!("only {} on {} {}, through its proxy", hosts.join(", "), if ports.len() == 1 { "port" } else { "ports" }, join(ports)),
        }
    }
}

pub fn join(ports: &[u16]) -> String {
    ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

#[derive(Deserialize, Clone, Debug)]
pub struct Command {
    pub id: String,
    pub title: String,
    #[serde(rename = "where")]
    pub place: Place,
}

#[derive(Deserialize, Clone, Debug)]
pub struct SegmentDecl {
    pub id: String,
}

/// File extensions a plugin previews: lowercase, without the dot.
#[derive(Deserialize, Clone, Debug)]
pub struct PreviewDecl {
    pub extensions: Vec<String>,
}

pub const PREVIEW_EXTENSIONS: usize = 16;

/// A panel, opened from the menu at `where`.
#[derive(Deserialize, Clone, Debug)]
pub struct PanelDecl {
    pub id: String,
    pub title: String,
    #[serde(rename = "where")]
    pub place: Place,
}

/// Which menu a command sits in.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Global,
    Pane,
    /// A tree menu, on a file and on a folder.
    Tree,
    /// A tree menu on a file only.
    File,
    /// A tree menu on a folder only.
    Folder,
}

impl Place {
    /// Whether an item declared here belongs in the menu `at`: Global, Pane, File or Folder.
    pub fn fits(self, at: Place) -> bool {
        self == at || (self == Place::Tree && matches!(at, Place::File | Place::Folder))
    }
}

/// Events a plugin can be sent.
pub const EVENTS: &[&str] = &["agent_state", "pane_opened", "pane_closed", "file_chosen", "command", "preview_request", "panel_opened", "panel_item", "pane_cwd"];

/// Launch services, Apple events and launchd: a plugin that ran one would act outside its sandbox.
const NEVER_EXEC: &[&str] = &["open", "osascript", "launchctl"];

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Agent {
    /// Binaries and aliases; the first is the program started when there is no session to resume.
    pub names: Vec<String>,
    #[serde(default)]
    pub config: Option<ConfigSpec>,
    /// The hook script's second argument; empty is Claude Code.
    #[serde(default)]
    pub arg: String,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub session: Option<Session>,
    /// Fixed words and one `{id}`, alone or after `=`.
    #[serde(default)]
    pub resume: Vec<String>,
    #[serde(default)]
    pub strip: Strip,
    /// Screen text that means the agent asks the human, for panes whose hook says nothing.
    #[serde(default)]
    pub blocked_text: Vec<String>,
    /// How a message from nerd reaches it.
    #[serde(default)]
    pub mail: Option<Mail>,
    /// Paths starting `~/` it writes, its session folders: writable in a "this folder only" pane.
    #[serde(default)]
    pub data: Vec<String>,
    /// Its provider's hosts and ports, for a pane set to "only these providers".
    #[serde(default)]
    pub network: AgentNetwork,
    /// One sentence: how to turn the agent's own sandbox off.
    #[serde(default)]
    pub own_sandbox: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Mail {
    pub via: MailVia,
    #[serde(default)]
    pub form: Option<MailForm>,
    /// The variable that tells the agent where its inbox is.
    #[serde(default)]
    pub inbox_env: Option<String>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MailVia {
    Inbox,
    Hook,
    Extension,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MailForm {
    Block,
    Deny,
    Followup,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ConfigSpec {
    pub path: String,
    pub shape: Shape,
}

/// How nerd's entries sit in an agent's config.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// `hooks: {Event: [{matcher?, hooks: [{type, command}]}]}`, merged into the agent's file.
    Nested,
    /// Cursor: `version: 1, hooks: {event: [{command}]}`, merged.
    Flat,
    /// Copilot: a file of nerd's own, `version: 1, hooks: {event: [{type, bash}]}`.
    Own,
    /// OpenCode: a plugin file.
    Plugin,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Event {
    pub on: String,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(rename = "do")]
    pub word: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Session {
    #[serde(default = "session_id")]
    pub field: String,
    #[serde(default)]
    pub kind: IdKind,
}

fn session_id() -> String {
    "session_id".into()
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IdKind {
    #[default]
    Uuid,
    Word,
}

/// Flags dropped from a stored command line so it does not take a session another copy holds.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Strip {
    /// Dropped with the word after them, or with their `=value`.
    #[serde(default)]
    pub value: Vec<String>,
    #[serde(default)]
    pub flag: Vec<String>,
}

/// The words the hook script acts on; anything else it ignores.
pub const WORDS: &[&str] = &["working", "idle", "blocked", "fail", "stop", "notify", "task", "sub-start", "sub-stop", "sub-reset", "tool-start", "tool-ok", "tool-fail", "mail"];

const BUILTIN: &[&str] = &[
    include_str!("../plugins/claude/plugin.json"),
    include_str!("../plugins/codex/plugin.json"),
    include_str!("../plugins/qwen/plugin.json"),
    include_str!("../plugins/gemini/plugin.json"),
    include_str!("../plugins/cursor/plugin.json"),
    include_str!("../plugins/copilot/plugin.json"),
    include_str!("../plugins/opencode/plugin.json"),
    include_str!("../plugins/pi/plugin.json"),
];

pub fn builtins() -> Vec<Manifest> {
    let mut out: Vec<Manifest> = Vec::new();
    for text in BUILTIN {
        let mut m: Manifest = serde_json::from_str(text).expect("a built-in manifest parses");
        m.builtin = true;
        validate(&m, &out).expect("a built-in manifest is valid");
        out.push(m);
    }
    out
}

static LOADED: RwLock<Option<Arc<Vec<Manifest>>>> = RwLock::new(None);

/// Built-ins first, then the user's; read once, then again on `reload`.
pub fn all() -> Arc<Vec<Manifest>> {
    if let Some(ms) = LOADED.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return ms.clone();
    }
    reload()
}

pub fn reload() -> Arc<Vec<Manifest>> {
    reload_admitting(&|_| true)
}

/// Reads the manifests again, loading only the folders `admit` names.
pub fn reload_admitting(admit: &dyn Fn(&str) -> bool) -> Arc<Vec<Manifest>> {
    let root = crate::paths::state_root();
    let ms = Arc::new(load_admitting(&root.join("plugins"), admit));
    let path = root.join(crate::hook::SESSIONS_FILE);
    let text = crate::hook::sessions(ms.iter().filter(|m| !m.builtin));
    if std::fs::read_to_string(&path).ok().unwrap_or_default() != text && crate::paths::write_private(&path, "tmp", text.as_bytes()).is_err() {
        crate::log::warn(&format!("agent sessions not written to {}", path.display()));
    }
    *LOADED.write().unwrap_or_else(|e| e.into_inner()) = Some(ms.clone());
    ms
}

/// Screen text that means `agent` asks the human: its manifest's, else the prompts Claude Code shows,
/// which agents without a manifest of their own are read by as before; none without an agent.
pub fn blocked_text(agent: Option<&str>) -> Vec<String> {
    let Some(agent) = agent else { return Vec::new() };
    get(agent).map(|m| m.agent.blocked_text).filter(|t| !t.is_empty()).unwrap_or_else(|| BLOCKED_TEXT.iter().map(|s| s.to_string()).collect())
}

const BLOCKED_TEXT: &[&str] = &["Do you want", "❯ 1."];

pub fn get(name: &str) -> Option<Manifest> {
    all().iter().find(|m| m.name == name).cloned()
}

/// The built-ins and every valid manifest under `dir`; a bad one is logged and skipped.
#[cfg(test)]
pub fn load(dir: &Path) -> Vec<Manifest> {
    load_admitting(dir, &|_| true)
}

/// As `load`, but only the folders `admit` names; the ones turned off in `plugins-off.json` next to `dir` never,
/// and none at all while that file does not parse.
pub fn load_admitting(dir: &Path, admit: &dyn Fn(&str) -> bool) -> Vec<Manifest> {
    let mut ms = builtins();
    let off = match dir.parent().map(|r| crate::install::read_off_at(&r.join(crate::install::OFF))).unwrap_or(Ok(Default::default())) {
        Ok(off) => off,
        Err(e) => {
            crate::log::error(&format!("no plugin loaded: {e}"));
            return ms;
        }
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path().join("plugin.json")).filter(|p| p.is_file()).collect()).unwrap_or_default();
    paths.sort();
    for path in paths {
        let folder = path.parent().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(why) = off.get(&folder) {
            crate::log::info(&format!("plugin {folder} is off: {why}"));
            continue;
        }
        if !admit(&folder) {
            crate::log::info(&format!("plugin {folder} waits for Reload plugins"));
            continue;
        }
        match read_user(&path, &ms) {
            Ok(m) => {
                crate::log::info(&format!("agent manifest loaded: {} from {}", m.name, path.display()));
                ms.push(m);
            }
            Err(e) => crate::log::warn(&format!("agent manifest skipped: {}: {e}", path.display())),
        }
    }
    ms
}

fn read_user(path: &Path, loaded: &[Manifest]) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut m: Manifest = serde_json::from_str(&text).map_err(|e| format!("does not parse: {e}"))?;
    let folder = path.parent().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if folder != m.name {
        return Err(format!("its folder is {folder:?}, its name {:?}", m.name));
    }
    m.dir = path.parent().and_then(|d| d.canonicalize().ok());
    prepare(m, loaded)
}

/// Validated, runnable here, its program resolved and its profile built.
fn prepare(mut m: Manifest, loaded: &[Manifest]) -> Result<Manifest, String> {
    validate(&m, loaded)?;
    if !m.runs_here() {
        return Err(format!("runs on {}, not macOS", m.platforms.join(", ")));
    }
    if m.exec.is_some() {
        m.program = resolve_exec(&m)?;
        m.profile = crate::plugins::profile(&m)?;
    }
    Ok(m)
}

/// What `--plugin-check` and an install report about a plugin folder whose name need not be the manifest's.
#[derive(serde::Serialize, Debug, Default)]
pub struct Check {
    pub name: String,
    pub title: String,
    pub version: Option<String>,
    pub platforms: Vec<String>,
    pub risk: u8,
    pub abilities: Vec<String>,
    pub problems: Vec<String>,
    #[serde(skip)]
    pub manifest: Option<Manifest>,
    #[serde(skip)]
    pub found: std::collections::BTreeSet<crate::plugins::Ability>,
}

/// Largest file a plugin folder may hold.
pub const FILE_MAX: u64 = 10 << 20;

/// The loader's rules against the built-ins and the plugins already loaded (except one of the same name, which
/// this would replace), plus rules for the folder itself; `catalog` adds what the catalog requires.
pub fn check_folder(dir: &Path, catalog: bool) -> Check {
    check(dir, catalog, true)
}

/// `check_folder` without the walk over its files, for a folder an install already walked.
pub fn check_installed(dir: &Path) -> Check {
    check(dir, false, false)
}

fn check(dir: &Path, catalog: bool, walk: bool) -> Check {
    let mut c = Check::default();
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            c.problems.push(format!("{}: {e}", dir.display()));
            return c;
        }
    };
    let parsed = std::fs::read_to_string(dir.join("plugin.json")).map_err(|e| format!("plugin.json: {e}")).and_then(|t| serde_json::from_str::<Manifest>(&t).map_err(|e| format!("plugin.json does not parse: {e}")));
    match parsed {
        Ok(mut m) => {
            (c.name, c.title, c.version, c.platforms) = (m.name.clone(), m.title.clone(), m.version.clone(), m.platforms.clone());
            m.dir = Some(dir.clone());
            let others: Vec<Manifest> = all().iter().filter(|o| o.builtin || o.name != m.name).cloned().collect();
            match prepare(m, &others) {
                Ok(m) => {
                    match crate::plugins::abilities(&m) {
                        Ok(a) => {
                            c.risk = crate::plugins::risk(&a);
                            c.abilities = a.iter().map(|x| x.to_string()).collect();
                            c.found = a;
                        }
                        Err(e) => c.problems.push(e),
                    }
                    c.manifest = Some(m);
                }
                Err(e) => c.problems.push(e),
            }
        }
        Err(e) => c.problems.push(e),
    }
    if catalog {
        if c.version.is_none() {
            c.problems.push("the catalog needs a version".into());
        }
        if c.manifest.as_ref().is_some_and(|m| m.min_nerd.is_none()) {
            c.problems.push("the catalog needs min_nerd".into());
        }
        let licence = std::fs::read_dir(&dir).map(|rd| rd.flatten().any(|e| { let n = e.file_name().to_string_lossy().to_uppercase(); n.starts_with("LICENSE") || n.starts_with("LICENCE") || n.starts_with("COPYING") })).unwrap_or(false);
        if !licence {
            c.problems.push("the catalog needs a LICENSE file".into());
        }
    }
    if walk {
        folder_rules(&dir, &dir, &mut c.problems);
    }
    c
}

/// No link out of the folder, no file over `FILE_MAX`; `.git` is git's, not the plugin's.
fn folder_rules(root: &Path, dir: &Path, problems: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_symlink() {
            if !p.canonicalize().is_ok_and(|t| t.starts_with(root)) {
                problems.push(format!("{rel} links outside the plugin's folder"));
            }
        } else if md.is_dir() {
            if e.file_name() != ".git" {
                folder_rules(root, &p, problems);
            }
        } else if md.len() > FILE_MAX {
            problems.push(format!("{rel} is over {} MB", FILE_MAX >> 20));
        }
    }
}

pub fn token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}

/// Every rule a manifest must pass before nerd writes anything it says; `others` are the ones already accepted.
pub fn validate(m: &Manifest, others: &[Manifest]) -> Result<(), String> {
    let a = &m.agent;
    if a.names.is_empty() && m.exec.is_none() {
        return Err("a manifest needs an agent or an exec".into());
    }
    if !token(&m.name) || m.name.starts_with('-') || !(a.names.contains(&m.name) || a.names.is_empty()) {
        return Err(format!("name {:?} must be lowercase letters, digits and dashes, and one of its names", m.name));
    }
    if !text(&m.title) {
        return Err("the title must be one line".into());
    }
    if crate::slots::BUILTINS.contains(&m.name.as_str()) {
        return Err(format!("{} is a built-in's name", m.name));
    }
    let many = |n: &Network| matches!(n, Network::Internet { hosts, .. } if hosts.len() > HOSTS);
    if !m.builtin && (many(&m.permissions.network) || many(&a.network.net)) {
        return Err(format!("{NETWORK_FORMS}; at most {HOSTS} hosts and 8 ports"));
    }
    // a plugin that regrouped its hosts would change what a saved pane reaches without asking
    if !m.builtin && !a.network.providers.is_empty() {
        return Err("agent.network.providers is for built-in agents only: name the hosts".into());
    }
    if let Some(o) = others.iter().find(|o| o.name == m.name) {
        return Err(format!("{} is already taken by {}", m.name, if o.builtin { "a built-in" } else { "another plugin" }));
    }
    validate_plugin(m)?;
    if let Some(n) = a.names.iter().find(|n| !token(n)) {
        return Err(format!("name {n:?} must be lowercase letters, digits and dashes"));
    }
    // a launcher or nerd itself would make every shell, script or pane "the agent"
    if let Some(n) = a.names.iter().find(|n| *n == "nerd" || crate::detect::LAUNCHERS.contains(&n.as_str())) {
        return Err(format!("{n} runs other programs, it is not an agent"));
    }
    // a file of nerd's own replaces what is there: only built-ins may, a user's manifest merges into a JSON file
    if !m.builtin && a.config.as_ref().is_some_and(|c| matches!(c.shape, Shape::Own | Shape::Plugin)) {
        return Err("a user's manifest may use the nested or flat shape only".into());
    }
    if let Some(c) = &a.config {
        config_dir(&c.path, &a.names).ok_or_else(|| format!("config path {:?} must be under ~/.<name> or ~/.config/<name> for one of its names", c.path))?;
        if let Some(o) = others.iter().find(|o| o.agent.config.as_ref().is_some_and(|oc| oc.path == c.path)) {
            return Err(format!("config path {} is {}'s", c.path, o.name));
        }
        if c.shape != Shape::Plugin && let Some(o) = others.iter().find(|o| o.agent.config.as_ref().is_some_and(|oc| oc.shape != Shape::Plugin) && o.from() == m.from()) {
            return Err(format!("arg {:?} is {}'s", m.from(), o.name));
        }
    }
    for o in others {
        if let Some(n) = a.names.iter().find(|n| o.agent.names.contains(n) || **n == o.name) {
            return Err(if o.builtin { format!("{n} is a built-in agent and cannot be replaced") } else { format!("{n} is already taken by {}", o.name) });
        }
    }
    if !a.arg.is_empty() && !token(&a.arg) {
        return Err(format!("arg {:?} must be lowercase letters, digits and dashes", a.arg));
    }
    for e in &a.events {
        if !WORDS.contains(&e.word.as_str()) {
            return Err(format!("{:?} is not a hook word; the words are {}", e.word, WORDS.join(", ")));
        }
        if !text(&e.on) || e.matcher.as_deref().is_some_and(|x| !text(x)) {
            return Err(format!("event {:?} and its matcher must be one line", e.on));
        }
    }
    if let Some(s) = &a.session
        && (s.field.is_empty() || s.field.len() > 64 || !s.field.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    {
        return Err(format!("session field {:?} must be letters, digits and underscores", s.field));
    }
    if !a.resume.is_empty() {
        if a.session.is_none() {
            return Err("a resume line needs a session".into());
        }
        if !a.names.contains(&a.resume[0]) {
            return Err(format!("resume starts {:?}, which is not one of its names", a.resume[0]));
        }
        if a.resume.iter().filter(|w| w.contains("{id}")).count() != 1 {
            return Err("resume must hold {id} exactly once".into());
        }
        for w in &a.resume[1..] {
            let fixed = match w.strip_suffix("{id}") {
                Some("") => continue,
                Some(flag) => flag.strip_suffix('=').unwrap_or("{"),
                None => w,
            };
            if !text(fixed) || fixed.contains(['{', '}']) {
                return Err(format!("resume word {w:?}: fixed words, and {{id}} alone or after ="));
            }
        }
    }
    if let Some(bad) = a.strip.value.iter().chain(&a.strip.flag).chain(&a.blocked_text).find(|s| !text(s)) {
        return Err(format!("{bad:?} must be one line"));
    }
    validate_mail(m)
}

/// The fields agent messaging and pane isolation read.
fn validate_mail(m: &Manifest) -> Result<(), String> {
    let a = &m.agent;
    if let Some(mail) = &a.mail {
        if matches!(mail.via, MailVia::Inbox | MailVia::Hook) && mail.form.is_none() {
            return Err("mail via inbox or hook needs a form".into());
        }
        let env = |e: &str| e.len() <= 64 && e.starts_with(|c: char| c.is_ascii_uppercase()) && e.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        match (mail.via, &mail.inbox_env) {
            (MailVia::Inbox, Some(e)) if env(e) => {}
            (MailVia::Inbox, _) => return Err("mail via inbox needs inbox_env, an uppercase variable name".into()),
            (_, Some(_)) => return Err("inbox_env is for mail via inbox only".into()),
            _ => {}
        }
    }
    if a.own_sandbox.as_deref().is_some_and(|s| !text(s)) {
        return Err("own_sandbox must be one line".into());
    }
    for d in &a.data {
        let rest = d.strip_prefix("~/").filter(|r| !r.is_empty()).ok_or_else(|| format!("data {d:?} must start with ~/ and name something under it"))?;
        if rest.split('/').any(|p| p == "..") || d.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
            return Err(format!("data {d:?} may hold no .., no quote, no backslash and no control character"));
        }
        if Path::new(rest).starts_with("Library/Application Support/nerd") {
            return Err(format!("data {d:?} is nerd's own folder"));
        }
        // Every agent's data paths are writable in every "this folder only" pane.
        if !m.builtin && config_dir(d, &a.names).is_none() {
            return Err(format!("data {d:?} must be under ~/.<name> or ~/.config/<name> for one of its names"));
        }
        if data_root(d).is_none() {
            return Err(format!("data {d:?} must be a session folder inside the agent's own folder, never the folder itself"));
        }
        let parts: Vec<&str> = rest.split('/').collect();
        let last = parts.last().copied().unwrap_or_default();
        if parts[..parts.len() - 1].iter().any(|p| p.contains('*')) || (last.contains('*') && last.trim_matches('*').is_empty()) {
            return Err(format!("data {d:?}: a * only in the last name, beside other letters"));
        }
        if let Some(why) = runs_code(&parts) {
            return Err(format!("data {d:?} {why}: an isolated pane could change what the agent runs"));
        }
    }
    if a.network.net == Network::Localhost {
        return Err("an agent's network names its hosts and ports".into());
    }
    Ok(())
}

/// The executable half: nothing here reaches a shell, and the sandbox profile is built from it.
fn validate_plugin(m: &Manifest) -> Result<(), String> {
    if let Some(v) = &m.version
        && semver(v).is_none()
    {
        return Err(format!("version {v:?} must be x.y.z, optionally with a -pre-release"));
    }
    for (i, p) in m.platforms.iter().enumerate() {
        if !PLATFORMS.contains(&p.as_str()) || m.platforms[..i].contains(p) {
            return Err(format!("platform {p:?}: one of {}, each once", PLATFORMS.join(", ")));
        }
    }
    if let Some(v) = &m.min_nerd {
        let (want, have) = (version(v).ok_or_else(|| format!("min_nerd {v:?} must be x.y.z"))?, version(env!("CARGO_PKG_VERSION")).unwrap_or_default());
        if want > have {
            return Err(format!("needs nerd {v}, this is {}", env!("CARGO_PKG_VERSION")));
        }
    }
    let Some(exec) = &m.exec else {
        if !m.commands.is_empty()
            || !m.events.is_empty()
            || !m.badges.is_empty()
            || !m.segments.is_empty()
            || !m.previews.is_empty()
            || !m.panels.is_empty()
            || m.permissions.network != Network::None
            || !m.permissions.write.is_empty()
        {
            return Err("commands, events, badges, segments, previews, panels and permissions need an exec".into());
        }
        return Ok(());
    };
    if m.builtin {
        return Err("a built-in has no executable".into());
    }
    if let Some(bad) = std::iter::once(&exec.command).chain(&exec.args).find(|s| !text(s)) {
        return Err(format!("exec word {bad:?} must be one line"));
    }
    if let Some(e) = m.events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(format!("{e:?} is not an event; the events are {}", EVENTS.join(", ")));
    }
    if m.commands.len() > 16 {
        return Err("at most 16 commands".into());
    }
    for c in &m.commands {
        if !token(&c.id) || !text(&c.title) || c.title.chars().count() > 40 {
            return Err(format!("command {:?}: an id of lowercase letters, digits and dashes, a title of one short line", c.id));
        }
    }
    if !m.commands.is_empty() && !m.events.iter().any(|e| e == "command") {
        return Err("commands need the command event".into());
    }
    for (i, b) in m.badges.iter().enumerate() {
        if !["tree", "tab"].contains(&b.as_str()) || m.badges[..i].contains(b) {
            return Err(format!("badges {b:?}: tree or tab, each once"));
        }
    }
    if m.segments.len() > crate::slots::SEGMENTS {
        return Err(format!("at most {} segments", crate::slots::SEGMENTS));
    }
    for (i, s) in m.segments.iter().enumerate() {
        if !token(&s.id) || m.segments[..i].iter().any(|o| o.id == s.id) {
            return Err(format!("segment {:?}: an id of lowercase letters, digits and dashes, each once", s.id));
        }
    }
    let exts: Vec<&String> = m.previews.iter().flat_map(|p| &p.extensions).collect();
    if exts.len() > PREVIEW_EXTENSIONS {
        return Err(format!("at most {PREVIEW_EXTENSIONS} preview extensions"));
    }
    for (i, e) in exts.iter().enumerate() {
        if e.is_empty() || e.len() > 16 || !e.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') || exts[..i].contains(e) {
            return Err(format!("preview extension {e:?}: lowercase letters and digits without the dot, each once"));
        }
    }
    if !exts.is_empty() && !m.events.iter().any(|e| e == "preview_request") {
        return Err("previews need the preview_request event".into());
    }
    if m.panels.len() > crate::slots::PANELS {
        return Err(format!("at most {} panels", crate::slots::PANELS));
    }
    for (i, p) in m.panels.iter().enumerate() {
        if !token(&p.id) || m.panels[..i].iter().any(|o| o.id == p.id) || !text(&p.title) || p.title.chars().count() > 40 {
            return Err(format!("panel {:?}: an id of lowercase letters, digits and dashes, each once, and a title of one short line", p.id));
        }
    }
    Ok(())
}

pub const PLATFORMS: &[&str] = &["macos", "linux", "windows"];

/// The numbers, then the pre-release if any.
pub type Semver<'a> = ((u32, u32, u32), Option<&'a str>);

/// Semver's `x.y.z[-pre]`, numbers without leading zeros; build metadata (`+…`) is refused.
pub fn semver(v: &str) -> Option<Semver<'_>> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    if pre.is_some_and(|p| p.is_empty() || p.split('.').any(|x| x.is_empty() || !x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))) {
        return None;
    }
    if core.split('.').any(|x| x.is_empty() || (x.len() > 1 && x.starts_with('0'))) {
        return None;
    }
    Some((version(core)?, pre))
}

/// Whether this nerd is `min` or newer.
pub fn nerd_meets(min: &str) -> bool {
    version(min).is_some_and(|m| m <= version(env!("CARGO_PKG_VERSION")).unwrap_or_default())
}

/// `x.y.z` as numbers, so 0.10.0 is newer than 0.9.0.
fn version(v: &str) -> Option<(u32, u32, u32)> {
    let mut p = v.split('.').map(|x| x.parse::<u32>().ok());
    let r = (p.next()??, p.next()??, p.next()??);
    p.next().is_none().then_some(r)
}

/// Where a bare command name is looked for after PATH; a plugin's own PATH ends with the same folders.
pub const PATH_FALLBACK: [&str; 4] = ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"];

/// The program a plugin runs, as a canonical path: inside its folder, or an absolute or PATH-found binary.
pub fn resolve_exec(m: &Manifest) -> Result<PathBuf, String> {
    let (Some(exec), Some(dir)) = (&m.exec, &m.dir) else { return Err("no exec".into()) };
    let c = &exec.command;
    let found = if c.starts_with('/') {
        Some(PathBuf::from(c))
    } else if c.contains('/') {
        Some(dir.join(c))
    } else {
        let path = std::env::var("PATH").unwrap_or_default();
        path.split(':').chain(PATH_FALLBACK).map(|d| Path::new(d).join(c)).find(|p| p.is_file())
    };
    let p = found.and_then(|p| p.canonicalize().ok()).ok_or_else(|| format!("exec command {c:?} was not found"))?;
    if c.contains('/') && !c.starts_with('/') && !p.starts_with(dir) {
        return Err(format!("exec command {c:?} leaves the plugin's folder"));
    }
    let named = |p: &Path| p.file_name().is_some_and(|n| NEVER_EXEC.iter().any(|x| n == *x));
    if named(Path::new(c)) || named(&p) {
        return Err(format!("{c} may not be a plugin's command"));
    }
    use std::os::unix::fs::PermissionsExt;
    if std::fs::metadata(&p).map(|md| md.permissions().mode() & 0o111 == 0).unwrap_or(true) {
        return Err(format!("exec command {} is not executable", p.display()));
    }
    Ok(p)
}

/// The agent's own folder a `data` path is inside, relative to HOME: `.<name>`, `.config/<name>` or `.local/share/<name>`;
/// None when the path is that folder itself or under none.
pub fn data_root(d: &str) -> Option<String> {
    let parts: Vec<&str> = d.strip_prefix("~/")?.split('/').collect();
    let n = match parts.as_slice() {
        [".config", _, ..] => 2,
        [".local", "share", _, ..] => 3,
        [p, ..] if p.starts_with('.') => 1,
        _ => return None,
    };
    (parts.len() > n).then(|| parts[..n].join("/"))
}

/// Why a `data` path holds what the agent runs or obeys: its settings, hooks, extensions or instructions.
fn runs_code(parts: &[&str]) -> Option<&'static str> {
    const CODE: &[&str] = &["hooks", "skills", "agents", "commands", "plugins", "extensions", "extension-store", "installed-plugins", "prompts", "bin", "memories", "rules", "ide"];
    if parts.iter().any(|p| CODE.contains(&p.to_ascii_lowercase().as_str())) {
        return Some("is inside a folder of code or instructions");
    }
    let last = parts.last()?.to_ascii_lowercase();
    let named = ["settings", "config", "mcp", "trust", "auth"].iter().any(|w| last.contains(w));
    let code = [".md", ".toml", ".js", ".mjs", ".cjs", ".ts", ".sh", ".py", ".yaml", ".yml"].iter().any(|e| last.ends_with(e));
    (named || code).then_some("names settings or code")
}

/// The config's folder relative to HOME: `.<name>` or `.config/<name>`, with the file somewhere below it.
fn config_dir(path: &str, names: &[String]) -> Option<String> {
    let parts: Vec<&str> = path.strip_prefix("~/")?.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || !text(p)) {
        return None;
    }
    let ours = |n: &str| names.iter().any(|x| x == n);
    match parts.as_slice() {
        [".config", n, _, ..] if ours(n) => Some(format!(".config/{n}")),
        [d, _, ..] if d.strip_prefix('.').is_some_and(ours) => Some(d.to_string()),
        _ => None,
    }
}

/// Exactly 8-4-4-4-12 hex.
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36 && b.iter().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

/// An id is a command-line argument: only its kind's shape, and never something that reads as a flag.
pub fn is_id(kind: IdKind, s: &str) -> bool {
    match kind {
        IdKind::Uuid => is_uuid(s),
        IdKind::Word => !s.is_empty() && s.len() <= 128 && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')),
    }
}

impl Manifest {
    pub fn runs_here(&self) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == "macos")
    }

    /// What the hook script calls this agent.
    pub fn from(&self) -> &str {
        if self.agent.arg.is_empty() { "claude" } else { &self.agent.arg }
    }

    pub fn program(&self) -> &str {
        &self.agent.names[0]
    }

    pub fn shape(&self) -> Option<Shape> {
        self.agent.config.as_ref().map(|c| c.shape)
    }

    /// Nerd installs only where this folder exists.
    pub fn dir(&self, home: &Path) -> Option<PathBuf> {
        let c = self.agent.config.as_ref()?;
        config_dir(&c.path, &self.agent.names).map(|d| home.join(d))
    }

    /// Its `data` paths under `home`.
    pub fn data_paths(&self, home: &Path) -> Vec<PathBuf> {
        self.agent.data.iter().filter_map(|d| d.strip_prefix("~/")).map(|d| home.join(d)).collect()
    }

    pub fn config(&self, home: &Path) -> Option<PathBuf> {
        Some(home.join(self.agent.config.as_ref()?.path.strip_prefix("~/")?))
    }

    pub fn is_id(&self, s: &str) -> bool {
        self.agent.session.as_ref().is_some_and(|x| is_id(x.kind, s))
    }

    /// None when the agent does not resume or the id is not of its kind.
    pub fn resume_argv(&self, id: &str) -> Option<Vec<String>> {
        if self.agent.resume.is_empty() || !self.is_id(id) {
            return None;
        }
        Some(self.agent.resume.iter().map(|w| w.replace("{id}", id)).collect())
    }

    /// The line as the human would read it.
    pub fn resume_line(&self) -> String {
        self.agent.resume.join(" ").replace("{id}", "<id>")
    }

    pub fn strip_resume(&self, argv: &[String]) -> Vec<String> {
        let s = &self.agent.strip;
        let mut out = Vec::new();
        let mut skip = false;
        for a in argv {
            if std::mem::take(&mut skip) {
                continue;
            }
            if s.value.contains(a) {
                skip = true;
            } else if !s.flag.contains(a) && !s.value.iter().any(|v| a.strip_prefix(v.as_str()).is_some_and(|r| r.starts_with('='))) {
                out.push(a.clone());
            }
        }
        out
    }

    /// IMPORTANT: check before resuming, or two copies write into one session at once.
    pub fn session_is_live(&self, id: &str) -> bool {
        crate::detect::ps_snapshot().map(|t| t.lines().any(|l| l.contains(id) && self.agent.names.iter().any(|n| l.contains(n.as_str())))).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(json: serde_json::Value) -> Manifest {
        let mut v = serde_json::json!({
            "name": "fake",
            "title": "Fake",
            "agent": {
                "names": ["fake", "fake-cli"],
                "config": { "path": "~/.fake/hooks.json", "shape": "nested" },
                "arg": "fake",
                "events": [{ "on": "Stop", "do": "idle" }],
                "session": { "field": "sessionId", "kind": "word" },
                "resume": ["fake", "--resume", "{id}"]
            }
        });
        for (k, x) in json.as_object().unwrap() {
            v["agent"][k] = x.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn refused(json: serde_json::Value) -> String {
        validate(&fake(json), &builtins()).unwrap_err()
    }

    #[test]
    fn every_builtin_passes_the_rules_a_user_manifest_must() {
        let b = builtins();
        assert_eq!(b.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["claude", "codex", "qwen", "gemini", "cursor", "copilot", "opencode", "pi"]);
        assert!(validate(&fake(serde_json::json!({})), &b).is_ok());
    }

    #[test]
    fn an_agent_s_mail_data_and_network_are_checked() {
        use serde_json::json;
        let ok = fake(json!({ "mail": { "via": "inbox", "form": "block", "inbox_env": "FAKE_INBOX" }, "data": ["~/.fake/sessions"], "network": { "hosts": ["api.fake.dev"], "ports": [443] }, "own_sandbox": "Run it with --no-sandbox." }));
        assert!(validate(&ok, &builtins()).is_ok());
        assert_eq!(ok.data_paths(Path::new("/h")), vec![PathBuf::from("/h/.fake/sessions")]);
        assert_eq!(ok.agent.mail.as_ref().map(|m| m.via), Some(MailVia::Inbox));
        for (bad, says) in [
            (json!({ "mail": { "via": "hook" } }), "needs a form"),
            (json!({ "mail": { "via": "inbox", "form": "deny" } }), "needs inbox_env"),
            (json!({ "mail": { "via": "inbox", "form": "deny", "inbox_env": "lower" } }), "needs inbox_env"),
            (json!({ "mail": { "via": "extension", "inbox_env": "X" } }), "inbox only"),
            (json!({ "own_sandbox": "two\nlines" }), "one line"),
            (json!({ "data": ["/abs"] }), "start with ~/"),
            (json!({ "data": ["~/"] }), "start with ~/"),
            (json!({ "data": ["~/.fake/../x"] }), "no .."),
            (json!({ "data": ["~/Library/Application Support/nerd/x"] }), "nerd's own"),
            (json!({ "data": ["~/.ssh/keys"] }), "under ~/.<name>"),
            (json!({ "data": ["~/.fake"] }), "under ~/.<name>"),
            (json!({ "data": ["~/.config/fake"] }), "under ~/.<name>"),
            (json!({ "data": ["~/.fake/hooks/x"] }), "folder of code"),
            (json!({ "data": ["~/.fake/Skills"] }), "folder of code"),
            (json!({ "data": ["~/.fake/settings.local.json"] }), "names settings"),
            (json!({ "data": ["~/.fake/mcp.json"] }), "names settings"),
            (json!({ "data": ["~/.fake/AGENTS.md"] }), "names settings"),
            (json!({ "data": ["~/.fake/*"] }), "a * only"),
            (json!({ "data": ["~/.fake/*/x"] }), "a * only"),
            (json!({ "data": ["~/.fake/a\"b"] }), "no quote"),
            (json!({ "network": "localhost" }), "names its hosts"),
            (json!({ "network": { "hosts": (0..17).map(|i| format!("h{i}.fake.dev")).collect::<Vec<_>>(), "ports": [443] } }), "at most 16 hosts"),
        ] {
            let e = refused(bad.clone());
            assert!(e.contains(says), "{bad}: {e}");
        }
        assert!(WORDS.contains(&"mail"));
        let mut wide = fake(json!({ "network": { "hosts": (0..64).map(|i| format!("h{i}.fake.dev")).collect::<Vec<_>>(), "ports": [443] } }));
        wide.builtin = true;
        assert!(validate(&wide, &builtins()).is_ok(), "a built-in agent may name all its providers' hosts");
    }

    #[test]
    fn a_hook_word_must_be_one_the_script_knows() {
        assert!(refused(serde_json::json!({ "events": [{ "on": "Stop", "do": "rm -rf" }] })).contains("not a hook word"));
        assert!(refused(serde_json::json!({ "events": [{ "on": "Stop\n", "do": "idle" }] })).contains("one line"));
    }

    #[test]
    fn an_arg_is_a_plain_token() {
        for bad in ["a b", "x;y", "$(id)", "A", "'q'"] {
            assert!(refused(serde_json::json!({ "arg": bad })).contains("arg"), "{bad}");
        }
        assert!(refused(serde_json::json!({ "arg": "codex", "config": { "path": "~/.fake/h.json", "shape": "nested" } })).contains("codex's"), "one arg, one agent");
    }

    #[test]
    fn a_session_kind_is_uuid_or_word_and_nothing_else() {
        let v = serde_json::json!({ "name": "fake", "title": "F", "agent": { "names": ["fake"], "session": { "kind": "regex" } } });
        assert!(serde_json::from_value::<Manifest>(v).is_err());
        assert!(refused(serde_json::json!({ "session": { "field": "a\"b" } })).contains("session field"));
        let s = fake(serde_json::json!({ "session": {} })).agent.session.unwrap();
        assert_eq!((s.field.as_str(), s.kind), ("session_id", IdKind::Uuid), "the defaults are Claude's");
    }

    #[test]
    fn id_kinds_take_only_their_shape() {
        assert!(is_id(IdKind::Uuid, "0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c"));
        for bad in ["abc", "-b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c", "0b5e7c3a_1d2f-4e6a-9b8c-7d6e5f4a3b2c", ""] {
            assert!(!is_id(IdKind::Uuid, bad), "{bad}");
        }
        assert!(is_id(IdKind::Word, "ses_4f2a.9-x"));
        for bad in ["", "-rf", "--dangerously-skip-permissions", "a b", "a;b", "a/b", &"x".repeat(129)] {
            assert!(!is_id(IdKind::Word, bad), "{bad}");
        }
    }

    #[test]
    fn a_resume_line_runs_its_own_agent_with_one_id() {
        assert!(refused(serde_json::json!({ "resume": ["sh", "-c", "{id}"] })).contains("not one of its names"));
        assert!(refused(serde_json::json!({ "resume": ["fake", "{id}", "{id}"] })).contains("exactly once"));
        assert!(refused(serde_json::json!({ "resume": ["fake", "--x"] })).contains("exactly once"));
        assert!(refused(serde_json::json!({ "resume": ["fake", "x{id}"] })).contains("resume word"));
        assert!(refused(serde_json::json!({ "resume": ["fake", "{id}", "a\nb"] })).contains("resume word"));
        assert!(refused(serde_json::json!({ "session": null })).contains("needs a session"));
        let m = fake(serde_json::json!({}));
        assert_eq!(m.resume_argv("s-1"), Some(vec!["fake".into(), "--resume".into(), "s-1".into()]));
        assert_eq!(m.resume_argv("-rf"), None, "an id that reads as a flag");
        let bound = fake(serde_json::json!({ "resume": ["fake-cli", "--resume={id}"] }));
        assert_eq!(bound.resume_argv("s1"), Some(vec!["fake-cli".into(), "--resume=s1".into()]));
    }

    #[test]
    fn a_config_path_stays_in_the_agents_own_folder() {
        for bad in ["~/.zshrc", "~/.claude/settings.json", "/etc/x.json", "~/.fake/../.ssh/config", "~/.fake", "~/.config/fake", "~/.config/other/x.json", "~/.fake//x"] {
            let e = refused(serde_json::json!({ "config": { "path": bad, "shape": "nested" } }));
            assert!(e.contains("config path"), "{bad}: {e}");
        }
        assert!(validate(&fake(serde_json::json!({ "config": { "path": "~/.config/fake-cli/hooks/x.json", "shape": "flat" } })), &builtins()).is_ok());
        let mut two = builtins();
        two.push(fake(serde_json::json!({})));
        let other = Manifest { name: "other".into(), agent: Agent { names: vec!["other".into(), "fake".into()], ..fake(serde_json::json!({ "arg": "other" })).agent }, ..fake(serde_json::json!({})) };
        assert!(validate(&other, &two).is_err_and(|e| e.contains("fake's")), "one config file, one manifest");
    }

    #[test]
    fn a_launcher_or_nerd_is_never_an_agent() {
        for n in ["sh", "zsh", "node", "env", "python3", "nerd"] {
            let e = refused(serde_json::json!({ "names": ["fake", n] }));
            assert!(e.contains("not an agent"), "{n}: {e}");
        }
    }

    #[test]
    fn a_user_manifest_only_merges_into_a_json_file() {
        for shape in ["own", "plugin"] {
            let e = refused(serde_json::json!({ "config": { "path": "~/.fake/hooks.json", "shape": shape } }));
            assert!(e.contains("nested or flat"), "{shape}: {e}");
        }
        assert!(validate(&fake(serde_json::json!({ "config": { "path": "~/.fake/hooks.json", "shape": "flat" } })), &builtins()).is_ok());
    }

    #[test]
    fn an_agent_without_its_own_prompt_text_is_read_by_claudes() {
        let claude = get("claude").unwrap().agent.blocked_text;
        assert_eq!(blocked_text(Some("claude")), claude);
        assert_eq!(blocked_text(Some("codex")), claude, "a manifest without blocked_text");
        assert_eq!(blocked_text(Some("amp")), claude, "an agent known by name only");
        assert!(blocked_text(None).is_empty(), "a shell is never asked a question");
    }

    #[test]
    fn a_user_manifest_cannot_take_a_builtin_name() {
        let e = refused(serde_json::json!({ "names": ["fake", "claude"] }));
        assert!(e.contains("built-in"), "{e}");
    }

    #[test]
    fn bad_manifests_are_skipped_and_good_ones_loaded() {
        let dir = std::env::temp_dir().join(format!("nerd-manifests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |folder: &str, text: &str| {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
            std::fs::write(dir.join(folder).join("plugin.json"), text).unwrap();
        };
        let good = serde_json::json!({ "name": "fake", "title": "Fake", "agent": { "names": ["fake"], "config": { "path": "~/.fake/h.json", "shape": "nested" }, "arg": "fake" } });
        put("fake", &good.to_string());
        put("claude", &serde_json::json!({ "name": "claude", "title": "Mine", "agent": { "names": ["claude"] } }).to_string());
        put("broken", "{ not json");
        put("elsewhere", &good.to_string());
        let names: Vec<String> = load(&dir).into_iter().filter(|m| !m.builtin).map(|m| m.name).collect();
        assert_eq!(names, ["fake"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_off_plugin_is_not_loaded_and_a_broken_off_file_loads_none() {
        let root = std::env::temp_dir().join(format!("nerd-off-{}", std::process::id()));
        let dir = root.join("plugins");
        for n in ["one", "two"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
            std::fs::write(dir.join(n).join("plugin.json"), serde_json::json!({ "name": n, "title": "T", "agent": { "names": [n] } }).to_string()).unwrap();
        }
        let users = |ms: Vec<Manifest>| ms.into_iter().filter(|m| !m.builtin).map(|m| m.name).collect::<Vec<_>>();
        assert_eq!(users(load(&dir)), ["one", "two"]);
        std::fs::write(root.join(crate::install::OFF), r#"{"one": "turned off"}"#).unwrap();
        assert_eq!(users(load(&dir)), ["two"]);
        assert_eq!(users(load_admitting(&dir, &|n| n != "two")), Vec::<String>::new(), "a folder not admitted waits");
        std::fs::write(root.join(crate::install::OFF), "{ broken").unwrap();
        assert_eq!(users(load(&dir)), Vec::<String>::new(), "fails closed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plugin with an executable, in a folder of its own under a temp state root.
    fn plugin(dir: &Path, extra: serde_json::Value) -> Result<Manifest, String> {
        let mut v = serde_json::json!({ "name": "plug", "title": "Plug", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        std::fs::create_dir_all(dir.join("plug")).unwrap();
        std::fs::write(dir.join("plug/plugin.json"), v.to_string()).unwrap();
        read_user(&dir.join("plug/plugin.json"), &builtins())
    }

    /// The data folder is the plugin's to write, so it can put a link at its name: neither the profile nor the next
    /// start may follow it, and what nerd opens there outside the sandbox must be a plain file.
    #[test]
    fn a_data_folder_swapped_for_a_link_grants_nothing() {
        crate::session::tests::with_temp_state_dir("data-link", || {
            let root = crate::paths::state_root();
            let secret = root.parent().unwrap().join("dotssh");
            std::fs::create_dir_all(&secret).unwrap();
            std::fs::write(secret.join("id"), "KEY").unwrap();
            let data = crate::plugins::data_dir("plug");
            std::fs::create_dir_all(data.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&secret, &data).unwrap();
            let m = plugin(&root.join("plugins"), serde_json::json!({})).unwrap();
            let secret_s = std::fs::canonicalize(&secret).unwrap().to_string_lossy().into_owned();
            assert!(!m.profile.contains(&secret_s), "the link's target is in no rule: {}", m.profile);
            assert!(!crate::plugins::plain_in_data("plug", "id"), "a file behind a link is not shown");

            let real = crate::plugins::real_data_dir("plug").unwrap();
            assert!(std::fs::symlink_metadata(&real).unwrap().is_dir(), "a real folder now");
            assert_eq!(std::fs::read_to_string(secret.join("id")).unwrap(), "KEY", "the target is untouched");
            std::fs::write(real.join("output.log"), "x").unwrap();
            assert!(crate::plugins::plain_in_data("plug", "output.log"));
            std::fs::remove_file(real.join("output.log")).unwrap();
            std::os::unix::fs::symlink(secret.join("id"), real.join("output.log")).unwrap();
            assert!(!crate::plugins::plain_in_data("plug", "output.log"), "a log that is a link is not shown");
        });
    }

    #[test]
    fn a_plugins_executable_half_is_checked_before_it_can_run() {
        crate::session::tests::with_temp_state_dir("plugin-rules", || {
            let dir = crate::paths::state_root().join("plugins");
            let m = plugin(&dir, serde_json::json!({})).unwrap();
            assert_eq!(m.program, Path::new("/bin/sh"));
            for (extra, why) in [
                (serde_json::json!({ "exec": { "command": "osascript" } }), "may not be"),
                (serde_json::json!({ "exec": { "command": "/usr/bin/open" } }), "may not be"),
                (serde_json::json!({ "exec": { "command": "launchctl" } }), "may not be"),
                (serde_json::json!({ "exec": { "command": "../../x.sh" } }), "was not found"),
                (serde_json::json!({ "exec": { "command": "sh", "args": ["a\nb"] } }), "one line"),
                (serde_json::json!({ "events": ["keys"] }), "not an event"),
                (serde_json::json!({ "events": [], "commands": [{ "id": "x", "title": "X", "where": "global" }] }), "command event"),
                (serde_json::json!({ "commands": [{ "id": "X y", "title": "X", "where": "global" }] }), "command"),
                (serde_json::json!({ "min_nerd": "999.0.0" }), "needs nerd"),
                (serde_json::json!({ "permissions": { "write": ["relative"] } }), "must start with"),
                (serde_json::json!({ "permissions": { "write": ["/tmp/../etc"] } }), "must start with"),
                (serde_json::json!({ "permissions": { "write": [crate::paths::state_root().to_string_lossy()] } }), "nerd's own files"),
                (serde_json::json!({ "permissions": { "write": ["~/a\"b"] } }), "cannot go into"),
                (serde_json::json!({ "permissions": { "write": ["/tmp/out"] } }), "inside your home folder"),
                (serde_json::json!({ "permissions": { "write": ["~/"] } }), "settings or programs"),
                (serde_json::json!({ "permissions": { "write": ["~/.zshrc"] } }), "settings or programs"),
                (serde_json::json!({ "permissions": { "write": ["~/.config/x"] } }), "settings or programs"),
                (serde_json::json!({ "permissions": { "write": ["~/Library/LaunchAgents"] } }), "settings or programs"),
                (serde_json::json!({ "permissions": { "write": ["~/library/launchagents"] } }), "settings or programs"),
                (serde_json::json!({ "permissions": { "write": ["~/Applications/x"] } }), "settings or programs"),
                (serde_json::json!({ "version": "1.2" }), "version"),
                (serde_json::json!({ "version": "01.2.3" }), "version"),
                (serde_json::json!({ "version": "1.2.3+build" }), "version"),
                (serde_json::json!({ "version": "1.2.3-" }), "version"),
                (serde_json::json!({ "platforms": ["bsd"] }), "platform"),
                (serde_json::json!({ "platforms": ["macos", "macos"] }), "each once"),
                (serde_json::json!({ "platforms": ["linux", "windows"] }), "not macOS"),
                (serde_json::json!({ "name": "claude" }), "folder"),
                (serde_json::json!({ "badges": ["rows"] }), "tree or tab"),
                (serde_json::json!({ "badges": ["tree", "tree"] }), "tree or tab"),
                (serde_json::json!({ "segments": [{ "id": "a" }, { "id": "b" }, { "id": "c" }, { "id": "d" }, { "id": "e" }] }), "at most 4 segments"),
                (serde_json::json!({ "segments": [{ "id": "A b" }] }), "segment"),
                (serde_json::json!({ "segments": [{ "id": "a" }, { "id": "a" }] }), "each once"),
                (serde_json::json!({ "previews": [{ "extensions": ["log"] }] }), "preview_request event"),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": ["CSV"] }] }), "preview extension \"CSV\""),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": [".csv"] }] }), "without the dot"),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": [""] }] }), "preview extension"),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": ["a".repeat(17)] }] }), "preview extension"),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": ["log"] }, { "extensions": ["log"] }] }), "each once"),
                (serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": (0..17).map(|i| format!("e{i}")).collect::<Vec<_>>() }] }), "at most 16 preview extensions"),
                (serde_json::json!({ "panels": (0..5).map(|i| serde_json::json!({ "id": format!("p{i}"), "title": "P", "where": "global" })).collect::<Vec<_>>() }), "at most 4 panels"),
                (serde_json::json!({ "panels": [{ "id": "A b", "title": "P", "where": "global" }] }), "panel \"A b\""),
                (serde_json::json!({ "panels": [{ "id": "a", "title": "P", "where": "global" }, { "id": "a", "title": "Q", "where": "tree" }] }), "each once"),
                (serde_json::json!({ "panels": [{ "id": "a", "title": "", "where": "global" }] }), "short line"),
                (serde_json::json!({ "panels": [{ "id": "a", "title": "x".repeat(41), "where": "global" }] }), "short line"),
                (serde_json::json!({ "panels": [{ "id": "a", "title": "a\nb", "where": "global" }] }), "short line"),
                (serde_json::json!({ "panels": [{ "id": "a", "title": "P", "where": "sidebar" }] }), "unknown variant"),
            ] {
                let e = plugin(&dir, extra.clone()).unwrap_err();
                assert!(e.contains(why), "{extra}: {e}");
            }
            let e = validate(&Manifest { exec: None, ..plugin(&dir, serde_json::json!({})).unwrap() }, &builtins()).unwrap_err();
            assert!(e.contains("an agent or an exec"), "{e}");
            let events_only = Manifest { events: vec!["command".into()], ..fake(serde_json::json!({})) };
            assert!(validate(&events_only, &builtins()).unwrap_err().contains("need an exec"));
            let badges_only = Manifest { badges: vec!["tree".into()], ..fake(serde_json::json!({})) };
            assert!(validate(&badges_only, &builtins()).unwrap_err().contains("need an exec"));
            let slots = plugin(&dir, serde_json::json!({ "badges": ["tree", "tab"], "segments": [{ "id": "a" }, { "id": "b-2" }] })).unwrap();
            assert_eq!((slots.badges.len(), slots.segments.len()), (2, 2));
            let panels_only = Manifest { panels: vec![PanelDecl { id: "p".into(), title: "P".into(), place: Place::Global }], ..fake(serde_json::json!({})) };
            assert!(validate(&panels_only, &builtins()).unwrap_err().contains("need an exec"));
            let four: Vec<serde_json::Value> = ["global", "pane", "tree", "tree"].iter().enumerate().map(|(i, w)| serde_json::json!({ "id": format!("p-{i}"), "title": "x".repeat(40), "where": w })).collect();
            let p = plugin(&dir, serde_json::json!({ "panels": four })).unwrap();
            assert_eq!(crate::slots::Decl::of(&p).panels.iter().map(|p| p.2).collect::<Vec<_>>(), [Place::Global, Place::Pane, Place::Tree, Place::Tree], "4 at the caps");
            let previews_only = Manifest { previews: vec![PreviewDecl { extensions: vec!["log".into()] }], ..fake(serde_json::json!({})) };
            assert!(validate(&previews_only, &builtins()).unwrap_err().contains("need an exec"));
            let sixteen: Vec<String> = (0..16).map(|i| format!("e{i}")).collect();
            let p = plugin(&dir, serde_json::json!({ "events": ["preview_request"], "previews": [{ "extensions": ["log", "tar_gz", "x-y"] }, { "extensions": sixteen[3..] }] })).unwrap();
            assert_eq!(crate::slots::Decl::of(&p).previews.len(), 16, "16 in all, over two entries");
            for name in crate::slots::BUILTINS {
                let taken = Manifest { name: name.to_string(), ..plugin(&dir, serde_json::json!({})).unwrap() };
                assert!(validate(&taken, &builtins()).unwrap_err().contains("built-in's name"), "{name}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_plugins_profile_denies_by_default_and_names_what_it_allows() {
        crate::session::tests::with_temp_state_dir("plugin-profile", || {
            let dir = crate::paths::state_root().join("plugins");
            let p = plugin(&dir, serde_json::json!({})).unwrap().profile;
            let sock = crate::session::plugin_socket_path();
            let sock = sock.parent().unwrap().canonicalize().unwrap().join(sock.file_name().unwrap());
            assert!(p.starts_with("(version 1)\n(deny default)\n"), "{p}");
            assert!(p.contains(&format!("(allow network-outbound (remote unix-socket (path-literal \"{}\")))", sock.display())), "{p}");
            assert!(p.contains("(literal \"/bin/sh\")") && p.contains("(deny process-exec (literal \"/usr/bin/open\")"), "{p}");
            let data = crate::paths::state_root().canonicalize().unwrap().join("plugin-data/plug");
            assert!(p.contains(&format!("(allow file-write* (literal \"/dev/null\") (subpath \"{}\"))", data.display())), "{p}");
            assert!(!p.contains("(allow network*)"), "no network unless declared");
            let root = crate::paths::state_root().canonicalize().unwrap();
            let deny = p.find("(deny file-read-data").unwrap();
            let own = p.find(&format!("(allow file-read-data (subpath \"{}\") (subpath \"{}\"))", dir.canonicalize().unwrap().join("plug").display(), data.display())).expect(&p);
            assert!(deny < own, "its own folders are readable again after the deny: the last rule wins");
            for secret in [".ssh", ".aws", ".gnupg", "Library", ".netrc", ".docker", ".config/gh", ".zsh_history"] {
                assert!(p[deny..own].contains(&format!("/{secret}\")")), "{secret}: {p}");
            }
            assert!(p[deny..own].contains(&format!("(subpath \"{}\")", root.display())), "the state root: {p}");
            let net = plugin(&dir, serde_json::json!({ "version": "1.2.3-rc.1", "platforms": ["linux", "macos"], "permissions": { "network": { "hosts": ["example.com"], "ports": [443, 80] }, "write": ["~/nerd-plug.out"] } })).unwrap().profile;
            assert!(!net.contains("remote ip") && !net.contains("mDNSResponder") && !net.contains("network*") && net.contains("/nerd-plug.out\")"), "no network of its own: the wrapper adds its proxy's port: {net}");
            let local = plugin(&dir, serde_json::json!({ "permissions": { "network": "localhost" } })).unwrap().profile;
            assert!(local.contains("(allow network-outbound (remote ip \"localhost:*\"))") && !local.contains("mDNSResponder") && !local.contains("network*"), "this Mac only, no name lookups: {local}");
            assert!(!p.contains("remote ip") && !p.contains("mDNSResponder"), "no permission, no network: {p}");
            assert!(net.contains("(deny file-write* (regex #\"^/") && net.contains("/nerd-plug\\.out/(.*/)?\\.[^/]*\"))"), "hidden entries under a write path stay unwritable: {net}");
            assert!(!p.contains("(deny file-write* (regex"), "no write path, no hidden rule: {p}");
            assert!(p.contains("(deny file-read* file-write* file-ioctl (regex #\"^/dev/(tty|ttys[0-9]+"), "every plugin is kept off the terminals: {p}");
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn an_agent_network_is_hosts_or_a_built_ins_provider_groups() {
        use serde_json::json;
        let n = |v: serde_json::Value| agent_network(v);
        let hosts = n(json!({ "hosts": ["b.dev", "a.dev"], "ports": [443] })).unwrap();
        assert_eq!(hosts, AgentNetwork { net: Network::Internet { hosts: vec!["a.dev".into(), "b.dev".into()], ports: vec![443] }, providers: Vec::new() });
        let groups = n(json!({ "providers": { "zed": ["z.dev", "a.dev"], "acme": ["a.dev"] }, "ports": [443] })).unwrap();
        assert_eq!(groups.providers, vec![("acme".to_string(), vec!["a.dev".to_string()]), ("zed".into(), vec!["a.dev".into(), "z.dev".into()])]);
        assert_eq!(groups.net, Network::Internet { hosts: vec!["a.dev".into(), "z.dev".into()], ports: vec![443] }, "the union");
        assert_eq!(n(serde_json::Value::Null), Ok(AgentNetwork::default()));
        let many = |k: usize, per: usize| json!({ "providers": (0..k).map(|i| (format!("g{i}"), json!((0..per).map(|j| format!("h{i}-{j}.dev")).collect::<Vec<_>>()))).collect::<serde_json::Map<_, _>>(), "ports": [443] });
        for (bad, says) in [
            (json!({ "hosts": ["a.dev"], "providers": { "a": ["a.dev"] }, "ports": [443] }), "not both"),
            (json!({ "providers": { "a": [] }, "ports": [443] }), "must list its hosts"),
            (json!({ "providers": {}, "ports": [443] }), "1 to 64 providers"),
            (json!({ "providers": ["a.dev"], "ports": [443] }), "must be an object"),
            (json!({ "providers": { "A": ["a.dev"] }, "ports": [443] }), "lowercase letters"),
            (json!({ "providers": { "a/b": ["a.dev"] }, "ports": [443] }), "lowercase letters"),
            (json!({ "providers": { "a": ["A.dev"] }, "ports": [443] }), "lowercase domain name"),
            (json!({ "providers": { "a": ["a.dev"] } }), "ports must be a list"),
            (json!({ "providers": { "a": ["a.dev"] }, "ports": [443], "other": 1 }), "\"other\" is not one of its keys"),
            (many(65, 1), "1 to 64 providers"),
            (many(13, 5), "at most 16 hosts"),
        ] {
            let e = n(bad.clone()).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
        assert!(n(many(64, 1)).is_ok(), "64 groups");
        let mut builtin = fake(json!({ "network": many(16, 4) }));
        builtin.builtin = true;
        assert!(validate(&builtin, &builtins()).is_ok(), "a built-in's union may reach 64 hosts");
        assert!(refused(json!({ "network": many(2, 1) })).contains("for built-in agents only"), "a user's or a plugin's manifest names hosts");
        assert!(refused(json!({ "network": many(17, 1) })).contains("at most 16 hosts"), "a user's union counts against 16");
    }

    #[test]
    fn pis_provider_groups_are_pinned() {
        const WHY: &str = "renaming or removing a group strands panes saved with it: see docs/isolation.md";
        let pi = builtins().into_iter().find(|m| m.name == "pi").unwrap().agent.network;
        let want: &[(&str, &[&str])] = &[
            ("amazon-bedrock", &["bedrock-runtime.eu-central-1.amazonaws.com", "bedrock-runtime.us-east-1.amazonaws.com"]),
            ("ant-ling", &["api.ant-ling.com"]),
            ("anthropic", &["api.anthropic.com", "claude.ai", "platform.claude.com"]),
            ("baseten", &["inference.baseten.co"]),
            ("cerebras", &["api.cerebras.ai"]),
            ("deepseek", &["api.deepseek.com"]),
            ("fireworks", &["api.fireworks.ai"]),
            ("github-copilot", &["api.business.githubcopilot.com", "api.enterprise.githubcopilot.com", "api.github.com", "api.individual.githubcopilot.com", "github.com"]),
            ("google", &["generativelanguage.googleapis.com"]),
            ("groq", &["api.groq.com"]),
            ("huggingface", &["router.huggingface.co"]),
            ("kimi-coding", &["api.kimi.com", "auth.kimi.com"]),
            ("meta", &["api.meta.ai", "auth.meta.com"]),
            ("minimax", &["api.minimax.io"]),
            ("minimax-cn", &["api.minimaxi.com"]),
            ("mistral", &["api.mistral.ai"]),
            ("moonshotai", &["api.moonshot.ai"]),
            ("moonshotai-cn", &["api.moonshot.cn"]),
            ("nvidia", &["integrate.api.nvidia.com"]),
            ("openai", &["api.openai.com", "auth.openai.com"]),
            ("openai-codex", &["auth.openai.com", "chatgpt.com"]),
            ("opencode", &["opencode.ai"]),
            ("openrouter", &["openrouter.ai", "us.openrouter.ai"]),
            ("qwen-token-plan", &["token-plan.ap-southeast-1.maas.aliyuncs.com"]),
            ("qwen-token-plan-cn", &["token-plan.cn-beijing.maas.aliyuncs.com"]),
            ("radius", &["radius.pi.dev"]),
            ("together", &["api.together.ai"]),
            ("typesafe", &["api.typesafe.ai"]),
            ("vercel-ai-gateway", &["ai-gateway.vercel.sh"]),
            ("xai", &["api.x.ai", "auth.x.ai"]),
            ("xiaomi", &["api.xiaomimimo.com"]),
            ("xiaomi-token-plan-ams", &["token-plan-ams.xiaomimimo.com"]),
            ("xiaomi-token-plan-cn", &["token-plan-cn.xiaomimimo.com"]),
            ("xiaomi-token-plan-sgp", &["token-plan-sgp.xiaomimimo.com"]),
            ("zai", &["api.z.ai"]),
            ("zai-coding-cn", &["open.bigmodel.cn"]),
        ];
        let names: Vec<&str> = pi.providers.iter().map(|(g, _)| g.as_str()).collect();
        assert_eq!(names, want.iter().map(|(g, _)| *g).collect::<Vec<_>>(), "{WHY}");
        for ((g, hosts), (wg, wh)) in pi.providers.iter().zip(want) {
            assert_eq!((g.as_str(), hosts.iter().map(String::as_str).collect::<Vec<_>>()), (*wg, wh.to_vec()), "{WHY}");
        }
        let Network::Internet { hosts, ports } = &pi.net else { panic!("pi names hosts") };
        assert_eq!((hosts.len(), ports.as_slice()), (48, &[443][..]));
        assert!(!hosts.iter().any(|h| h == "console.anthropic.com" || h == "api.githubcopilot.com"), "pi-ai 1.3.0 names neither: {hosts:?}");
    }

    #[test]
    fn a_plugins_network_is_none_localhost_or_named_ports_and_true_is_refused() {
        let n = |v: serde_json::Value| network(&v);
        assert_eq!(n(serde_json::json!(false)), Ok(Network::None));
        assert_eq!(n(serde_json::Value::Null), Ok(Network::None));
        assert_eq!(n(serde_json::json!("localhost")), Ok(Network::Localhost));
        assert_eq!(n(serde_json::json!({ "hosts": ["wttr.in"], "ports": [443] })), Ok(Network::Internet { hosts: vec!["wttr.in".into()], ports: vec![443] }));
        let addrs = serde_json::json!({ "hosts": ["192.168.1.5", "::1", "::ffff:10.0.0.1", "2001:DB8::1", "1.example"], "ports": [80] });
        assert_eq!(n(addrs), Ok(Network::Internet { hosts: ["1.example", "10.0.0.1", "192.168.1.5", "2001:db8::1", "::1"].map(String::from).to_vec(), ports: vec![80] }), "addresses in their standard form, private ones too");
        for (bad, says) in [
            (serde_json::json!(true), "true is gone"),
            (serde_json::json!("internet"), r#""localhost" or"#),
            (serde_json::json!({ "hosts": ["wttr.in"] }), "ports must be a list"),
            (serde_json::json!({ "hosts": [], "ports": [443] }), "hosts must be a list"),
            (serde_json::json!({ "hosts": ["Wttr.in"], "ports": [443] }), "lowercase domain name"),
            (serde_json::json!({ "hosts": ["localhost"], "ports": [443] }), "lowercase domain name"),
            (serde_json::json!({ "hosts": ["*.example.com"], "ports": [443] }), "lowercase domain name"),
            (serde_json::json!({ "hosts": ["2130706433"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["0x7f.1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["010.0.0.1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["127.1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["127.0x1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["10.0x1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["0x7f.0x1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["1.0x7f"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["[::1]"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["0.0.0.0"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["::"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["224.0.0.251"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["ff02::1"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["255.255.255.255"], "ports": [443] }), "or an IP address"),
            (serde_json::json!({ "hosts": ["wttr.in"], "ports": [0] }), "from 1 to 65535"),
            (serde_json::json!({ "hosts": ["wttr.in"], "ports": [70000] }), "from 1 to 65535"),
            (serde_json::json!({ "hosts": ["wttr.in"], "ports": ["443"] }), "from 1 to 65535"),
            (serde_json::json!({ "hosts": ["wttr.in"], "ports": [1, 2, 3, 4, 5, 6, 7, 8, 9] }), "at most"),
            (serde_json::json!({ "hosts": ["wttr.in"], "ports": [443], "any": true }), "is not one of its keys"),
        ] {
            assert!(n(bad.clone()).unwrap_err().contains(says), "{bad}: {:?}", n(bad.clone()));
        }
    }

    /// Each tier's profile, applied to a real nc against listeners on this Mac: no internet needed, and none touched.
    #[test]
    fn a_plugins_network_tier_reaches_only_what_it_names() {
        use std::net::TcpListener;
        crate::session::tests::with_temp_state_dir("plugin-net-tiers", || {
            let dir = crate::paths::state_root().join("plugins");
            let (open4, shut) = (TcpListener::bind("127.0.0.1:0").unwrap(), TcpListener::bind("127.0.0.1:0").unwrap());
            let open6 = TcpListener::bind("[::1]:0").unwrap();
            let (p, q, p6) = (open4.local_addr().unwrap().port(), shut.local_addr().unwrap().port(), open6.local_addr().unwrap().port());
            let tries = |profile: &str, args: &[&str]| {
                std::process::Command::new("/usr/bin/sandbox-exec").arg("-p").arg(profile).arg("/usr/bin/nc").args(["-z", "-w", "1"]).args(args).output().unwrap().status.success()
            };
            let profile = |v: serde_json::Value| plugin(&dir, serde_json::json!({ "permissions": { "network": v } })).unwrap().profile;
            let none = profile(serde_json::json!(false));
            assert!(!tries(&none, &["127.0.0.1", &p.to_string()]), "no network reaches nothing");
            let local = profile(serde_json::json!("localhost"));
            assert!(tries(&local, &["127.0.0.1", &p.to_string()]) && tries(&local, &["-6", "::1", &p6.to_string()]), "localhost reaches this Mac");
            let named = profile(serde_json::json!({ "hosts": ["example.com"], "ports": [p] }));
            assert!(!tries(&named, &["127.0.0.1", &p.to_string()]), "hosts and ports reach nothing by themselves");
            let proxied = format!("{named}(allow network-outbound (remote ip \"localhost:{p}\"))\n");
            assert!(tries(&proxied, &["127.0.0.1", &p.to_string()]), "the proxy's port, as the wrapper appends it, is reachable");
            assert!(!tries(&proxied, &["127.0.0.1", &q.to_string()]), "another port is not");
            for profile in [&local, &proxied] {
                let mut l = std::process::Command::new("/usr/bin/sandbox-exec").arg("-p").arg(profile).args(["/usr/bin/nc", "-l", "127.0.0.1", "0"]).stderr(std::process::Stdio::piped()).spawn().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(500));
                let listening = l.try_wait().unwrap().is_none();
                let _ = l.kill();
                assert!(!listening, "a plugin cannot listen: {profile}");
            }
            // No plugin reads a terminal: what is typed into a pane, or into nerd's own window, is not its to see.
            let pty = nix::pty::openpty(None, None).unwrap();
            let name = nix::unistd::ttyname(&pty.slave).unwrap();
            let read = std::process::Command::new("/usr/bin/sandbox-exec").arg("-p").arg(&none).args(["/bin/dd", "bs=1", "count=1"]).arg(format!("if={}", name.display())).arg("of=/dev/null").output().unwrap();
            assert!(!read.status.success() && String::from_utf8_lossy(&read.stderr).contains("Operation not permitted"), "{}", String::from_utf8_lossy(&read.stderr));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn what_a_plugin_can_do_comes_from_its_manifest_and_only_new_kinds_are_new() {
        use crate::plugins::{Ability, abilities, risk};
        crate::session::tests::with_temp_state_dir("plugin-abilities", || {
            let dir = crate::paths::state_root().join("plugins");
            let shows = plugin(&dir, serde_json::json!({ "events": ["command", "preview_request", "pane_opened"], "commands": [{ "id": "a", "title": "A", "where": "global" }], "badges": ["tree"], "previews": [{ "extensions": ["log"] }] })).unwrap();
            let a = abilities(&shows).unwrap();
            assert_eq!(risk(&a), 0);
            assert!(a.contains(&Ability::Runs("/bin/sh".into())) && a.contains(&Ability::Commands) && a.contains(&Ability::PaneFolders));
            assert!(a.iter().any(|x| x.to_string() == "Learns which .log files you select, and reads them"), "{a:?}");
            let more_items = plugin(&dir, serde_json::json!({ "events": ["command", "preview_request", "pane_opened"], "commands": [{ "id": "a", "title": "A", "where": "global" }, { "id": "b", "title": "Renamed", "where": "pane" }], "badges": ["tree"], "previews": [{ "extensions": ["log"] }] })).unwrap();
            assert!(abilities(&more_items).unwrap().difference(&a).next().is_none(), "a second menu item is nothing new");
            let writes = plugin(&dir, serde_json::json!({ "permissions": { "write": ["~/notes"] } })).unwrap();
            assert_eq!(risk(&abilities(&writes).unwrap()), 1);
            let net = |v: serde_json::Value| abilities(&plugin(&dir, serde_json::json!({ "permissions": { "network": v } })).unwrap()).unwrap();
            let n = net(serde_json::json!({ "hosts": ["wttr.in"], "ports": [443] }));
            assert_eq!(risk(&n), 2);
            let new = n.difference(&abilities(&plugin(&dir, serde_json::json!({})).unwrap()).unwrap()).map(|a| a.to_string()).collect::<Vec<_>>();
            assert_eq!(new, ["Reaches only wttr.in on port 443, through nerd's filter: a name at whatever public address its owner gives it, never this Mac or a network it is on; it looks up no names itself; could send anything it can read to those hosts, the environment of your other programs included"]);
            let two = net(serde_json::json!({ "hosts": ["wttr.in", "192.168.1.5", "8.8.8.8"], "ports": [443, 80] }));
            assert!(two.iter().any(|a| a.to_string().starts_with("Reaches only 192.168.1.5, 8.8.8.8, wttr.in on ports 80, 443, through nerd's filter: a name at whatever public address its owner gives it, never this Mac or a network it is on; it looks up no names itself; reaches your local network or this Mac at 192.168.1.5; could send")), "{two:?}");
            assert_eq!(net(serde_json::json!({ "hosts": ["b.example", "a.example"], "ports": [80, 443] })), net(serde_json::json!({ "ports": [443, 80, 443], "hosts": ["a.example", "b.example"] })), "a reordered list asks nothing");
            let local = net(serde_json::json!("localhost"));
            assert_eq!(risk(&local), 2);
            assert!(n.difference(&local).next().is_some(), "localhost to the internet asks again");
            assert!(net(serde_json::json!({ "hosts": ["wttr.in"], "ports": [443, 22] })).difference(&n).next().is_some(), "an added port asks again");
            assert!(net(serde_json::json!({ "hosts": ["wttr.in", "evil.example"], "ports": [443] })).difference(&n).next().is_some(), "an added host asks again");
            assert!(!net(serde_json::json!(false)).iter().any(|a| matches!(a, Ability::Network(_))), "false is no network");
            let wide = net(serde_json::json!({ "hosts": ["a.example", "b.example"], "ports": [443, 80] }));
            assert!(crate::plugins::new_abilities(&net(serde_json::json!({ "hosts": ["a.example"], "ports": [443] })), &wide).is_empty(), "a narrower network asks nothing");
            assert_eq!(crate::plugins::new_abilities(&net(serde_json::json!({ "hosts": ["a.example"], "ports": [22] })), &wide).len(), 1, "a port outside the old ones asks");
            assert_eq!(crate::plugins::new_abilities(&local, &wide).len(), 1, "localhost is not inside some ports: it asks");
            let _ = std::fs::remove_dir_all(&dir);
        });
        let agent = fake(serde_json::json!({}));
        let a = abilities(&agent).unwrap();
        assert_eq!(risk(&a), 3);
        assert_eq!(a.iter().map(|x| x.to_string()).collect::<Vec<_>>(), ["Speaks for the agent fake, fake-cli, adding nerd's hooks to ~/.fake/hooks.json, and restores its panes by typing `fake --resume {id}`"]);
    }

    #[test]
    fn a_checked_folder_reports_what_it_can_do_and_every_problem() {
        crate::session::tests::with_temp_state_dir("plugin-check", || {
            let root = crate::paths::state_root();
            let dir = root.join("work/nerd-plug");
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = |extra: serde_json::Value| {
                let mut v = serde_json::json!({ "name": "plug", "title": "Plug", "version": "1.0.0", "min_nerd": "0.1.0", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] });
                for (k, x) in extra.as_object().unwrap() {
                    v[k] = x.clone();
                }
                std::fs::write(dir.join("plugin.json"), v.to_string()).unwrap();
            };
            manifest(serde_json::json!({}));
            let c = check_folder(&dir, false);
            assert!(c.problems.is_empty(), "a folder not named after the plugin is fine: {:?}", c.problems);
            assert_eq!((c.name.as_str(), c.risk, c.abilities[0].as_str()), ("plug", 0, "Runs /bin/sh"));
            let json: serde_json::Value = serde_json::to_value(&c).unwrap();
            assert_eq!(json.as_object().unwrap().keys().collect::<Vec<_>>(), ["name", "title", "version", "platforms", "risk", "abilities", "problems"]);
            assert_eq!(check_folder(&dir, true).problems, ["the catalog needs a LICENSE file"]);
            std::fs::write(dir.join("LICENSE"), "MIT").unwrap();
            assert!(check_folder(&dir, true).problems.is_empty());
            std::fs::create_dir_all(root.join("outside")).unwrap();
            std::os::unix::fs::symlink(root.join("outside"), dir.join("out")).unwrap();
            std::os::unix::fs::symlink(dir.join("LICENSE"), dir.join("in")).unwrap();
            std::fs::write(dir.join("big"), vec![0u8; FILE_MAX as usize + 1]).unwrap();
            assert_eq!(check_folder(&dir, false).problems, ["big is over 10 MB", "out links outside the plugin's folder"]);
            std::fs::remove_file(dir.join("big")).unwrap();
            std::fs::remove_file(dir.join("out")).unwrap();
            manifest(serde_json::json!({ "name": "claude" }));
            assert!(check_folder(&dir, false).problems[0].contains("built-in"));
            // An installed plugin owns its agent's name; another may not take it, and it may replace itself.
            let installed = root.join("plugins/fake");
            std::fs::create_dir_all(&installed).unwrap();
            std::fs::write(installed.join("plugin.json"), serde_json::json!({ "name": "fake", "title": "Fake", "agent": { "names": ["fake"] } }).to_string()).unwrap();
            reload();
            manifest(serde_json::json!({ "name": "other", "agent": { "names": ["other", "fake"] } }));
            assert!(check_folder(&dir, false).problems[0].contains("already taken by fake"), "{:?}", check_folder(&dir, false).problems);
            manifest(serde_json::json!({ "name": "fake", "exec": null, "events": [], "agent": { "names": ["fake"] } }));
            assert!(check_folder(&dir, false).problems.is_empty(), "{:?}", check_folder(&dir, false).problems);
            assert!(!check_folder(&root.join("none"), false).problems.is_empty());
            let _ = std::fs::remove_dir_all(root.join("plugins"));
            reload();
        });
    }

    #[test]
    fn strip_resume_leaves_a_fresh_agent() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let b = builtins();
        let by = |n: &str| b.iter().find(|m| m.name == n).unwrap();
        assert_eq!(by("claude").strip_resume(&v(&["claude", "--resume", "abc", "--rc", "name"])), v(&["claude"]));
        assert_eq!(by("claude").strip_resume(&v(&["claude", "--resume=abc", "--continue"])), v(&["claude"]));
        assert_eq!(by("claude").strip_resume(&v(&["zsh", "-f"])), v(&["zsh", "-f"]));
        assert_eq!(by("codex").strip_resume(&v(&["codex", "resume", "abc", "--model", "o"])), v(&["codex", "--model", "o"]));
        assert_eq!(by("opencode").strip_resume(&v(&["opencode", "-s", "ses_1", "-c"])), v(&["opencode"]));
        assert_eq!(by("pi").strip_resume(&v(&["pi", "--session", "019a", "--model", "m", "-c"])), v(&["pi", "--model", "m"]));
    }

    #[test]
    fn each_agent_resumes_with_its_own_line() {
        let b = builtins();
        let uuid = "0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c";
        let argv = |n: &str, id: &str| b.iter().find(|m| m.name == n).unwrap().resume_argv(id).map(|v| v.join(" "));
        assert_eq!(argv("claude", uuid), Some(format!("claude --resume={uuid}")));
        assert_eq!(argv("codex", uuid), Some(format!("codex resume {uuid}")));
        let recorded = "01a0e00a-52ae-7ef0-a6ff-7e16d3f63eeb";
        assert_eq!(argv("codex", recorded), Some(format!("codex resume {recorded}")), "a Codex id as recorded: uuid-shaped, taken as a word");
        assert_eq!(argv("copilot", uuid), Some(format!("copilot --resume {uuid}")));
        assert_eq!(argv("gemini", uuid), Some(format!("gemini --resume {uuid}")));
        assert_eq!(argv("qwen", uuid), Some(format!("qwen --resume {uuid}")));
        assert_eq!(argv("opencode", "ses_9aB"), Some("opencode --session ses_9aB".into()));
        assert_eq!(argv("pi", recorded), Some(format!("pi --session {recorded}")), "a UUIDv7, pi's default id");
        assert_eq!(argv("pi", "my-task.2"), Some("pi --session my-task.2".into()), "an id given with --session-id");
        assert_eq!(argv("cursor", uuid), None, "Cursor does not resume");
        assert_eq!(argv("claude", "abc-123"), None);
    }

    /// Every templates/ example, installed the way a user would (folder renamed to the manifest's
    /// name), passes the same validator and sandbox build a real plugin does.
    #[test]
    fn every_template_manifest_validates_and_builds_its_profile() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let dir = std::env::temp_dir().join(format!("nerd-template-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for folder in ["plugin", "badges", "segment", "preview", "panels"] {
            let src = root.join(folder);
            let text = std::fs::read_to_string(src.join("plugin.json")).unwrap();
            let name = serde_json::from_str::<Manifest>(&text).unwrap().name;
            let dst = dir.join(&name);
            std::fs::create_dir_all(&dst).unwrap();
            for f in ["plugin.json", "plugin.sh"] {
                std::fs::copy(src.join(f), dst.join(f)).unwrap();
            }
            let m = read_user(&dst.join("plugin.json"), &builtins()).unwrap_or_else(|e| panic!("templates/{folder}: {e}"));
            assert!(m.exec.is_some() && !m.profile.is_empty(), "templates/{folder}: no sandbox profile built");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
