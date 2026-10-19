//! Who may message whom: the routes and modes in `policy.json`, one copy in memory, and the isolation types.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use serde::{Deserialize, Serialize};

/// One end of a route.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Pane(String),
    Space(String),
    Tab(String, String),
    Uid(u32),
    Agent(String),
    You,
    All,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope, String> {
        let nonempty = |v: &str, what: &str| if v.is_empty() || v.chars().any(char::is_control) { Err(format!("{s:?}: the {what} is missing or has a control character")) } else { Ok(v.to_string()) };
        if s == "*" {
            Ok(Scope::All)
        } else if s == "you" {
            Ok(Scope::You)
        } else if let Some(v) = s.strip_prefix("space:") {
            Ok(Scope::Space(nonempty(v, "space")?))
        } else if let Some(v) = s.strip_prefix("tab:") {
            let (sp, t) = v.split_once('/').ok_or_else(|| format!("{s:?}: a tab is tab:<space>/<tab>"))?;
            Ok(Scope::Tab(nonempty(sp, "space")?, nonempty(t, "tab")?))
        } else if let Some(v) = s.strip_prefix("uid:") {
            v.parse().map(Scope::Uid).map_err(|_| format!("{s:?}: a uid is a number"))
        } else if let Some(v) = s.strip_prefix("agent:") {
            Ok(Scope::Agent(nonempty(v, "agent")?))
        } else if valid_address(s) {
            Ok(Scope::Pane(s.to_string()))
        } else {
            Err(format!("{s:?} is not a pane name, space:, tab:, uid:, agent:, you or *"))
        }
    }

    pub fn text(&self) -> String {
        match self {
            Scope::Pane(n) => n.clone(),
            Scope::Space(s) => format!("space:{s}"),
            Scope::Tab(s, t) => format!("tab:{s}/{t}"),
            Scope::Uid(u) => format!("uid:{u}"),
            Scope::Agent(a) => format!("agent:{a}"),
            Scope::You => "you".into(),
            Scope::All => "*".into(),
        }
    }

    /// How specific: the more specific route wins.
    pub fn kind(&self) -> u8 {
        match self {
            Scope::Pane(_) | Scope::You => 2,
            Scope::Space(_) | Scope::Tab(..) | Scope::Uid(_) | Scope::Agent(_) => 1,
            Scope::All => 0,
        }
    }

    fn holds_sender(&self, s: &Sender) -> bool {
        self.holds(&s.name, &s.space, &s.tab, s.uid, s.agent.as_deref())
    }

    fn holds_recipient(&self, r: &Recipient) -> bool {
        match r {
            Recipient::You => *self == Scope::You,
            Recipient::Pane { name, space, tab, uid, agent } => self.holds(name, space, tab, *uid, agent.as_deref()),
        }
    }

    fn holds(&self, name: &str, space: &str, tab: &str, uid: u32, agent: Option<&str>) -> bool {
        match self {
            Scope::Pane(n) => n == name,
            Scope::Space(s) => s == space,
            Scope::Tab(s, t) => s == space && t == tab,
            Scope::Uid(u) => *u == uid,
            Scope::Agent(a) => agent == Some(a.as_str()),
            Scope::You => false,
            Scope::All => true,
        }
    }
}

impl Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Scope::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Ord: Allow < Ask < No, so max() is the stricter.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Ask,
    No,
}

impl Action {
    pub fn word(self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Ask => "ask",
            Action::No => "no",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Open,
    Strict,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub from: Scope,
    pub to: Scope,
    pub action: Action,
    /// Wall seconds; past it the route is ignored.
    pub until: Option<u64>,
}

impl Route {
    /// `from → to action`, as the UI and the log say it.
    pub fn said(&self) -> String {
        format!("{} → {} {}", self.from.text(), self.to.text(), self.action.word())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub version: u32,
    pub on: bool,
    pub mode: Mode,
    pub routes: Vec<Route>,
    pub accounts: Vec<Account>,
    pub allow_from_panes: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { version: 1, on: true, mode: Mode::Open, routes: Vec::new(), accounts: Vec::new(), allow_from_panes: false }
    }
}

pub struct Sender {
    pub name: String,
    pub space: String,
    pub tab: String,
    pub uid: u32,
    pub agent: Option<String>,
}

pub enum Recipient {
    You,
    Pane { name: String, space: String, tab: String, uid: u32, agent: Option<String> },
}

impl Recipient {
    fn name(&self) -> &str {
        match self {
            Recipient::You => "you",
            Recipient::Pane { name, .. } => name,
        }
    }
}

/// The names on the message a reply answers: its sender and its recipient.
pub struct ThreadFacts {
    pub first_from: String,
    pub first_to: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub action: Action,
    pub why: String,
    /// Index into `Policy::routes`.
    pub route: Option<usize>,
}

pub fn decide(p: &Policy, from: &Sender, to: &Recipient, thread: Option<&ThreadFacts>, now: u64) -> Verdict {
    let best = p
        .routes
        .iter()
        .enumerate()
        .filter(|(_, r)| r.until.is_none_or(|t| t > now) && r.from.holds_sender(from) && r.to.holds_recipient(to))
        .max_by_key(|(i, r)| (r.from.kind() + r.to.kind(), r.action, std::cmp::Reverse(*i)))
        .map(|(i, r)| Verdict { action: r.action, why: format!("route {}: {} → {} {}", i + 1, r.from.text(), r.to.text(), r.action.word()), route: Some(i) });
    let reply = thread.is_some_and(|t| t.first_from == to.name() && t.first_to == from.name);
    if reply && best.as_ref().is_none_or(|v| v.action != Action::No) {
        return Verdict { action: Action::Allow, why: format!("a reply in a thread {} started", to.name()), route: None };
    }
    if let Some(v) = best {
        return v;
    }
    let (action, why) = match (to, p.mode) {
        (Recipient::You, _) => (Action::Allow, "you always receive".to_string()),
        (_, Mode::Strict) => (Action::No, format!("no route {} → {}, strict mode", from.name, to.name())),
        (Recipient::Pane { uid, .. }, Mode::Open) if *uid == from.uid => (Action::Allow, "open mode".into()),
        (_, Mode::Open) => (Action::Ask, "open mode, another account: the owner is asked".into()),
    };
    Verdict { action, why, route: None }
}

/// 1–32 ASCII letters, digits, `.`, `_`, `-`; not starting with `-`, not all digits, not `you` or `all`.
pub fn valid_address(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !name.starts_with('-')
        && !name.bytes().all(|b| b.is_ascii_digit())
        && name != "you"
        && name != "all"
}

pub fn owner_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { nix::libc::geteuid() }
}

static POLICY: LazyLock<RwLock<Arc<Policy>>> = LazyLock::new(|| RwLock::new(Arc::new(Policy::default())));
static BROKEN: AtomicBool = AtomicBool::new(false);
/// policy.json's mtime as nerd last read or wrote it.
static SEEN: Mutex<Option<std::time::SystemTime>> = Mutex::new(None);

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// The one copy every thread reads; nothing rereads the file while the server runs.
pub fn current() -> Arc<Policy> {
    POLICY.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn replace(p: Policy) {
    *POLICY.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(p);
}

/// Server start only: reads `path()` once.
pub fn init() {
    let path = path();
    match load_from(&path) {
        Ok(Some(p)) => replace(p),
        Ok(None) => {}
        Err(e) => {
            replace(Policy { on: false, ..Default::default() });
            BROKEN.store(true, Ordering::SeqCst);
            crate::log::error(&format!("policy.json does not parse: {e}; messaging off until fixed in the Messages window"));
        }
    }
    *SEEN.lock().unwrap_or_else(|e| e.into_inner()) = mtime(&path);
}

/// policy.json did not parse at start, and no window click has replaced it.
pub fn broken() -> bool {
    BROKEN.load(Ordering::SeqCst)
}

pub fn path() -> PathBuf {
    crate::session::state_dir().join("policy.json")
}

/// Ok(None): no file.
pub fn load_from(path: &Path) -> Result<Option<Policy>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    match v.get("version").and_then(serde_json::Value::as_u64) {
        Some(1) => {}
        Some(n) => return Err(format!("version {n} is not one this build reads")),
        None => return Err("it names no version".into()),
    }
    serde_json::from_value(v).map(Some).map_err(|e| e.to_string())
}

/// To `path()`: an outside edit since nerd last saw the file is warned about and overwritten; a broken file is kept as `policy.json.bad`.
pub fn save(p: &Policy) -> std::io::Result<()> {
    let path = path();
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if mtime(&path) != *seen {
        crate::log::warn("policy.json changed outside nerd; ignored");
    }
    if broken() {
        std::fs::rename(&path, path.with_extension("json.bad"))?;
    }
    save_to(&path, p)?;
    BROKEN.store(false, Ordering::SeqCst);
    *seen = mtime(&path);
    Ok(())
}

pub fn save_to(path: &Path, p: &Policy) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::paths::write_private(path, "tmp", &serde_json::to_vec_pretty(p)?)
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// What a pane's programs may reach.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Isolation {
    pub files: Files,
    pub network: Network,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Files {
    #[default]
    Everything,
    Folder,
    /// An admitted account's name.
    Account(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Open,
    /// Entries read by `entry`: `"pi"` is every provider of pi, `"pi/deepseek"` only that one.
    Providers(Vec<String>),
    None,
}

/// A providers entry: the agent, and the one provider group it names, if any; None when malformed.
pub fn entry(s: &str) -> Option<(&str, Option<&str>)> {
    let tok = crate::agents::token;
    match s.split_once('/') {
        None => tok(s).then_some((s, None)),
        Some((a, g)) => (tok(a) && tok(g)).then_some((a, Some(g))),
    }
}

/// Per agent: every provider (None) or the groups named, sorted.
pub type Chosen = Vec<(String, Option<Vec<String>>)>;

/// Providers entries by agent in first-seen order, and the malformed ones; the agent alone beside some of its groups is the whole agent.
pub fn chosen(entries: &[String]) -> (Chosen, Vec<String>) {
    let (mut out, mut bad): (Chosen, Vec<String>) = (Vec::new(), Vec::new());
    for e in entries {
        let Some((a, g)) = entry(e) else {
            bad.push(e.clone());
            continue;
        };
        match (out.iter_mut().find(|(x, _)| x == a), g) {
            (Some((_, all)), None) => *all = None,
            (Some((_, Some(gs))), Some(g)) => gs.push(g.into()),
            (Some((_, None)), Some(_)) => {}
            (None, g) => out.push((a.into(), g.map(|g| vec![g.into()]))),
        }
    }
    for (_, gs) in &mut out {
        if let Some(gs) = gs {
            gs.sort();
            gs.dedup();
        }
    }
    (out, bad)
}

/// What is saved: `pi`, or `pi/<group>` per group, sorted, never both.
pub fn entries(chosen: &[(String, Option<Vec<String>>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (a, gs) in chosen {
        match gs {
            None => out.push(a.clone()),
            Some(gs) => {
                let mut gs = gs.clone();
                gs.sort();
                gs.dedup();
                out.extend(gs.iter().map(|g| format!("{a}/{g}")));
            }
        }
    }
    out
}

impl Isolation {
    pub fn is_open(&self) -> bool {
        self.files == Files::Everything && self.network == Network::Open
    }

    /// "" when open.
    pub fn words(&self) -> String {
        let files = match &self.files {
            Files::Everything => None,
            Files::Folder => Some("this folder only".to_string()),
            Files::Account(n) => Some(format!("as {n}")),
        };
        let network = match &self.network {
            Network::Open => None,
            Network::Providers(a) => {
                let (c, bad) = chosen(a);
                let said = c.iter().map(|(a, gs)| gs.as_ref().map_or_else(|| a.clone(), |gs| format!("{a} ({})", gs.join(", "))));
                Some(format!("only {}", said.chain(bad).collect::<Vec<_>>().join(", ")))
            }
            Network::None => Some("no network".to_string()),
        };
        files.into_iter().chain(network).collect::<Vec<_>>().join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, space: &str, tab: &str, uid: u32, agent: Option<&str>) -> (Sender, Recipient) {
        let s = Sender { name: name.into(), space: space.into(), tab: tab.into(), uid, agent: agent.map(Into::into) };
        let r = Recipient::Pane { name: name.into(), space: space.into(), tab: tab.into(), uid, agent: agent.map(Into::into) };
        (s, r)
    }

    fn route(from: &str, to: &str, action: Action) -> Route {
        Route { from: Scope::parse(from).unwrap(), to: Scope::parse(to).unwrap(), action, until: None }
    }

    fn policy(routes: Vec<Route>) -> Policy {
        Policy { routes, ..Default::default() }
    }

    #[test]
    fn the_most_specific_route_wins() {
        let (planner, _) = pane("planner", "work", "main", 501, Some("claude"));
        let (_, reader) = pane("reader", "research", "docs", 502, Some("pi"));
        // Each row: a broader route that says one thing, a narrower one that says another; the narrower wins.
        let rows = [
            ("*", "*", "planner", "*"),
            ("*", "*", "*", "reader"),
            ("*", "*", "space:work", "*"),
            ("*", "*", "*", "space:research"),
            ("*", "*", "*", "tab:research/docs"),
            ("*", "*", "uid:501", "*"),
            ("*", "*", "*", "uid:502"),
            ("*", "*", "agent:claude", "*"),
            ("*", "*", "*", "agent:pi"),
            ("space:work", "*", "planner", "*"),
            ("*", "space:research", "*", "reader"),
            ("agent:claude", "*", "planner", "space:research"),
            ("*", "agent:pi", "planner", "reader"),
        ];
        for (bf, bt, nf, nt) in rows {
            for (broad, narrow) in [(Action::No, Action::Allow), (Action::Allow, Action::No)] {
                let p = policy(vec![route(nf, nt, narrow), route(bf, bt, broad)]);
                let v = decide(&p, &planner, &reader, None, 0);
                assert_eq!((v.action, v.route), (narrow, Some(0)), "{nf} → {nt} over {bf} → {bt}: {}", v.why);
            }
        }
        let p = policy(vec![route("*", "*", Action::No), route("planner", "you", Action::Ask)]);
        assert_eq!(decide(&p, &planner, &Recipient::You, None, 0).action, Action::Ask, "you is as specific as a name");
    }

    #[test]
    fn a_tie_goes_to_the_stricter() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let (_, reader) = pane("reader", "work", "main", 501, None);
        for (a, b) in [(Action::Allow, Action::Ask), (Action::Ask, Action::No), (Action::No, Action::Allow)] {
            let p = policy(vec![route("planner", "*", a), route("*", "reader", b)]);
            assert_eq!(decide(&p, &planner, &reader, None, 0).action, a.max(b));
        }
    }

    #[test]
    fn open_mode_allows_the_same_account_and_asks_across() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let (_, mine) = pane("reader", "work", "main", 501, None);
        let (_, theirs) = pane("scout", "work", "main", 502, None);
        let p = Policy::default();
        assert_eq!(decide(&p, &planner, &mine, None, 0), Verdict { action: Action::Allow, why: "open mode".into(), route: None });
        assert_eq!(decide(&p, &planner, &theirs, None, 0), Verdict { action: Action::Ask, why: "open mode, another account: the owner is asked".into(), route: None });
    }

    #[test]
    fn strict_mode_refuses_with_the_route_named() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let (_, reader) = pane("reader", "research", "main", 501, None);
        let mut p = Policy { mode: Mode::Strict, ..Default::default() };
        assert_eq!(decide(&p, &planner, &reader, None, 0).why, "no route planner → reader, strict mode");
        assert_eq!(decide(&p, &planner, &reader, None, 0).action, Action::No);
        p.routes = vec![route("reader", "planner", Action::Allow), route("planner", "space:research", Action::Ask)];
        assert_eq!(decide(&p, &planner, &reader, None, 0), Verdict { action: Action::Ask, why: "route 2: planner → space:research ask".into(), route: Some(1) });
    }

    #[test]
    fn you_always_receive_unless_a_no_route_says_otherwise() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let p = Policy { mode: Mode::Strict, ..Default::default() };
        assert_eq!(decide(&p, &planner, &Recipient::You, None, 0).why, "you always receive");
        let p = policy(vec![route("planner", "you", Action::No)]);
        assert_eq!(decide(&p, &planner, &Recipient::You, None, 0).action, Action::No);
    }

    #[test]
    fn all_on_the_to_side_does_not_reach_you() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let p = policy(vec![route("*", "*", Action::No), route("planner", "*", Action::No)]);
        assert_eq!(decide(&p, &planner, &Recipient::You, None, 0).action, Action::Allow);
        assert!(!Scope::All.holds_recipient(&Recipient::You));
        assert!(!Scope::You.holds_sender(&planner), "you is never a sender");
    }

    #[test]
    fn a_reply_in_a_thread_the_recipient_started_passes_but_not_an_explicit_no() {
        let (reader, _) = pane("reader", "research", "main", 502, None);
        let (_, planner) = pane("planner", "work", "main", 501, None);
        let facts = ThreadFacts { first_from: "planner".into(), first_to: "reader".into() };
        let p = Policy { mode: Mode::Strict, routes: vec![route("*", "planner", Action::Ask)], ..Default::default() };
        assert_eq!(decide(&p, &reader, &planner, Some(&facts), 0), Verdict { action: Action::Allow, why: "a reply in a thread planner started".into(), route: None });
        let p = policy(vec![route("reader", "planner", Action::No)]);
        assert_eq!(decide(&p, &reader, &planner, Some(&facts), 0).action, Action::No);
        let other = ThreadFacts { first_from: "scout".into(), first_to: "reader".into() };
        assert_eq!(decide(&Policy { mode: Mode::Strict, ..Default::default() }, &reader, &planner, Some(&other), 0).action, Action::No, "someone else's thread");
    }

    #[test]
    fn an_expired_route_is_ignored() {
        let (planner, _) = pane("planner", "work", "main", 501, None);
        let (_, reader) = pane("reader", "work", "main", 501, None);
        let p = policy(vec![Route { until: Some(100), ..route("planner", "reader", Action::No) }]);
        assert_eq!(decide(&p, &planner, &reader, None, 99).action, Action::No);
        assert_eq!(decide(&p, &planner, &reader, None, 100).action, Action::Allow);
    }

    #[test]
    fn an_agent_scope_never_matches_a_shell() {
        let (shell, _) = pane("planner", "work", "main", 501, None);
        let (_, reader) = pane("reader", "work", "main", 501, None);
        let p = policy(vec![route("agent:claude", "*", Action::No)]);
        assert_eq!(decide(&p, &shell, &reader, None, 0).action, Action::Allow);
    }

    #[test]
    fn scopes_round_trip_as_text() {
        for s in ["planner", "space:my project", "tab:a/b/c", "uid:502", "agent:pi", "you", "*"] {
            let scope = Scope::parse(s).unwrap();
            assert_eq!(scope.text(), s);
            assert_eq!(serde_json::from_str::<Scope>(&serde_json::to_string(&scope).unwrap()).unwrap(), scope);
        }
        assert_eq!(Scope::parse("tab:a/b/c").unwrap(), Scope::Tab("a".into(), "b/c".into()), "split at the first /");
        for bad in ["", "space:", "tab:a", "uid:x", "agent:", "-x", "12"] {
            assert!(Scope::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn addresses_have_one_shape() {
        for good in ["a", "planner", "re.ad_er-2", "9a", &"x".repeat(32)] {
            assert!(valid_address(good), "{good}");
        }
        for bad in ["", &"x".repeat(33), "-a", "123", "you", "all", "a b", "é", "a/b"] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    fn sample() -> Policy {
        Policy { routes: vec![route("planner", "space:research", Action::Ask)], accounts: vec![Account { name: "scout".into(), uid: 502 }], ..Default::default() }
    }

    #[test]
    fn the_file_round_trips() {
        let path = crate::paths::test_dir("policy-rt").join("policy.json");
        save_to(&path, &sample()).unwrap();
        assert_eq!(load_from(&path), Ok(Some(sample())));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let text = r#"{"version":1,"on":true,"mode":"open","routes":[{"from":"planner","to":"space:research","action":"ask","until":null}],"accounts":[{"name":"scout","uid":502}],"allow_from_panes":false}"#;
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_from(&path), Ok(Some(sample())));
        assert_eq!(load_from(&path.with_extension("none")), Ok(None));
    }

    #[test]
    fn an_unknown_version_does_not_load() {
        let path = crate::paths::test_dir("policy-v").join("policy.json");
        std::fs::write(&path, r#"{"version":2,"on":true,"mode":"open","routes":[],"accounts":[],"allow_from_panes":false}"#).unwrap();
        assert_eq!(load_from(&path), Err("version 2 is not one this build reads".into()));
    }

    /// Runs `f` with a state folder of its own, so `path()` is a file of its own.
    fn in_home(tag: &str, f: impl FnOnce(PathBuf)) {
        crate::session::tests::with_temp_state_dir(tag, || f(path()));
        replace(Policy::default());
        BROKEN.store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_file_that_does_not_parse_leaves_messaging_off_and_untouched() {
        let _l = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        in_home("policy-bad", |path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"{not json").unwrap();
            init();
            assert!(!current().on);
            assert!(broken());
            assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
            save(&Policy::default()).unwrap();
            assert!(!broken());
            assert_eq!(std::fs::read(path.with_extension("json.bad")).unwrap(), b"{not json");
            assert_eq!(load_from(&path), Ok(Some(Policy::default())));
        });
    }

    #[test]
    fn an_outside_edit_is_warned_and_overwritten() {
        let _l = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        in_home("policy-edit", |path| {
            init();
            save(&Policy::default()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            save_to(&path, &sample()).unwrap();
            let edited = mtime(&path);
            assert_ne!(edited, *SEEN.lock().unwrap(), "the outside edit is seen as one");
            save(&Policy { on: false, ..Default::default() }).unwrap();
            assert!(!load_from(&path).unwrap().unwrap().on, "nerd's copy wins");
            assert_eq!(mtime(&path), *SEEN.lock().unwrap());
        });
    }

    #[test]
    fn a_swapped_policy_is_seen_by_another_thread() {
        let _l = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        replace(sample());
        let seen = std::thread::spawn(|| current().accounts.clone()).join().unwrap();
        replace(Policy::default());
        assert_eq!(seen, sample().accounts);
    }

    #[test]
    fn isolation_says_what_it_closes() {
        assert_eq!(Isolation::default().words(), "");
        assert!(Isolation::default().is_open());
        let i = Isolation { files: Files::Account("scout".into()), network: Network::None };
        assert_eq!(i.words(), "as scout, no network");
        assert_eq!(Isolation { files: Files::Folder, network: Network::Providers(vec!["claude".into(), "gemini".into()]) }.words(), "this folder only, only claude, gemini");
        let only = |e: &[&str]| Isolation { files: Files::Everything, network: Network::Providers(e.iter().map(|s| s.to_string()).collect()) }.words();
        assert_eq!(only(&["pi/deepseek"]), "only pi (deepseek)");
        assert_eq!(only(&["pi"]), "only pi");
        assert_eq!(only(&["claude", "pi/openai", "pi/deepseek"]), "only claude, pi (deepseek, openai)");
        assert_eq!(only(&["pi/deepseek", "pi"]), "only pi", "the agent beside its groups is the whole agent");
    }

    #[test]
    fn a_providers_entry_is_an_agent_or_an_agent_and_one_group() {
        for (s, want) in [("pi", Some(("pi", None))), ("pi/deepseek", Some(("pi", Some("deepseek")))), ("open-code2/x-1", Some(("open-code2", Some("x-1"))))] {
            assert_eq!(entry(s), want, "{s}");
        }
        for bad in ["", "pi/", "/x", "pi/x/y", "Pi", "pi/DeepSeek", "pi deepseek", "pi//x", &"x".repeat(65)] {
            assert_eq!(entry(bad), None, "{bad:?}");
        }
        let e = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (c, bad) = chosen(&e(&["pi/openai", "claude", "pi/deepseek", "pi/openai", "Bad", "codex/x", "codex"]));
        assert_eq!(c, vec![("pi".to_string(), Some(vec!["deepseek".to_string(), "openai".into()])), ("claude".into(), None), ("codex".into(), None)]);
        assert_eq!(bad, ["Bad"]);
        assert_eq!(entries(&c), ["pi/deepseek", "pi/openai", "claude", "codex"], "normal form: sorted groups, no duplicates, never the agent beside them");
    }
}
