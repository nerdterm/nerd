//! A nerd that Homebrew installed from nerdterm/tap, upgraded on the person's click: nerd fetches and checks the release
//! as for ~/.local/bin, writes its own formula into the tap, has Homebrew install from a primed cache with no network,
//! and puts the tap's formula back ("What leaves nerd", `egress:release-check`).

use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PUBLISH_FORMULA: &str = include_str!("../../scripts/publish-formula.sh");
/// No network for brew guards against Homebrew's own habits; what it installs, the signed archive and nerd's formula,
/// is the boundary. Homebrew binds a unix socket per build, and its own build sandbox starts outside this one.
const PROFILE: &str = r#"(version 1)(allow default)(deny network*)(allow network-bind network-inbound (local unix-socket))(allow process-exec (literal "/usr/bin/sandbox-exec") (with no-sandbox))"#;
const REFUSED: &str = "nerd's Homebrew tap is not as nerd expects: run brew update && brew upgrade nerd";
const STATUS: &[&str] = &["status", "--porcelain", "-z", "--untracked-files=all", "--ignored"];
pub(super) const BREW_LIMIT: Duration = Duration::from_secs(300);
const STEP_LIMIT: Duration = Duration::from_secs(30);
/// Between TERM to brew's group and KILL to its tree: Homebrew's rescue removes a half-built keg.
const GRACE: Duration = Duration::from_secs(5);

pub(super) struct Brew {
    prefix: PathBuf,
    /// /usr/bin/sandbox-exec, which runs every brew.
    run: PathBuf,
    /// nerd's state folder: the lock and the tap's saved formula.
    state: PathBuf,
}

impl Brew {
    /// The Homebrew that installed this nerd from nerdterm/tap: the file `<prefix>/Cellar/nerd/<v>/bin/nerd`, the file
    /// `<prefix>/bin/brew`, the keg's receipt naming the tap, and the tap's folder.
    pub(super) fn of(exe: &Path) -> Option<Brew> {
        let real = std::fs::canonicalize(exe).ok()?;
        let up: Vec<&Path> = real.ancestors().take(6).collect();
        let named = |i: usize, n: &str| up.get(i).and_then(|p| p.file_name()).is_some_and(|f| f == n);
        if up.len() < 6 || !(named(0, "nerd") && named(1, "bin") && named(3, "nerd") && named(4, "Cellar")) {
            return None;
        }
        let (keg, prefix) = (up[2], up[5]);
        let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(keg.join("INSTALL_RECEIPT.json")).ok()?).ok()?;
        let brew = std::fs::symlink_metadata(prefix.join("bin/brew")).is_ok_and(|m| m.file_type().is_file());
        let tap = std::fs::symlink_metadata(tap_of(prefix)).is_ok_and(|m| m.is_dir());
        (receipt["source"]["tap"] == "nerdterm/tap" && brew && tap).then(|| Brew { prefix: prefix.to_path_buf(), run: sandbox_exe(), state: crate::paths::state_root() })
    }

    /// Every brew nerd runs: under the profile, with only `brew_env` and no stdin.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = crate::spawn::cmd(&self.run);
        cmd.arg("-p").arg(PROFILE).arg(self.prefix.join("bin/brew")).args(args);
        cmd.env_clear().envs(brew_env(&self.prefix)).stdin(Stdio::null());
        cmd
    }

    /// One brew run's output, or brew's own last line when it failed.
    fn step(&self, args: &[&str], limit: Duration, dir: &Path) -> Result<String, String> {
        match run(self.cmd(args), limit, dir)? {
            (true, out, _) => Ok(out),
            (false, out, err) => Err(last_line(&err).or_else(|| last_line(&out)).map_or_else(|| "Homebrew gave no reason".into(), |l| format!("Homebrew said \"{l}\""))),
        }
    }

    /// The release checked in `dir` installed by this Homebrew with nerd's own formula; the tap's formula back after.
    pub(super) fn upgrade(&self, tag: &str, name: &str, dir: &Path, limit: Duration) -> Result<String, String> {
        self.steps(tag, name, dir, limit).map_err(|(step, why)| format!("failed at {step}: {why}"))
    }

    fn steps(&self, tag: &str, name: &str, dir: &Path, limit: Duration) -> Result<String, (&'static str, String)> {
        let at = |step: &'static str| move |why: String| (step, why);
        let _lock = lock(&self.state).map_err(at("the lock"))?;
        let tap = tap_of(&self.prefix);
        recover(&tap, &self.state, tag, dir).map_err(at("recovery"))?;
        let version = self.step(&["--version"], STEP_LIMIT, dir).map_err(at("brew --version"))?;
        crate::log::info(&format!("release {tag}: {}", crate::render::unspoofed(version.lines().next().unwrap_or("").trim())));
        check_tap(&tap, dir).map_err(at("tap check"))?;
        crate::log::info(&format!("release {tag}: tap checked"));
        let formula = formula(tag, name, &checked_sha(dir, name).map_err(at("checksum"))?);
        let root = PathBuf::from(self.step(&["--cache"], STEP_LIMIT, dir).map_err(at("brew --cache"))?.lines().next().unwrap_or("").trim());
        if !root.is_absolute() {
            return Err(("brew --cache", format!("not a folder: {}", crate::render::unspoofed(&root.to_string_lossy()))));
        }
        let primed = prime(&root, &super::asset_url(tag, name), &dir.join(name)).map_err(at("Homebrew's cache"))?;
        let orig = put(&tap, &self.state, &formula).inspect_err(|_| drop(std::fs::remove_file(&primed))).map_err(at("formula"))?;
        crate::log::info(&format!("release {tag}: formula written"));
        let done = self.step(&["upgrade", "nerdterm/tap/nerd"], limit, dir);
        restore(&tap, &self.state, tag, &formula, &orig, dir);
        match done {
            Ok(_) => {
                crate::log::info(&format!("release {tag}: brew upgrade done"));
                Ok(format!("nerd {}", tag.trim_start_matches('v')))
            }
            Err(why) if self.prefix.join("bin/nerd").exists() => Err(("brew upgrade", format!("{why}; run brew update && brew upgrade nerd"))),
            Err(why) => Err(("brew upgrade", format!("{why}; nerd is no longer linked into {}: run brew link nerd, then brew update && brew upgrade nerd", self.prefix.join("bin").display()))),
        }
    }
}

fn tap_of(prefix: &Path) -> PathBuf {
    prefix.join("Library/Taps/nerdterm/homebrew-tap")
}

fn sandbox_exe() -> PathBuf {
    // Debug builds only: the tests' stand-in, which runs their stand-in brew and reaches nothing.
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("NERD_TEST_BREW") {
        return p.into();
    }
    PathBuf::from("/usr/bin/sandbox-exec")
}

/// The home folder, the user, this Homebrew first on PATH, and Homebrew told to update, report and remove nothing.
fn brew_env(prefix: &Path) -> Vec<(String, std::ffi::OsString)> {
    let mut env: Vec<(String, std::ffi::OsString)> = ["USER", "LOGNAME", "TMPDIR"].iter().filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v))).collect();
    env.push(("HOME".into(), crate::paths::home().unwrap_or_default().into()));
    env.push(("PATH".into(), format!("{}/bin:/usr/bin:/bin:/usr/sbin:/sbin", prefix.display()).into()));
    env.push(("LANG".into(), "en_US.UTF-8".into()));
    for k in ["HOMEBREW_NO_AUTO_UPDATE", "HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_INSTALL_CLEANUP", "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK", "HOMEBREW_NO_ENV_HINTS", "HOMEBREW_NO_AUTOREMOVE"] {
        env.push((k.into(), "1".into()));
    }
    env.push(("HOMEBREW_CURL_RETRIES".into(), "0".into()));
    env
}

/// `cmd` in a process group of its own, its output in files in `dir`; past `limit`, TERM to that group, then KILL to
/// the group and every process still under it. `Ok((succeeded, stdout, stderr))`.
fn run(mut cmd: Command, limit: Duration, dir: &Path) -> Result<(bool, String, String), String> {
    let (o, e) = (dir.join("out"), dir.join("err"));
    let file = |p: &Path| std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()));
    cmd.stdout(file(&o)?).stderr(file(&e)?).process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("did not start: {e}"))?;
    let start = Instant::now();
    loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            let read = |p: &Path| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned();
            return Ok((s.success(), read(&o), read(&e)));
        }
        if start.elapsed() >= limit {
            stop(&mut child);
            return Err(format!("did not finish in {} s and was stopped; a build Homebrew started may still be running", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop(child: &mut std::process::Child) {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    let group = Pid::from_raw(child.id() as i32);
    let _ = killpg(group, Signal::SIGTERM);
    let t = Instant::now();
    let mut gone = false;
    while !gone && t.elapsed() < GRACE {
        std::thread::sleep(Duration::from_millis(50));
        gone = child.try_wait().ok().flatten().is_some();
    }
    // Only while it is unreaped is its pid still its own: what left its group is found as its descendants.
    if !gone {
        let tree = crate::transport::descendants(group.as_raw(), 4096);
        let _ = kill(group, Signal::SIGKILL);
        for p in tree {
            let _ = kill(Pid::from_raw(p), Signal::SIGKILL);
        }
    }
    let _ = killpg(group, Signal::SIGKILL);
    let _ = child.wait();
}

fn last_line(s: &str) -> Option<String> {
    s.lines().map(str::trim).rfind(|l| !l.is_empty()).map(crate::render::unspoofed)
}

/// git on the tap, local only: git's own cleaned environment, no hooks, no fsmonitor, no optional locks.
fn tap_git(tap: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::spawn::cmd(crate::install::git_exe()?);
    crate::install::git_env(&mut cmd);
    cmd.args(["--no-pager", "--no-optional-locks", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "protocol.allow=never", "-C"]);
    cmd.arg(tap).args(args).stdin(Stdio::null());
    match run(cmd, STEP_LIMIT, dir).map_err(|e| format!("git {e}"))? {
        (true, out, _) => Ok(out),
        (false, out, err) => Err(format!("git {}: {}", args[0], last_line(&err).or_else(|| last_line(&out)).unwrap_or_default())),
    }
}

/// Before nerd touches the tap: nothing changed in it, and no file brew reads but `Formula/nerd.rb`, a plain file.
fn check_tap(tap: &Path, dir: &Path) -> Result<(), String> {
    let status = tap_git(tap, dir, STATUS)?;
    let files = tap_git(tap, dir, &["ls-files", "-s", "-z"])?;
    let mut odd: Vec<String> = status.split('\0').filter(|l| !l.is_empty()).map(|l| format!("changed {l}")).collect();
    let mut ours = false;
    for entry in files.split('\0').filter(|e| !e.is_empty()) {
        let (meta, path) = entry.split_once('\t').unwrap_or((entry, ""));
        match path {
            "Formula/nerd.rb" if meta.starts_with("100644 ") => ours = true,
            p if p == "Formula/nerd.rb" || brew_reads(p) => odd.push(format!("brew reads {p} ({meta})")),
            _ => {}
        }
    }
    let plain = |p: &str, folder: bool| std::fs::symlink_metadata(tap.join(p)).is_ok_and(|m| if folder { m.is_dir() } else { m.file_type().is_file() });
    if !ours || !plain("Formula", true) || !plain("Formula/nerd.rb", false) {
        odd.push("no plain file Formula/nerd.rb".into());
    }
    if odd.is_empty() {
        return Ok(());
    }
    crate::log::warn(&format!("Homebrew tap {}: {}", tap.display(), crate::render::unspoofed(&odd.join(", "))));
    Err(REFUSED.into())
}

/// The tap's paths that can change what brew loads (Homebrew's formulary.rb).
fn brew_reads(p: &str) -> bool {
    p.ends_with(".rb") || ["Aliases/", "cmd/", "Casks/", "HomebrewFormula/"].iter().any(|d| p.starts_with(d)) || ["formula_renames.json", "tap_migrations.json"].contains(&p)
}

/// The formula nerd's release script writes, filled in for this release: one template, pinned with the script by the
/// release commit.
fn formula(tag: &str, name: &str, sha: &str) -> String {
    let block = PUBLISH_FORMULA.split_once("# formula begin\n").and_then(|(_, r)| r.split_once("# formula end\n")).map(|(b, _)| b);
    let body = block.and_then(|b| b.split_once("<<EOF\n")).and_then(|(_, r)| r.split_once("\nEOF\n")).map(|(b, _)| b);
    let body = body.expect("scripts/publish-formula.sh holds its heredoc between the formula marker lines");
    format!("{}\n", body.replace("$tag", tag).replace("$name", name).replace("$sha", sha))
}

/// The archive's hash from its `.sha256`, which install.sh checked against the archive and nerd's release keys.
fn checked_sha(dir: &Path, name: &str) -> Result<String, String> {
    let line = std::fs::read_to_string(dir.join(format!("{name}.sha256"))).map_err(|e| e.to_string())?;
    match line.trim_end().split_once("  ") {
        Some((sha, n)) if n == name && sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => Ok(sha.into()),
        _ => Err("the checksum file is not one line of shasum".into()),
    }
}

fn sha256_hex(b: &[u8]) -> String {
    unsafe extern "C" {
        fn CC_SHA256(data: *const std::ffi::c_void, len: u32, md: *mut u8) -> *mut u8;
    }
    let mut md = [0u8; 32];
    // SAFETY: CommonCrypto, in libSystem, reads `len` bytes of `data` and writes 32 into `md`; inputs here are a few KB.
    unsafe { CC_SHA256(b.as_ptr().cast(), b.len() as u32, md.as_mut_ptr()) };
    md.iter().map(|x| format!("{x:02x}")).collect()
}

/// The checked archive where Homebrew looks for its download of `url` before fetching one.
/// ceiling: Homebrew's naming rule (`cached_location` in abstract_file_download_strategy.rb) copied, not asked; a Homebrew
/// that changes it fetches instead, which the profile refuses, so the upgrade fails closed until this rule follows.
fn prime(root: &Path, url: &str, archive: &Path) -> Result<PathBuf, String> {
    let err = |e: std::io::Error| e.to_string();
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).map_err(err)?;
    let key = format!("{}--", sha256_hex(url.as_bytes()));
    let mut found = Vec::new();
    for e in std::fs::read_dir(&downloads).map_err(err)? {
        let p = e.map_err(err)?.path();
        let n = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if n.starts_with(&key) && !n.ends_with(".incomplete") {
            found.push(p);
        }
    }
    let target = match found.as_slice() {
        [one] => one.clone(),
        many => {
            for p in many {
                std::fs::remove_file(p).map_err(err)?;
            }
            downloads.join(format!("{key}{}", url.rsplit('/').next().unwrap_or_default()))
        }
    };
    // A name outside Homebrew's pattern, then a rename: Homebrew never takes half a file.
    let tmp = downloads.join(format!(".nerd-release-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let copied = (|| -> std::io::Result<()> {
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).custom_flags(nix::libc::O_NOFOLLOW).open(&tmp)?;
        std::io::copy(&mut std::fs::File::open(archive)?, &mut out)?;
        out.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        std::fs::rename(&tmp, &target)
    })();
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(target)
}

fn read_nofollow(p: &Path) -> std::io::Result<Vec<u8>> {
    let mut b = Vec::new();
    std::fs::OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW).open(p)?.read_to_end(&mut b)?;
    Ok(b)
}

fn write_nofollow(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).truncate(true).custom_flags(nix::libc::O_NOFOLLOW).open(p)?.write_all(bytes)
}

/// The tap's own formula and the hash of nerd's, kept while nerd's is in the tap.
fn saved_path(state: &Path) -> PathBuf {
    state.join("release-brew-formula")
}

/// One Homebrew upgrade at a time on this Mac; the lock goes with the process, a killed one too.
fn lock(state: &Path) -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let p = state.join("release-brew.lock");
    let f = std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).custom_flags(nix::libc::O_NOFOLLOW).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    nix::fcntl::Flock::lock(f, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(|_| "another Homebrew upgrade of nerd is running on this Mac".to_string())
}

/// A run killed while nerd's formula was in the tap: the tap's own bytes go back if the formula is still nerd's and
/// its only change; a `brew update` since may have committed the same bytes.
fn recover(tap: &Path, state: &Path, tag: &str, dir: &Path) -> Result<(), String> {
    let Ok(saved) = std::fs::read(saved_path(state)) else { return Ok(()) };
    let (hash, orig) = saved.split_at(saved.iter().position(|b| *b == b'\n').unwrap_or(saved.len()));
    let f = tap.join("Formula/nerd.rb");
    let only_ours = tap_git(tap, dir, STATUS)? == " M Formula/nerd.rb\0";
    if only_ours && read_nofollow(&f).is_ok_and(|now| sha256_hex(&now).as_bytes() == hash) {
        write_nofollow(&f, orig.get(1..).unwrap_or_default()).map_err(|e| format!("{}: {e}", f.display()))?;
        crate::log::info(&format!("release {tag}: the tap's formula put back after an interrupted upgrade"));
    } else {
        crate::log::warn(&format!("release {tag}: an interrupted upgrade's saved formula dropped: the tap changed since and is left as it is"));
    }
    std::fs::remove_file(saved_path(state)).map_err(|e| e.to_string())
}

/// nerd's formula in place of the tap's, whose bytes wait in the state folder until they go back.
fn put(tap: &Path, state: &Path, formula: &str) -> Result<Vec<u8>, String> {
    let f = tap.join("Formula/nerd.rb");
    let err = |e: std::io::Error| format!("{}: {e}", f.display());
    let orig = read_nofollow(&f).map_err(err)?;
    let saved = [sha256_hex(formula.as_bytes()).as_bytes(), b"\n", &orig].concat();
    crate::paths::write_private(&saved_path(state), "tmp", &saved).map_err(|e| e.to_string())?;
    if let Err(e) = write_nofollow(&f, formula.as_bytes()) {
        // Half written by nerd a moment ago: the tap's own bytes go back over whatever it holds.
        let _ = write_nofollow(&f, &orig);
        let _ = std::fs::remove_file(saved_path(state));
        return Err(err(e));
    }
    Ok(orig)
}

/// The tap's own formula back, only over nerd's: a `brew update` meanwhile wins. Then the tap is checked clean.
fn restore(tap: &Path, state: &Path, tag: &str, formula: &str, orig: &[u8], dir: &Path) {
    let f = tap.join("Formula/nerd.rb");
    if read_nofollow(&f).is_ok_and(|now| now == formula.as_bytes()) {
        if let Err(e) = write_nofollow(&f, orig) {
            // The saved copy stays for the next run's recovery.
            crate::log::warn(&format!("release {tag}: the tap's formula was not put back: {e}"));
            return;
        }
        crate::log::info(&format!("release {tag}: tap restored"));
    } else {
        crate::log::warn(&format!("release {tag}: tap changed during the upgrade"));
    }
    let _ = std::fs::remove_file(saved_path(state));
    match tap_git(tap, dir, STATUS) {
        Ok(s) if s.is_empty() => {}
        Ok(s) => crate::log::warn(&format!("release {tag}: the tap is not clean after the upgrade: {}", crate::render::unspoofed(&s.replace('\0', ", ")))),
        Err(e) => crate::log::warn(&format!("release {tag}: the tap's state after the upgrade is unknown: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "v0.99.0";
    const NAME: &str = "nerd-v0.99.0-macos-arm64.tar.gz";
    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TAPS_OWN: &str = "class Nerd < Formula\n  # the tap's own\nend\n";

    fn write(p: &Path, text: &str, mode: u32) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The test's own git, never the user's config or hooks.
    fn git(dir: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C"]).arg(dir).args(args);
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE");
        assert!(cmd.stdout(Stdio::null()).status().unwrap().success(), "git {args:?}");
    }

    struct Fake {
        root: PathBuf,
        brew: Brew,
    }

    impl Fake {
        fn tap(&self) -> PathBuf {
            tap_of(&self.brew.prefix)
        }
        /// What the stand-in brew also does on `upgrade`, as sh.
        fn does(&self, sh: &str) {
            std::fs::write(self.root.join("does"), sh).unwrap();
        }
        fn formula_now(&self) -> String {
            std::fs::read_to_string(self.tap().join("Formula/nerd.rb")).unwrap()
        }
        fn saved(&self) -> PathBuf {
            saved_path(&self.brew.state)
        }
    }

    /// A Homebrew prefix in a test folder holding nerd from nerdterm/tap, its tap a git repository; a stand-in for
    /// sandbox-exec that runs the stand-in brew; a state folder of its own; a checked release in `run/`.
    fn fake(tag: &str) -> Fake {
        let root = crate::paths::test_dir(tag).canonicalize().unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let prefix = root.join("prefix");
        let keg = prefix.join("Cellar/nerd/0.21.0");
        write(&keg.join("bin/nerd"), "", 0o755);
        write(&keg.join("INSTALL_RECEIPT.json"), r#"{"source":{"tap":"nerdterm/tap"}}"#, 0o644);
        let tap = tap_of(&prefix);
        let r = root.display();
        let brew = format!(
            "#!/bin/sh\ncase $1 in\n--version) echo 'Homebrew 7.0.11'; echo 'Homebrew/homebrew-core' ;;\n--cache) if [ -f '{r}/cache-fails' ]; then echo 'Error: no cache' >&2; exit 1; fi; echo '{r}/cache' ;;\n\
             upgrade) cp '{t}/Formula/nerd.rb' '{r}/seen.rb'; echo \"$*\" > '{r}/upgraded'; if [ -f '{r}/does' ]; then . '{r}/does'; fi ;;\n*) exit 9 ;;\nesac\n",
            t = tap.display()
        );
        write(&prefix.join("bin/brew"), &brew, 0o755);
        std::os::unix::fs::symlink(keg.join("bin/nerd"), prefix.join("bin/nerd")).unwrap();
        write(&root.join("sandbox-exec"), &format!("#!/bin/sh\n[ \"$1\" = -p ] || exit 97\nprintf '%s' \"$2\" > '{r}/profile'\nshift 2\nexec \"$@\"\n"), 0o755);
        write(&tap.join("README.md"), "# tap\n", 0o644);
        write(&tap.join("Formula/nerd.rb"), TAPS_OWN, 0o644);
        git(&tap, &["init", "-q"]);
        git(&tap, &["add", "-A"]);
        git(&tap, &["commit", "-qm", "tap"]);
        write(&root.join(format!("run/{NAME}")), "the archive", 0o600);
        write(&root.join(format!("run/{NAME}.sha256")), &format!("{SHA}  {NAME}\n"), 0o600);
        Fake { brew: Brew { prefix, run: root.join("sandbox-exec"), state: root.join("state") }, root }
    }

    fn upgrade(f: &Fake, limit: Duration) -> Result<String, String> {
        f.brew.upgrade(TAG, NAME, &f.root.join("run"), limit)
    }

    fn rendered() -> String {
        formula(TAG, NAME, SHA)
    }

    #[test]
    fn only_a_nerd_homebrew_installed_from_nerd_s_tap_is_upgraded_by_it() {
        let f = fake("brew-of");
        let exe = f.brew.prefix.join("bin/nerd");
        assert_eq!(Brew::of(&exe).map(|b| b.prefix), Some(f.brew.prefix.clone()), "through Homebrew's link");
        let receipt = f.brew.prefix.join("Cellar/nerd/0.21.0/INSTALL_RECEIPT.json");
        std::fs::write(&receipt, r#"{"source":{"tap":"homebrew/core"}}"#).unwrap();
        assert!(Brew::of(&exe).is_none(), "another tap");
        std::fs::remove_file(&receipt).unwrap();
        assert!(Brew::of(&exe).is_none(), "no receipt");
        std::fs::write(&receipt, r#"{"source":{"tap":"nerdterm/tap"}}"#).unwrap();
        std::fs::rename(f.tap(), f.root.join("moved")).unwrap();
        assert!(Brew::of(&exe).is_none(), "no tap folder");
        std::fs::rename(f.root.join("moved"), f.tap()).unwrap();
        std::fs::rename(f.brew.prefix.join("bin/brew"), f.root.join("brew")).unwrap();
        std::os::unix::fs::symlink(f.root.join("brew"), f.brew.prefix.join("bin/brew")).unwrap();
        assert!(Brew::of(&exe).is_none(), "brew is a link, not this prefix's own file");
        std::fs::remove_file(f.brew.prefix.join("bin/brew")).unwrap();
        std::fs::rename(f.root.join("brew"), f.brew.prefix.join("bin/brew")).unwrap();
        assert!(Brew::of(&exe).is_some());
        let elsewhere = f.root.join("Cellar/nerd/0.21.0/bin/nerd");
        write(&elsewhere, "", 0o755);
        assert!(Brew::of(&elsewhere).is_none(), "a Cellar with no Homebrew beside it");
    }

    #[test]
    fn the_formula_is_the_release_script_s_own() {
        let t = crate::paths::test_dir("brew-formula");
        let ssl = "/usr/bin/openssl";
        assert!(Command::new(ssl).args(["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out"]).arg(t.join("k.key")).status().unwrap().success());
        let public = Command::new(ssl).args(["ec", "-pubout", "-in"]).arg(t.join("k.key")).stderr(Stdio::null()).output().unwrap().stdout;
        write(&t.join("install.sh"), &format!("# release keys begin\nkeys='{}'\n# release keys end\n", String::from_utf8(public).unwrap().trim_end()), 0o644);
        let sum = t.join(format!("served/{NAME}.sha256"));
        write(&sum, &format!("{SHA}  {NAME}\n"), 0o644);
        assert!(Command::new(ssl).args(["dgst", "-sha256", "-sign"]).arg(t.join("k.key")).arg("-out").arg(t.join(format!("served/{NAME}.sha256.sig"))).arg(&sum).status().unwrap().success());
        let gh = "#!/bin/sh\n[ \"$1 $2\" = \"release download\" ] || exit 1\nwhile [ $# -gt 0 ]; do case $1 in -p) p=$2; shift ;; -O) o=$2; shift ;; esac; shift; done\ncp \"$STUB/served/$p\" \"$o\"\n";
        write(&t.join("bin/gh"), gh, 0o755);
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/publish-formula.sh");
        let out = Command::new("/bin/sh").arg(script).args(["--dry-run", TAG]).env("PATH", format!("{}:/usr/bin:/bin", t.join("bin").display())).env("STUB", &t).env("NERD_INSTALL_SH", t.join("install.sh")).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(rendered(), String::from_utf8(out.stdout).unwrap());
        assert!(rendered().contains(&format!("  url \"{}\"\n", super::super::asset_url(TAG, NAME))), "the url Homebrew's cache is named after");
    }

    #[test]
    fn the_tap_is_touched_only_when_nothing_in_it_can_redirect_brew() {
        let f = fake("brew-tap");
        let (tap, dir) = (f.tap(), f.root.join("run"));
        assert_eq!(check_tap(&tap, &dir), Ok(()));
        write(&tap.join("README.md"), "# tap, edited\n", 0o644);
        assert_eq!(check_tap(&tap, &dir), Err(REFUSED.into()), "a change not committed");
        git(&tap, &["commit", "-qam", "readme"]);
        assert_eq!(check_tap(&tap, &dir), Ok(()), "a file brew does not read, changed upstream");
        write(&tap.join(".git/info/exclude"), "/x\n", 0o644);
        write(&tap.join("x"), "", 0o644);
        assert_eq!(check_tap(&tap, &dir), Err(REFUSED.into()), "an ignored file");
        std::fs::remove_file(tap.join("x")).unwrap();
        for extra in ["cmd/brew-x.rb", "Aliases/nerd2", "formula_renames.json", "tap_migrations.json", "Formula/other.rb", "Casks/x", "HomebrewFormula/x"] {
            write(&tap.join(extra), "", 0o644);
            git(&tap, &["add", "-A"]);
            git(&tap, &["commit", "-qm", extra]);
            assert_eq!(check_tap(&tap, &dir), Err(REFUSED.into()), "{extra}");
            git(&tap, &["rm", "-q", extra]);
            git(&tap, &["commit", "-qm", "back"]);
        }
        assert_eq!(check_tap(&tap, &dir), Ok(()));
        std::fs::set_permissions(tap.join("Formula/nerd.rb"), std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&tap, &["commit", "-qam", "mode"]);
        assert_eq!(check_tap(&tap, &dir), Err(REFUSED.into()), "an executable formula");
        std::fs::remove_file(tap.join("Formula/nerd.rb")).unwrap();
        std::os::unix::fs::symlink(f.root.join("elsewhere.rb"), tap.join("Formula/nerd.rb")).unwrap();
        git(&tap, &["add", "-A"]);
        git(&tap, &["commit", "-qm", "link"]);
        assert_eq!(check_tap(&tap, &dir), Err(REFUSED.into()), "a link");
    }

    #[test]
    fn the_archive_goes_where_homebrew_looks_for_its_download() {
        let root = crate::paths::test_dir("brew-cache");
        let archive = root.join("a.tar.gz");
        write(&archive, "verified", 0o600);
        let url = super::super::asset_url(TAG, NAME);
        let key = sha256_hex(url.as_bytes());
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let downloads = root.join("cache/downloads");
        let listed = || {
            let mut l: Vec<String> = std::fs::read_dir(&downloads).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            l.sort();
            l
        };

        let p = prime(&root.join("cache"), &url, &archive).unwrap();
        assert_eq!(p, downloads.join(format!("{key}--{NAME}")), "none yet: the url's own name");
        assert_eq!(std::fs::read(&p).unwrap(), b"verified");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);

        std::fs::remove_file(&p).unwrap();
        write(&downloads.join(format!("{key}--older-name.tar.gz")), "old", 0o644);
        write(&downloads.join(format!("{key}--{NAME}.incomplete")), "half", 0o644);
        let p = prime(&root.join("cache"), &url, &archive).unwrap();
        assert_eq!(p, downloads.join(format!("{key}--older-name.tar.gz")), "the one there is the one Homebrew takes");
        assert_eq!(std::fs::read(&p).unwrap(), b"verified");
        assert_eq!(listed(), [format!("{key}--{NAME}.incomplete"), format!("{key}--older-name.tar.gz")], "an .incomplete is not a download; nothing else left");

        write(&downloads.join(format!("{key}--second.tar.gz")), "old", 0o644);
        let p = prime(&root.join("cache"), &url, &archive).unwrap();
        assert_eq!(p, downloads.join(format!("{key}--{NAME}")), "two: both gone, the url's own name");
        assert_eq!(listed(), [format!("{key}--{NAME}"), format!("{key}--{NAME}.incomplete")]);
    }

    #[test]
    fn brew_runs_with_no_network_and_only_nerd_s_variables() {
        assert_eq!(PROFILE, r#"(version 1)(allow default)(deny network*)(allow network-bind network-inbound (local unix-socket))(allow process-exec (literal "/usr/bin/sandbox-exec") (with no-sandbox))"#);
        let b = Brew { prefix: "/opt/homebrew".into(), run: "/usr/bin/sandbox-exec".into(), state: "/x".into() };
        for args in [&["--cache"][..], &["--version"], &["upgrade", "nerdterm/tap/nerd"]] {
            let cmd = b.cmd(args);
            assert_eq!(cmd.get_program(), "/usr/bin/sandbox-exec");
            let mut want: Vec<&str> = vec!["-p", PROFILE, "/opt/homebrew/bin/brew"];
            want.extend(args);
            assert_eq!(cmd.get_args().collect::<Vec<_>>(), want);
            let env: std::collections::BTreeMap<String, Option<String>> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into(), v.map(|v| v.to_string_lossy().into()))).collect();
            let fixed = [
                ("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
                ("LANG", "en_US.UTF-8"),
                ("HOMEBREW_NO_AUTO_UPDATE", "1"),
                ("HOMEBREW_NO_ANALYTICS", "1"),
                ("HOMEBREW_NO_INSTALL_CLEANUP", "1"),
                ("HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK", "1"),
                ("HOMEBREW_NO_ENV_HINTS", "1"),
                ("HOMEBREW_NO_AUTOREMOVE", "1"),
                ("HOMEBREW_CURL_RETRIES", "0"),
            ];
            for (k, v) in fixed {
                assert_eq!(env.get(k), Some(&Some(v.to_string())), "{k}");
            }
            assert_eq!(env.get("HOME"), Some(&crate::paths::home().map(|h| h.to_string_lossy().into_owned())));
            let others: Vec<&String> = env.keys().filter(|k| !fixed.iter().any(|(f, _)| f == k) && !["HOME", "USER", "LOGNAME", "TMPDIR"].contains(&k.as_str())).collect();
            assert!(others.is_empty(), "nothing else: {others:?}");
            assert!(env.values().all(Option::is_some), "a clean slate, not removals from nerd's own");
        }
    }

    #[test]
    fn a_run_past_its_limit_is_stopped_with_everything_it_started() {
        let root = crate::paths::test_dir("brew-stop");
        let kid = root.join("kid");
        let sh = format!("#!/bin/sh\ntrap '' TERM\n/usr/bin/perl -e '$SIG{{TERM}}=\"IGNORE\"; setpgrp(0,0); open(F, \">\", $ARGV[0]); print F $$; close F; sleep 60' '{}' &\nsleep 60\n", kid.display());
        write(&root.join("hang"), &sh, 0o755);
        let start = Instant::now();
        let r = run(Command::new(root.join("hang")), Duration::from_secs(1), &root);
        assert!(r.as_ref().is_err_and(|e| e.starts_with("did not finish in 1 s")), "{r:?}");
        assert!(start.elapsed() < GRACE + Duration::from_secs(3));
        let pid = nix::unistd::Pid::from_raw(std::fs::read_to_string(&kid).unwrap().trim().parse().unwrap());
        let t = Instant::now();
        while nix::sys::signal::kill(pid, None).is_ok() && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(nix::sys::signal::kill(pid, None).is_err(), "a process that left the group and ignored TERM is gone too");
    }

    #[test]
    fn one_upgrade_at_a_time() {
        let f = fake("brew-lock");
        let held = lock(&f.brew.state).unwrap();
        assert_eq!(lock(&f.brew.state).err().as_deref(), Some("another Homebrew upgrade of nerd is running on this Mac"));
        drop(held);
        assert!(lock(&f.brew.state).is_ok());
    }

    #[test]
    fn brew_installs_nerd_s_formula_from_the_primed_cache_and_the_tap_gets_its_own_back() {
        let f = fake("brew-ok");
        assert_eq!(upgrade(&f, BREW_LIMIT), Ok("nerd 0.99.0".into()));
        assert_eq!(std::fs::read_to_string(f.root.join("seen.rb")).unwrap(), rendered(), "brew ran with nerd's formula");
        assert_eq!(std::fs::read_to_string(f.root.join("upgraded")).unwrap(), "upgrade nerdterm/tap/nerd\n");
        assert_eq!(std::fs::read_to_string(f.root.join("profile")).unwrap(), PROFILE);
        assert_eq!(f.formula_now(), TAPS_OWN, "the tap's own formula back");
        let key = sha256_hex(super::super::asset_url(TAG, NAME).as_bytes());
        assert_eq!(std::fs::read(f.root.join(format!("cache/downloads/{key}--{NAME}"))).unwrap(), b"the archive");
        assert!(!f.saved().exists(), "the saved copy is gone");
    }

    #[test]
    fn a_failed_or_stopped_brew_leaves_the_tap_as_it_was_and_says_what_to_do() {
        let f = fake("brew-fail");
        f.does("exit 1");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew upgrade: Homebrew gave no reason; run brew update && brew upgrade nerd".into()));
        assert_eq!(f.formula_now(), TAPS_OWN);

        f.does("sleep 30");
        let e = upgrade(&f, Duration::from_secs(1)).unwrap_err();
        assert!(e.starts_with("failed at brew upgrade: did not finish in 1 s") && e.ends_with("; run brew update && brew upgrade nerd") && !e.contains("brew link"), "{e}");
        assert_eq!(f.formula_now(), TAPS_OWN);
        assert!(!f.saved().exists());

        f.does(&format!("echo 'Error: nerd: no bottle' >&2; rm -f '{}'; exit 1", f.brew.prefix.join("bin/nerd").display()));
        let e = upgrade(&f, BREW_LIMIT).unwrap_err();
        assert!(e.starts_with("failed at brew upgrade: Homebrew said \"Error: nerd: no bottle\"; nerd is no longer linked into") && e.ends_with("run brew link nerd, then brew update && brew upgrade nerd"), "{e}");
        assert_eq!(f.formula_now(), TAPS_OWN);
        assert!(!f.saved().exists());
    }

    #[test]
    fn a_refused_tap_is_left_untouched_and_brew_never_runs() {
        let f = fake("brew-refused");
        write(&f.tap().join("cmd/brew-x.rb"), "", 0o644);
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(format!("failed at tap check: {REFUSED}")));
        assert!(!f.root.join("upgraded").exists() && !f.root.join("cache").exists(), "no brew upgrade, nothing primed");
        assert_eq!(f.formula_now(), TAPS_OWN);
    }

    #[test]
    fn a_change_to_the_tap_during_the_upgrade_wins() {
        let f = fake("brew-race");
        f.does(&format!("printf 'upstream\\n' > '{}'", f.tap().join("Formula/nerd.rb").display()));
        assert!(upgrade(&f, BREW_LIMIT).is_ok());
        assert_eq!(f.formula_now(), "upstream\n", "not overwritten");
        assert!(!f.saved().exists());
    }

    #[test]
    fn a_killed_run_s_formula_is_put_back_by_the_next_run() {
        let f = fake("brew-killed");
        let saved = [sha256_hex(rendered().as_bytes()).as_bytes(), b"\n", TAPS_OWN.as_bytes()].concat();
        std::fs::write(f.tap().join("Formula/nerd.rb"), rendered()).unwrap();
        crate::paths::write_private(&f.saved(), "tmp", &saved).unwrap();
        assert!(upgrade(&f, BREW_LIMIT).is_ok(), "put back first, so the tap checks clean");
        assert_eq!(f.formula_now(), TAPS_OWN);

        std::fs::write(f.tap().join("Formula/nerd.rb"), "someone else's\n").unwrap();
        crate::paths::write_private(&f.saved(), "tmp", &saved).unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(format!("failed at tap check: {REFUSED}")), "not nerd's: left, then refused as changed");
        assert_eq!(f.formula_now(), "someone else's\n");
        assert!(!f.saved().exists());
    }

    #[test]
    fn a_killed_run_then_brew_update_bringing_the_same_formula_leaves_upstream_s_commit() {
        let f = fake("brew-killed-updated");
        let saved = [sha256_hex(rendered().as_bytes()).as_bytes(), b"\n", TAPS_OWN.as_bytes()].concat();
        std::fs::write(f.tap().join("Formula/nerd.rb"), rendered()).unwrap();
        crate::paths::write_private(&f.saved(), "tmp", &saved).unwrap();
        git(&f.tap(), &["commit", "-qam", "nerd 0.99.0"]);
        assert_eq!(upgrade(&f, BREW_LIMIT), Ok("nerd 0.99.0".into()));
        assert_eq!(f.formula_now(), rendered(), "upstream's commit, not the older bytes");
        let status = Command::new("git").arg("-C").arg(f.tap()).args(["status", "--porcelain", "--untracked-files=all", "--ignored"]).output().unwrap();
        assert!(status.status.success() && status.stdout.is_empty(), "{}", String::from_utf8_lossy(&status.stdout));
        assert!(!f.saved().exists());
    }

    #[test]
    fn a_step_before_brew_upgrade_that_fails_stops_there_and_leaves_the_tap() {
        let untouched = |f: &Fake| {
            assert_eq!(f.formula_now(), TAPS_OWN);
            assert!(!f.root.join("upgraded").exists() && !f.saved().exists());
        };
        let f = fake("brew-sum");
        std::fs::write(f.root.join(format!("run/{NAME}.sha256")), format!("{SHA}  other.tar.gz\n")).unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at checksum: the checksum file is not one line of shasum".into()));
        assert!(!f.root.join("cache").exists());
        untouched(&f);

        let f = fake("brew-cache");
        std::fs::write(f.root.join("cache-fails"), "").unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew --cache: Homebrew said \"Error: no cache\"".into()));
        assert!(!f.root.join("cache").exists());
        untouched(&f);

        let f = fake("brew-put");
        std::fs::set_permissions(f.tap().join("Formula/nerd.rb"), std::fs::Permissions::from_mode(0o444)).unwrap();
        let e = upgrade(&f, BREW_LIMIT).unwrap_err();
        assert!(e.starts_with("failed at formula: "), "{e}");
        assert_eq!(std::fs::read_dir(f.root.join("cache/downloads")).unwrap().count(), 0, "the primed archive removed");
        untouched(&f);
    }
}
