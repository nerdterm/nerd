//! A new release of nerd, asked of GitHub on the person's word, or once a day after they allowed it. It runs outside
//! the server's sandbox, through the broker or from a terminal ("What leaves nerd", `egress:release-check`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

const API: &str = "https://api.github.com/repos/nerdterm/nerd/releases/latest";
const DOWNLOADS: &str = "https://github.com/nerdterm/nerd/releases/download";
const INSTALL_SH: &str = include_str!("../install.sh");
pub const DAY: u64 = 86400;

/// What a check found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub tag: String,
    /// The release's changelog as plain lines.
    pub notes: Vec<String>,
    pub newer: bool,
    /// Why nerd cannot install it here, said as what to do instead.
    pub blocked: Option<String>,
}

/// What `nerd --check-update --window` leaves for the server that ran it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Said {
    Found(Found),
    Installed(String),
    Failed(String),
}

/// `nerd --check-update`; `--window` and `--install` are the server's own runs through the broker.
pub fn main(args: &[String]) -> std::io::Result<()> {
    let has = |f: &str| args.iter().any(|a| a == f);
    if args.iter().any(|a| a != "--window" && a != "--install") || (has("--install") && !has("--window")) {
        return Err(std::io::Error::other("usage: nerd --check-update"));
    }
    if has("--window") {
        return leave(&window(has("--install")));
    }
    terminal()
}

fn terminal() -> std::io::Result<()> {
    let f = check().map_err(std::io::Error::other)?;
    println!("Running: {}\nNewest:  nerd {}", crate::version_line(), f.tag.trim_start_matches('v'));
    if !f.newer {
        println!("Nothing newer to install.");
        return Ok(());
    }
    println!();
    f.notes.iter().for_each(|l| println!("{l}"));
    println!();
    if let Some(b) = f.blocked {
        println!("{b}");
        return Ok(());
    }
    if crate::install::in_pane() {
        println!("From a pane nerd only checks: install it with Check for a new release… in nerd's menu.");
        return Ok(());
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!("No terminal to answer in: nothing installed. Run nerd --check-update in a terminal, or use Check for a new release… in nerd's menu.");
        return Ok(());
    }
    if !crate::install::ask_terminal(&format!("Install nerd {}?", f.tag.trim_start_matches('v'))) {
        println!("Nothing installed.");
        return Ok(());
    }
    let v = install(&f.tag).map_err(std::io::Error::other)?;
    println!("Installed {v}. Running servers offer Update available.");
    Ok(())
}

/// The server's run: a check, and with `--install` the release its window's button was pressed for.
fn window(install_too: bool) -> Said {
    // Taken first: a tag left by a click is good for this one run only.
    let go = if install_too { take(&go_path()) } else { None };
    let f = match check() {
        Ok(f) => f,
        Err(e) => return Said::Failed(e),
    };
    match go {
        _ if !install_too || !f.newer => Said::Found(f),
        None => Said::Failed("refused: a release is installed only from a button in nerd's window".into()),
        Some(t) if t != f.tag => Said::Failed(format!("the newest release is now {}: check again", f.tag)),
        Some(_) => install(&f.tag).map_or_else(Said::Failed, Said::Installed),
    }
}

/// One question to GitHub, and one log line either way.
fn check() -> Result<Found, String> {
    let running = env!("CARGO_PKG_VERSION");
    let got = in_fresh_dir(|dir| {
        let file = dir.join("release.json");
        curl(API, &file, 30)?;
        parse(&std::fs::read(&file).map_err(|e| e.to_string())?)
    });
    match got {
        Ok((tag, body)) => {
            let f = Found { newer: newer(&tag, running), blocked: blocked_here(), notes: plain(&body), tag };
            crate::log::info(&format!("release check: newest {}, running {running}, {}", f.tag, if f.newer { "newer" } else { "nothing newer" }));
            Ok(f)
        }
        Err(e) => {
            crate::log::warn(&format!("release check failed: {e}"));
            Err(e)
        }
    }
}

/// The release `tag` into ~/.local/bin, through the embedded install.sh, which refuses an archive whose checksum differs.
/// ceiling: the checksum comes from the same release, so it proves the download intact, not who made it; signed releases are the way past.
fn install(tag: &str) -> Result<String, String> {
    if let Some(b) = blocked_here() {
        return Err(b);
    }
    let home = crate::paths::home().ok_or("no home folder")?;
    let name = format!("nerd-{tag}-macos-arm64.tar.gz");
    let done = in_fresh_dir(|dir| {
        for n in [name.clone(), format!("{name}.sha256")] {
            curl(&asset_url(tag, &n), &dir.join(&n), 300)?;
        }
        install_sh(&dir.join(&name), &home)
    });
    match &done {
        Ok(v) => crate::log::info(&format!("release {tag} installed in ~/.local/bin: {v}")),
        Err(e) => crate::log::warn(&format!("release {tag} not installed: {e}")),
    }
    done
}

/// With the archive given, install.sh reaches no network: it checks, unpacks, signs and renames into place.
fn install_sh(archive: &Path, home: &Path) -> Result<String, String> {
    use std::io::Write as _;
    let bindir = home.join(".local/bin");
    let mut cmd = crate::spawn::cmd("/bin/sh");
    cmd.arg("-s").arg("--").arg(archive);
    // The bindir on PATH, so install.sh leaves ~/.zshrc alone; last, so no tool of the user's stands in for the system's.
    cmd.env_remove("NERD_INSTALL_UNVERIFIED").env_remove("NERD_RELEASE_API").env("HOME", home).env("NERD_BINDIR", &bindir);
    cmd.env("PATH", format!("/usr/bin:/bin:/usr/sbin:/sbin:{}", bindir.display()));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("sh: {e}"))?;
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(INSTALL_SH.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("sh: {e}"))?;
    let last = |b: &[u8]| {
        let t = String::from_utf8_lossy(b);
        let l = t.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
        crate::render::unspoofed(l.strip_prefix("nerd: ").unwrap_or(&l))
    };
    if !out.status.success() {
        return Err(match last(&out.stderr) {
            l if l.is_empty() => "install.sh failed".into(),
            l => l,
        });
    }
    let said = String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("nerd: installed ").map(str::to_string));
    Ok(crate::render::unspoofed(&said.unwrap_or_default()))
}

/// A folder of its own per run, gone after it: two servers may check at once.
fn in_fresh_dir<T>(f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = crate::paths::state_root().join(format!("release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let r = f(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn curl_exe() -> PathBuf {
    // Debug builds only: the tests' stand-in, a script that writes canned files and reaches nothing.
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("NERD_TEST_CURL") {
        return p.into();
    }
    PathBuf::from("/usr/bin/curl")
}

/// `-q` first, or curl reads `.curlrc`; https only, redirects included; never a proxy.
fn curl_args(url: &str, out: &Path, max_secs: u32) -> Vec<String> {
    let mut a: Vec<String> = ["-q", "--proto", "=https", "--proto-redir", "=https", "--location", "--max-redirs", "5", "--noproxy", "*"].map(String::from).into();
    a.extend(["--connect-timeout".into(), "10".into(), "--max-time".into(), max_secs.to_string()]);
    a.extend(["--silent", "--show-error", "--write-out", "%{http_code}", "--output"].map(String::from));
    a.extend([out.to_string_lossy().into_owned(), "--".into(), url.into()]);
    a
}

/// What curl would read from the environment: a proxy, its own settings, other certificates, a TLS key log.
fn curl_drops(k: &str) -> bool {
    k.to_ascii_lowercase().ends_with("_proxy") || k.starts_with("CURL_") || ["SSL_CERT_FILE", "SSL_CERT_DIR", "SSLKEYLOGFILE", "XDG_CONFIG_HOME"].contains(&k)
}

/// One GET into `out`; a refusal by GitHub is told apart from no network.
fn curl(url: &str, out: &Path, max_secs: u32) -> Result<(), String> {
    let mut cmd = crate::spawn::cmd(curl_exe());
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if curl_drops(&k) {
            cmd.env_remove(k.as_ref());
        }
    }
    cmd.args(curl_args(url, out, max_secs)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let o = cmd.output().map_err(|e| format!("curl did not start: {e}"))?;
    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
    match (o.status.success(), code.as_str()) {
        (true, "200") => Ok(()),
        (_, "403" | "429") => Err("GitHub refused the request (rate limit): try again in an hour".into()),
        (true, c) => Err(format!("GitHub answered HTTP {}", crate::render::unspoofed(c))),
        (false, _) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let why = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("no answer");
            Err(format!("could not reach GitHub: {}", crate::render::unspoofed(why.trim_start_matches("curl: "))))
        }
    }
}

/// A release tag newer than the running build; a pre-release never is.
fn newer(tag: &str, running: &str) -> bool {
    let (Some(t), Some(r)) = (tag.strip_prefix('v').and_then(crate::agents::semver), crate::agents::semver(running)) else { return false };
    t.1.is_none() && t.0 > r.0
}

fn blocked_here() -> Option<String> {
    let exe = std::env::current_exe().unwrap_or_default();
    match crate::paths::home() {
        Some(home) => blocked(&exe, &home),
        None => Some("no home folder, so no ~/.local/bin to install in".into()),
    }
}

/// None when nerd may replace itself: it runs as the file `~/.local/bin/nerd`, where install.sh puts it.
fn blocked(exe: &Path, home: &Path) -> Option<String> {
    let real = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    if real.components().any(|c| c.as_os_str() == "Cellar") {
        return Some("nerd was installed with Homebrew: run brew upgrade nerd".into());
    }
    let ours = home.join(".local/bin/nerd");
    let file = std::fs::symlink_metadata(&ours).is_ok_and(|m| m.file_type().is_file());
    if file && std::fs::canonicalize(&ours).is_ok_and(|p| p == real) {
        return None;
    }
    Some(format!("nerd runs from {}; only a nerd in ~/.local/bin installs a release itself", crate::paths::shown(exe)))
}

/// The tag and changelog of GitHub's answer, when the release carries the macOS archive and its checksum.
fn parse(json: &[u8]) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    struct Asset {
        name: String,
    }
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        assets: Vec<Asset>,
    }
    let r: Release = serde_json::from_slice(json).map_err(|_| "GitHub's answer did not parse".to_string())?;
    let tag = crate::render::unspoofed(&r.tag_name);
    if !r.tag_name.strip_prefix('v').is_some_and(|v| crate::agents::semver(v).is_some()) {
        return Err(format!("the newest release's tag {tag:?} is not a version"));
    }
    let archive = format!("nerd-{tag}-macos-arm64.tar.gz");
    if let Some(n) = [archive.clone(), format!("{archive}.sha256")].into_iter().find(|n| !r.assets.iter().any(|a| a.name == *n)) {
        return Err(format!("release {tag} has no {n} yet"));
    }
    Ok((tag, r.body.unwrap_or_default()))
}

/// Built from the tag, never taken from the answer: the download stays on GitHub's own release path.
fn asset_url(tag: &str, name: &str) -> String {
    format!("{DOWNLOADS}/{tag}/{name}")
}

/// Markdown links and headings as plain text, wrapped for a window.
fn plain(body: &str) -> Vec<String> {
    const WIDTH: usize = 72;
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let l = unlink(&crate::render::unspoofed(line.trim()));
        let l = l.trim_start_matches('#').trim_start();
        if l.is_empty() {
            if out.last().is_some_and(|x| !x.is_empty()) {
                out.push(String::new());
            }
            continue;
        }
        match l.strip_prefix("* ").or_else(|| l.strip_prefix("- ")) {
            Some(item) => out.extend(crate::menu::wrap(item, WIDTH - 2).into_iter().enumerate().map(|(i, w)| format!("{}{w}", if i == 0 { "- " } else { "  " }))),
            None => out.extend(crate::menu::wrap(l, WIDTH)),
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// `[text](url)` as `text`.
fn unlink(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|m| after[m + 2..].find(')').map(|c| (m, m + 2 + c))) {
            Some((m, close)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..m]);
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out + rest
}

/// Next to config.json, so once per Mac: every server on it shares the answer and the time of the last check.
pub fn check_path() -> PathBuf {
    crate::paths::config_path().with_file_name("update-check")
}

fn checked_path() -> PathBuf {
    crate::paths::config_path().with_file_name("update-checked")
}

/// The server's own folder, which its broker's run shares through `NERD_INSTANCE`: two servers never take each other's.
fn said_path() -> PathBuf {
    crate::session::state_dir().join("release.json")
}

fn go_path() -> PathBuf {
    crate::session::state_dir().join("release-install")
}

fn skipped_path() -> PathBuf {
    crate::paths::state_root().join("release-skipped")
}

/// The per-Mac answer to "check daily?": `None` until it was given; anything but `daily` reads as off.
pub fn daily() -> Option<bool> {
    match std::fs::read_to_string(check_path()) {
        Ok(s) => Some(s.trim() == "daily"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(false),
    }
}

/// Written only from nerd's own window on this Mac; it stops a window in a pane, not a program of the same user
/// writing the file itself.
pub fn set_daily(on: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::paths::config_path().parent().expect("config_path always has a parent"))?;
    crate::paths::write_private(&check_path(), "tmp", if on { b"daily\n" } else { b"off\n" })
}

/// A daily check is due when none ran in the last day, or the clock went back.
fn due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|t| now >= t + DAY || t > now)
}

/// Allowed and due; marked as run at once, so the other servers on this Mac wait a day.
pub fn daily_due(now: u64) -> bool {
    let last = std::fs::read_to_string(checked_path()).ok().and_then(|s| s.trim().parse().ok());
    daily() == Some(true) && due(last, now) && std::fs::write(checked_path(), now.to_string()).is_ok()
}

pub fn leave(said: &Said) -> std::io::Result<()> {
    crate::paths::write_private(&said_path(), "json.tmp", &serde_json::to_vec(said).map_err(std::io::Error::other)?)
}

/// What the server's last run left, taken so it is read once.
pub fn take_said() -> Said {
    take(&said_path()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| Said::Failed("the check ended without an answer; nerd's log says why".into()))
}

/// The tag the window's install button was pressed for; the next `--install --window` run takes it.
pub fn allow_install(tag: &str) -> std::io::Result<()> {
    crate::paths::write_private(&go_path(), "tmp", tag.as_bytes())
}

pub fn skipped() -> Option<String> {
    std::fs::read_to_string(skipped_path()).ok().map(|s| s.trim().to_string())
}

pub fn skip(tag: &str) -> std::io::Result<()> {
    std::fs::write(skipped_path(), tag)
}

fn take(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok();
    let _ = std::fs::remove_file(p);
    s.map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_reads_no_curlrc_speaks_only_https_and_takes_no_proxy() {
        let a = curl_args(API, Path::new("/x/out"), 30);
        assert_eq!(a[0], "-q", "-q only works first");
        let pair = |k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(pair("--proto", "=https") && pair("--proto-redir", "=https"), "{a:?}");
        assert!(pair("--noproxy", "*") && pair("--max-time", "30") && a.iter().any(|x| x == "--connect-timeout"), "{a:?}");
        assert!(pair("--output", "/x/out"), "{a:?}");
        assert_eq!(a.last().map(String::as_str), Some(API), "the url is last, after `--`");
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(API, "https://api.github.com/repos/nerdterm/nerd/releases/latest", "no override in any build");
        for k in ["https_proxy", "ALL_PROXY", "CURL_HOME", "SSL_CERT_FILE", "SSL_CERT_DIR", "XDG_CONFIG_HOME", "SSLKEYLOGFILE"] {
            assert!(curl_drops(k), "{k}");
        }
        assert!(!curl_drops("HOME") && !curl_drops("PATH"));
    }

    #[test]
    fn only_a_strictly_newer_release_is_offered() {
        assert!(newer("v0.13.0", "0.12.99"));
        assert!(newer("v0.13.1", "0.13.0"));
        assert!(!newer("v0.13.0", "0.13.0"), "the same");
        assert!(!newer("v0.12.4", "0.13.0"), "a local build ahead of the release is never offered an older one");
        assert!(!newer("v0.14.0-rc.1", "0.13.0"), "not a pre-release");
        assert!(!newer("0.14.0", "0.13.0"), "a release tag starts with v");
        assert!(!newer("v0.14", "0.13.0"));
    }

    #[test]
    fn only_a_nerd_running_from_local_bin_replaces_itself() {
        let home = crate::paths::test_dir("release-home");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("nerd"), "x").unwrap();
        assert_eq!(blocked(&bin.join("nerd"), &home), None);

        let cellar = home.join("homebrew/Cellar/nerd/0.13.0/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::write(cellar.join("nerd"), "x").unwrap();
        let link = home.join("homebrew/bin");
        std::fs::create_dir_all(&link).unwrap();
        std::os::unix::fs::symlink(cellar.join("nerd"), link.join("nerd")).unwrap();
        assert!(blocked(&link.join("nerd"), &home).is_some_and(|b| b.contains("brew upgrade nerd")), "a Homebrew link");

        let dev = home.join("dev/nerd/target/release");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("nerd"), "x").unwrap();
        assert!(blocked(&dev.join("nerd"), &home).is_some_and(|b| b.contains("target/release/nerd")), "another path is named");

        std::fs::remove_file(bin.join("nerd")).unwrap();
        std::os::unix::fs::symlink(dev.join("nerd"), bin.join("nerd")).unwrap();
        assert!(blocked(&bin.join("nerd"), &home).is_some(), "a link in ~/.local/bin is somebody's own setup, never replaced");
    }

    #[test]
    fn the_answer_needs_the_archive_and_its_checksum() {
        let json = |assets: &[&str]| {
            let a: Vec<_> = assets.iter().map(|n| serde_json::json!({ "name": n, "browser_download_url": format!("https://evil.example/{n}") })).collect();
            serde_json::json!({ "tag_name": "v0.13.0", "body": "## x", "assets": a }).to_string()
        };
        let both = ["nerd-v0.13.0-macos-arm64.tar.gz", "nerd-v0.13.0-macos-arm64.tar.gz.sha256"];
        assert_eq!(parse(json(&both).as_bytes()), Ok(("v0.13.0".into(), "## x".into())));
        assert!(parse(json(&both[..1]).as_bytes()).is_err(), "no checksum, nothing to offer");
        assert!(parse(b"not json").is_err());
        let odd = serde_json::json!({ "tag_name": "v0.13.0/../x", "assets": [] }).to_string();
        assert!(parse(odd.as_bytes()).is_err(), "a tag that is not a version");
        assert_eq!(asset_url("v0.13.0", both[0]), "https://github.com/nerdterm/nerd/releases/download/v0.13.0/nerd-v0.13.0-macos-arm64.tar.gz", "built from the tag, never the answer's own url");
    }

    #[test]
    fn the_changelog_reads_as_plain_lines() {
        let body = "## [0.13.0](https://github.com/x/compare/a...b) (2026-10-20)\r\n\r\n\n### Added\n\n* Control + B is nerd's key ([#83](https://github.com/x/83)) ([850c2c1](https://github.com/x/c))\n";
        assert_eq!(plain(body), ["0.13.0 (2026-10-20)", "", "Added", "", "- Control + B is nerd's key (#83) (850c2c1)"]);
        assert!(plain(&format!("* {}", "word ".repeat(40))).iter().all(|l| l.chars().count() <= 72), "wrapped");
    }

    #[test]
    fn the_daily_check_runs_once_a_day() {
        assert!(due(None, 1000));
        assert!(!due(Some(1000), 1000 + DAY - 1));
        assert!(due(Some(1000), 1000 + DAY));
        assert!(due(Some(1000 + 2 * DAY), 1000), "a clock that went back does not stop it for days");
    }

    #[test]
    fn the_answer_file_holds_off_or_daily_and_nothing_else_counts_as_asked() {
        let home = crate::paths::test_dir("release-daily");
        let old = crate::paths::TEST_HOME.with(|h| h.replace(Some(home.clone())));
        assert_eq!(daily(), None, "not asked yet");
        std::fs::create_dir_all(crate::paths::config_path().parent().unwrap()).unwrap();
        for (text, want) in [("daily\n", Some(true)), ("off", Some(false)), ("weekly", Some(false)), ("", Some(false))] {
            std::fs::write(check_path(), text).unwrap();
            assert_eq!(daily(), want, "{text:?}");
        }
        set_daily(true).unwrap();
        assert_eq!(daily(), Some(true));
        crate::paths::TEST_HOME.with(|h| h.replace(old));
    }
}
