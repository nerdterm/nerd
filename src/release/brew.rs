//! A nerd that Homebrew installed from nerdterm/tap, upgraded on the person's click: nerd fetches and checks the release
//! as for ~/.local/bin, writes its own formula into the tap, has Homebrew install from a primed cache with no network,
//! and puts the tap's formula back ("What leaves nerd", `egress:release-check`).

use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
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
    fn cmd(&self, args: &[&str], temp: &Path) -> Command {
        let mut cmd = crate::spawn::cmd(&self.run);
        cmd.arg("-p").arg(PROFILE).arg(self.prefix.join("bin/brew")).args(args);
        cmd.env_clear().envs(brew_env(&self.prefix, temp)).stdin(Stdio::null());
        cmd
    }

    /// One brew run's output, or brew's own last line when it failed.
    fn step(&self, args: &[&str], limit: Duration, dir: &Path) -> Result<String, String> {
        match run(self.cmd(args, &dir.join("tmp")), limit, dir)? {
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
        // Where Homebrew unpacks the archive: inside nerd's own release folder, which no pane may write.
        let temp = dir.join("tmp");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&temp).and_then(|_| std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o700))).map_err(|e| ("Homebrew's temp folder", format!("{}: {e}; try again", temp.display())))?;
        let version = self.step(&["--version"], STEP_LIMIT, dir).map_err(at("brew --version"))?;
        crate::log::info(&format!("release {tag}: {}", crate::render::unspoofed(version.lines().next().unwrap_or("").trim())));
        check_tap(&tap, dir).map_err(at("tap check"))?;
        crate::log::info(&format!("release {tag}: tap checked"));
        let sha = checked_sha(dir, name).map_err(at("checksum"))?;
        let formula = formula(tag, name, &sha);
        let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let config = self.step(&["config"], STEP_LIMIT, dir).map_err(|e| ("brew config", format!("{e}; try again")))?;
        // brew config names HOMEBREW_TEMP only when it is not the default, which brew.sh also falls back to unasked.
        let staged = PathBuf::from(config.lines().find_map(|l| l.strip_prefix("HOMEBREW_TEMP: ")).unwrap_or("/private/tmp").trim());
        let staged_shown = crate::render::unspoofed(&staged.to_string_lossy());
        crate::log::info(&format!("release {tag}: Homebrew stages in {staged_shown}"));
        if real(&staged) != real(&temp) {
            return Err(("Homebrew's temp folder", format!("Homebrew stages in {staged_shown}, not in nerd's own folder (HOMEBREW_TEMP in a brew.env, or the folder not writable); remove HOMEBREW_TEMP from your brew.env, or run brew update && brew upgrade nerd")));
        }
        let root = PathBuf::from(self.step(&["--cache"], STEP_LIMIT, dir).map_err(at("brew --cache"))?.lines().next().unwrap_or("").trim());
        let root_shown = crate::render::unspoofed(&root.to_string_lossy());
        if !root.is_absolute() {
            return Err(("brew --cache", format!("not a folder: {root_shown}")));
        }
        crate::log::info(&format!("release {tag}: Homebrew's cache is {root_shown}"));
        if real(&root) != real(&crate::paths::home().unwrap_or_default().join("Library/Caches/Homebrew")) {
            return Err(("Homebrew's cache", format!("Homebrew's cache is {root_shown}, not ~/Library/Caches/Homebrew; remove HOMEBREW_CACHE from your brew.env, or run brew update && brew upgrade nerd")));
        }
        // Checked, hashed and primed from one read: what install.sh checked is what Homebrew gets and nerd compares.
        let bytes = std::fs::read(dir.join(name)).map_err(|e| ("checksum", format!("{}: {e}; try again", dir.join(name).display())))?;
        if sha256_hex(&bytes) != sha {
            return Err(("checksum", "the archive changed after install.sh checked it; try again".into()));
        }
        let want = sha256_hex(&archived_nerd(&bytes).map_err(|e| ("the archive", format!("{e}; try again")))?);
        let primed = prime(&root, &super::asset_url(tag, name), &bytes).map_err(at("Homebrew's cache"))?;
        let orig = put(&tap, &self.state, &formula).inspect_err(|_| drop(std::fs::remove_file(&primed))).map_err(at("formula"))?;
        crate::log::info(&format!("release {tag}: formula written"));
        let done = self.step(&["upgrade", "nerdterm/tap/nerd"], limit, dir);
        restore(&tap, &self.state, tag, &formula, &orig, dir);
        match done {
            Ok(_) => {
                crate::log::info(&format!("release {tag}: brew upgrade done"));
                self.installed(tag, &want).map_err(at("brew upgrade"))?;
                Ok(format!("nerd {}", tag.trim_start_matches('v')))
            }
            // ceiling: matches Homebrew's English wording; another wording gets the advice below, which a pinned nerd cannot follow.
            Err(why) if why.contains("pinned") => Err(("brew upgrade", format!("{why}; run brew unpin nerd, then brew update && brew upgrade nerd"))),
            Err(why) if self.prefix.join("bin/nerd").exists() => Err(("brew upgrade", format!("{why}; run brew update && brew upgrade nerd"))),
            Err(why) => Err(("brew upgrade", format!("{why}; nerd is no longer linked into {}: run brew link nerd, then brew update && brew upgrade nerd", self.prefix.join("bin").display()))),
        }
    }

    /// With nothing run: Homebrew's linked nerd is this release's keg, and its `nerd` hashes to `want`, the checked
    /// archive's.
    fn installed(&self, tag: &str, want: &str) -> Result<(), String> {
        let version = tag.trim_start_matches('v');
        let (opt, keg) = (self.prefix.join("opt/nerd"), self.prefix.join("Cellar/nerd").join(version));
        let exe = keg.join("bin/nerd");
        let linked = std::fs::canonicalize(&opt).ok();
        if linked.as_deref() != Some(keg.as_path()) {
            let to = linked.map_or_else(|| "nothing".into(), |p| crate::render::unspoofed(&p.to_string_lossy()));
            return Err(format!("Homebrew did not install nerd {version}: {} leads to {to}; run brew update && brew upgrade nerd", opt.display()));
        }
        if !std::fs::symlink_metadata(&exe).is_ok_and(|m| m.file_type().is_file()) {
            // Not a plain file (a link may lead anywhere): the keg's bin loses its search bit, so nothing runs through it.
            let bin = keg.join("bin");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).map_err(|e| format!("Homebrew's nerd {version} has no plain file at {}, and nerd could not stop it running ({}: {e}): run brew reinstall nerd", exe.display(), bin.display()))?;
            return Err(format!("Homebrew's nerd {version} has no plain file at {}; it will not run: run chmod 755 {}, then brew reinstall nerd", exe.display(), bin.display()));
        }
        // A keg nerd cannot read counts as not the release: it is disabled too.
        let got = read_nofollow(&exe).map_or_else(|e| format!("unreadable ({e})"), |b| sha256_hex(&b));
        crate::log::info(&format!("release {tag}: nerd in the checked archive {want}, in Homebrew's keg {got}"));
        if want == got {
            return Ok(());
        }
        // ceiling: Homebrew links the keg before this check, so what starts <prefix>/bin/nerd meanwhile could run a swapped binary before nerd disables it; the way past is an install without linking, and nerd linking only after the comparison.
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).map_err(|e| format!("the nerd Homebrew installed is not the release nerd checked, and nerd could not stop it running ({}: {e}): run brew reinstall nerd", exe.display()))?;
        Err("the nerd Homebrew installed is not the release nerd checked; it will not run: run brew reinstall nerd".into())
    }
}

/// `nerd` from the checked archive's bytes, through tar's stdin and stdout, written nowhere.
fn archived_nerd(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = crate::spawn::cmd("/usr/bin/tar").arg("-xOzf").arg("-").arg("nerd").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("tar: {e}"))?;
    let mut input = child.stdin.take().ok_or("tar: no stdin")?;
    // macOS <sys/fcntl.h>; the libc version this crate locks does not name it.
    const F_SETNOSIGPIPE: nix::libc::c_int = 73;
    // SAFETY: fcntl on the pipe's own descriptor, open in `input` for this call; a tar that exits early then gives EPIPE, not a SIGPIPE that would end `--check-update`.
    if unsafe { nix::libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&input), F_SETNOSIGPIPE, 1) } == -1 {
        return Err(format!("tar: {}", std::io::Error::last_os_error()));
    }
    // Fed from a thread: tar's output fills its pipe before all of the input is read.
    let out = std::thread::scope(|s| {
        s.spawn(move || drop(input.write_all(archive)));
        child.wait_with_output()
    })
    .map_err(|e| format!("tar: {e}"))?;
    match out.status.success() && !out.stdout.is_empty() {
        true => Ok(out.stdout),
        // tar's first line names the cause; its last says only that it exited late.
        false => Err(String::from_utf8_lossy(&out.stderr).lines().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| "tar found no nerd in the archive".into(), crate::render::unspoofed)),
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

/// The home folder, the user, this Homebrew first on PATH, `temp` to stage in, and Homebrew told to update, report and
/// remove nothing. A `brew.env` file can still override these: what Homebrew used is asked afterwards.
fn brew_env(prefix: &Path, temp: &Path) -> Vec<(String, std::ffi::OsString)> {
    let mut env: Vec<(String, std::ffi::OsString)> = ["USER", "LOGNAME"].iter().filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v))).collect();
    env.push(("HOME".into(), crate::paths::home().unwrap_or_default().into()));
    env.push(("HOMEBREW_TEMP".into(), temp.into()));
    // For bin/brew's own shell: Homebrew stages under HOMEBREW_TEMP, not TMPDIR.
    env.push(("TMPDIR".into(), temp.into()));
    env.push(("PATH".into(), format!("{}/bin:/usr/bin:/bin:/usr/sbin:/sbin", prefix.display()).into()));
    env.push(("LANG".into(), "en_US.UTF-8".into()));
    for k in ["HOMEBREW_NO_AUTO_UPDATE", "HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_INSTALL_CLEANUP", "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK", "HOMEBREW_NO_ENV_HINTS", "HOMEBREW_NO_AUTOREMOVE"] {
        env.push((k.into(), "1".into()));
    }
    env.push(("HOMEBREW_CURL_RETRIES".into(), "0".into()));
    env
}

/// The signal that told this run to stop, or 0; and brew's process group while one runs.
static STOPPED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static GROUP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// The broker kills this run 250 ms after its TERM: brew's group is killed inside that.
const TERM_GRACE: Duration = Duration::from_millis(100);

/// Signal-safe: brew is in a group of its own, so the signal that stops this run is passed on to it.
#[cfg(not(test))]
extern "C" fn on_stop(sig: nix::libc::c_int) {
    let errno = nix::errno::Errno::last_raw();
    let group = GROUP.load(std::sync::atomic::Ordering::SeqCst);
    if group > 0 {
        // SAFETY: kill is async-signal-safe; the group is the child's this run started last, cleared as it is reaped.
        unsafe { nix::libc::kill(-group, nix::libc::SIGTERM) };
    }
    STOPPED.store(sig, std::sync::atomic::Ordering::SeqCst);
    nix::errno::Errno::set_raw(errno);
}

fn stopped() -> Option<(i32, String)> {
    let sig = STOPPED.load(std::sync::atomic::Ordering::SeqCst);
    (sig != 0).then(|| (sig, format!("was stopped with nerd (signal {sig})")))
}

/// The three signals that stop this run go to `on_stop`, once. Not in the unit tests' binary, which Control + C must
/// still end; tests/release.rs runs it in the real one.
#[cfg(not(test))]
fn listen() {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
            // SAFETY: the handler only calls kill and stores atomics.
            let _ = unsafe { sigaction(sig, &SigAction::new(SigHandler::Handler(on_stop), SaFlags::SA_RESTART, SigSet::empty())) };
        }
    });
}

/// `cmd` in a process group of its own, its output in files in `dir`; past `limit`, or when this run is told to stop,
/// TERM to that group, then KILL to the group and every process still under it. `Ok((succeeded, stdout, stderr))`.
fn run(mut cmd: Command, limit: Duration, dir: &Path) -> Result<(bool, String, String), String> {
    #[cfg(not(test))]
    listen();
    if let Some((_, why)) = stopped() {
        return Err(why);
    }
    let (o, e) = (dir.join("out"), dir.join("err"));
    let file = |p: &Path| std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()));
    cmd.stdout(file(&o)?).stderr(file(&e)?).process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("did not start: {e}"))?;
    GROUP.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let start = Instant::now();
    let done = loop {
        if let Some((sig, why)) = stopped() {
            // ceiling: after a TERM Homebrew's rescue is cut to 100 ms, and on a busy Mac the broker's KILL can still come first, leaving the formula to the next run's recovery; the way past is a longer grace in the broker for the release run.
            stop(&mut child, if sig == nix::libc::SIGTERM { TERM_GRACE } else { GRACE });
            break Err(why);
        }
        match child.try_wait() {
            Ok(Some(s)) => {
                GROUP.store(0, std::sync::atomic::Ordering::SeqCst);
                let read = |p: &Path| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned();
                break Ok((s.success(), read(&o), read(&e)));
            }
            Ok(None) => {}
            Err(e) => break Err(e.to_string()),
        }
        if start.elapsed() >= limit {
            stop(&mut child, GRACE);
            break Err(format!("did not finish in {} s and was stopped; a build Homebrew started may still be running", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    GROUP.store(0, std::sync::atomic::Ordering::SeqCst);
    done
}

fn stop(child: &mut std::process::Child, grace: Duration) {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    let group = Pid::from_raw(child.id() as i32);
    let _ = killpg(group, Signal::SIGTERM);
    let t = Instant::now();
    let mut gone = false;
    while !gone && t.elapsed() < grace {
        std::thread::sleep(Duration::from_millis(20));
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

/// git on the tap, local only: the prefix's or the system's git, git's own cleaned environment, no hooks, no fsmonitor,
/// no optional locks.
fn tap_git(tap: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    // The tap is `tap_of(prefix)`, four folders below the prefix.
    let prefix = tap.ancestors().nth(4).unwrap_or(tap);
    let old_system_git = |e: &String| e.contains(" is older than git ") && !prefix.join("bin/git").is_file();
    let git = crate::install::git_in(&[prefix.join("bin"), PathBuf::from("/usr/bin")]).map_err(|e| if old_system_git(&e) { format!("{e}; run brew install git") } else { e })?;
    let mut cmd = crate::spawn::cmd(git);
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
    // SAFETY: CommonCrypto, in libSystem, reads `len` bytes of `data` and writes 32 into `md`; `len as u32` holds only for inputs under 4 GB.
    unsafe { CC_SHA256(b.as_ptr().cast(), b.len() as u32, md.as_mut_ptr()) };
    md.iter().map(|x| format!("{x:02x}")).collect()
}

/// The checked archive where Homebrew looks for its download of `url` before fetching one.
/// ceiling: Homebrew's naming rule (`cached_location` in abstract_file_download_strategy.rb) copied, not asked; a Homebrew
/// that changes it fetches instead, which the profile refuses, so the upgrade fails closed until this rule follows.
fn prime(root: &Path, url: &str, bytes: &[u8]) -> Result<PathBuf, String> {
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
        out.write_all(bytes)?;
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
        /// The archive's checksum, as install.sh checked it.
        sha: String,
        /// The thread's TEST_HOME before this fake's, back when it drops.
        was: Option<PathBuf>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            crate::paths::TEST_HOME.with(|h| h.replace(self.was.take()));
        }
    }

    impl Fake {
        fn tap(&self) -> PathBuf {
            tap_of(&self.brew.prefix)
        }
        /// What the stand-in brew also does on `upgrade`, as sh.
        fn does(&self, sh: &str) {
            std::fs::write(self.root.join("does"), sh).unwrap();
        }
        /// What the stand-in brew does first on `config` or `--cache`, as sh.
        fn at(&self, cmd: &str, sh: &str) {
            std::fs::write(self.root.join(format!("at-{cmd}")), sh).unwrap();
        }
        /// sh for `upgrade`: a keg of 0.99.0 holding `nerd`, linked as Homebrew links it.
        fn keg(&self, nerd: &Path) -> String {
            let (p, k) = (self.brew.prefix.display(), self.brew.prefix.join("Cellar/nerd/0.99.0"));
            format!("mkdir -p '{k}/bin' && cp '{n}' '{k}/bin/nerd' && chmod 755 '{k}/bin/nerd' && ln -sfn '{k}' '{p}/opt/nerd'", k = k.display(), n = nerd.display())
        }
        /// sh for `upgrade`: the release's own nerd installed.
        fn installs(&self) -> String {
            self.keg(&self.root.join("build/nerd"))
        }
        fn cache(&self) -> PathBuf {
            self.root.join("home/Library/Caches/Homebrew")
        }
        fn formula_now(&self) -> String {
            std::fs::read_to_string(self.tap().join("Formula/nerd.rb")).unwrap()
        }
        fn saved(&self) -> PathBuf {
            saved_path(&self.brew.state)
        }
        fn rendered(&self) -> String {
            formula(TAG, NAME, &self.sha)
        }
    }

    /// A Homebrew prefix in a test folder holding nerd 0.21.0 from nerdterm/tap, its tap a git repository; a stand-in
    /// for sandbox-exec that runs the stand-in brew; a state folder and a HOME of its own; a checked release in `run/`.
    fn fake(tag: &str) -> Fake {
        let root = crate::paths::test_dir(tag).canonicalize().unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let was = crate::paths::TEST_HOME.with(|h| h.replace(Some(root.join("home"))));
        let prefix = root.join("prefix");
        let keg = prefix.join("Cellar/nerd/0.21.0");
        write(&keg.join("bin/nerd"), "", 0o755);
        write(&keg.join("INSTALL_RECEIPT.json"), r#"{"source":{"tap":"nerdterm/tap"}}"#, 0o644);
        let tap = tap_of(&prefix);
        let r = root.display();
        let brew = format!(
            "#!/bin/sh\ncase $1 in\n--version) if [ ! -d \"$HOMEBREW_TEMP\" ]; then echo 'Error: no HOMEBREW_TEMP folder' >&2; exit 1; fi; echo 'Homebrew 7.0.11'; echo 'Homebrew/homebrew-core' ;;\n\
             config) if [ -f '{r}/at-config' ]; then . '{r}/at-config'; fi; echo 'HOMEBREW_PREFIX: {p}'; echo \"HOMEBREW_TEMP: $HOMEBREW_TEMP\" ;;\n\
             --cache) if [ -f '{r}/at-cache' ]; then . '{r}/at-cache'; fi; echo \"$HOME/Library/Caches/Homebrew\" ;;\n\
             upgrade) cp '{t}/Formula/nerd.rb' '{r}/seen.rb'; echo \"$*\" > '{r}/upgraded'; if [ -f '{r}/does' ]; then . '{r}/does'; fi ;;\n*) exit 9 ;;\nesac\n",
            p = prefix.display(),
            t = tap.display()
        );
        write(&prefix.join("bin/brew"), &brew, 0o755);
        std::os::unix::fs::symlink(keg.join("bin/nerd"), prefix.join("bin/nerd")).unwrap();
        std::fs::create_dir_all(prefix.join("opt")).unwrap();
        std::os::unix::fs::symlink(&keg, prefix.join("opt/nerd")).unwrap();
        write(&root.join("sandbox-exec"), &format!("#!/bin/sh\n[ \"$1\" = -p ] || exit 97\nprintf '%s' \"$2\" > '{r}/profile'\nshift 2\nexec \"$@\"\n"), 0o755);
        write(&tap.join("README.md"), "# tap\n", 0o644);
        write(&tap.join("Formula/nerd.rb"), TAPS_OWN, 0o644);
        git(&tap, &["init", "-q"]);
        git(&tap, &["add", "-A"]);
        git(&tap, &["commit", "-qm", "tap"]);
        write(&root.join("build/nerd"), "#!/bin/sh\necho 'nerd 0.99.0'\n", 0o755);
        std::fs::create_dir_all(root.join("run")).unwrap();
        let archive = root.join(format!("run/{NAME}"));
        assert!(Command::new("/usr/bin/tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg(&archive).arg("-C").arg(root.join("build")).arg("nerd").status().unwrap().success());
        let sha = sha256_hex(&std::fs::read(&archive).unwrap());
        write(&root.join(format!("run/{NAME}.sha256")), &format!("{sha}  {NAME}\n"), 0o600);
        Fake { brew: Brew { prefix, run: root.join("sandbox-exec"), state: root.join("state") }, root, sha, was }
    }

    fn upgrade(f: &Fake, limit: Duration) -> Result<String, String> {
        f.brew.upgrade(TAG, NAME, &f.root.join("run"), limit)
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
        assert_eq!(formula(TAG, NAME, SHA), String::from_utf8(out.stdout).unwrap());
        assert!(formula(TAG, NAME, SHA).contains(&format!("  url \"{}\"\n", super::super::asset_url(TAG, NAME))), "the url Homebrew's cache is named after");
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
        let url = super::super::asset_url(TAG, NAME);
        let key = sha256_hex(url.as_bytes());
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let downloads = root.join("cache/downloads");
        let listed = || {
            let mut l: Vec<String> = std::fs::read_dir(&downloads).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            l.sort();
            l
        };

        let p = prime(&root.join("cache"), &url, b"verified").unwrap();
        assert_eq!(p, downloads.join(format!("{key}--{NAME}")), "none yet: the url's own name");
        assert_eq!(std::fs::read(&p).unwrap(), b"verified");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);

        std::fs::remove_file(&p).unwrap();
        write(&downloads.join(format!("{key}--older-name.tar.gz")), "old", 0o644);
        write(&downloads.join(format!("{key}--{NAME}.incomplete")), "half", 0o644);
        let p = prime(&root.join("cache"), &url, b"verified").unwrap();
        assert_eq!(p, downloads.join(format!("{key}--older-name.tar.gz")), "the one there is the one Homebrew takes");
        assert_eq!(std::fs::read(&p).unwrap(), b"verified");
        assert_eq!(listed(), [format!("{key}--{NAME}.incomplete"), format!("{key}--older-name.tar.gz")], "an .incomplete is not a download; nothing else left");

        write(&downloads.join(format!("{key}--second.tar.gz")), "old", 0o644);
        let p = prime(&root.join("cache"), &url, b"verified").unwrap();
        assert_eq!(p, downloads.join(format!("{key}--{NAME}")), "two: both gone, the url's own name");
        assert_eq!(listed(), [format!("{key}--{NAME}"), format!("{key}--{NAME}.incomplete")]);
    }

    #[test]
    fn nerd_starts_brew_with_no_network_and_a_fixed_environment() {
        assert_eq!(PROFILE, r#"(version 1)(allow default)(deny network*)(allow network-bind network-inbound (local unix-socket))(allow process-exec (literal "/usr/bin/sandbox-exec") (with no-sandbox))"#);
        let b = Brew { prefix: "/opt/homebrew".into(), run: "/usr/bin/sandbox-exec".into(), state: "/x".into() };
        for args in [&["--cache"][..], &["--version"], &["config"], &["upgrade", "nerdterm/tap/nerd"]] {
            let cmd = b.cmd(args, Path::new("/x/release-1/tmp"));
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
                ("HOMEBREW_TEMP", "/x/release-1/tmp"),
                ("TMPDIR", "/x/release-1/tmp"),
            ];
            for (k, v) in fixed {
                assert_eq!(env.get(k), Some(&Some(v.to_string())), "{k}");
            }
            assert_eq!(env.get("HOME"), Some(&crate::paths::home().map(|h| h.to_string_lossy().into_owned())));
            let others: Vec<&String> = env.keys().filter(|k| !fixed.iter().any(|(f, _)| f == k) && !["HOME", "USER", "LOGNAME"].contains(&k.as_str())).collect();
            assert!(others.is_empty(), "nothing else: {others:?}");
            assert!(env.values().all(Option::is_some), "a clean slate, not removals from nerd's own");
        }
    }

    #[test]
    fn a_run_past_its_limit_is_stopped_with_everything_it_started() {
        let root = crate::paths::test_dir("brew-stop");
        let kid = root.join("kid");
        // Renamed into place: a kid file is a whole pid of a process that already left the group and ignores TERM.
        let sh = format!("#!/bin/sh\ntrap '' TERM\n/usr/bin/perl -e '$SIG{{TERM}}=\"IGNORE\"; setpgrp(0,0); open(F, \">\", \"$ARGV[0].t\"); print F $$; close F; rename(\"$ARGV[0].t\", $ARGV[0]); sleep 60' '{}' &\nsleep 60\n", kid.display());
        write(&root.join("hang"), &sh, 0o755);
        // A busy machine can stop the run before perl got there, and then the case never happened: again, with twice the limit.
        let mut limit = 1;
        loop {
            let start = Instant::now();
            let r = run(Command::new(root.join("hang")), Duration::from_secs(limit), &root);
            assert!(r.as_ref().is_err_and(|e| e.starts_with(&format!("did not finish in {limit} s"))), "{r:?}");
            assert!(start.elapsed() < Duration::from_secs(limit) + GRACE + Duration::from_secs(2));
            if kid.exists() || limit == 16 {
                break;
            }
            limit *= 2;
        }
        let pid = nix::unistd::Pid::from_raw(std::fs::read_to_string(&kid).expect("perl did not leave the group before a 16 s limit").trim().parse().unwrap());
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
        f.does(&f.installs());
        assert_eq!(upgrade(&f, BREW_LIMIT), Ok("nerd 0.99.0".into()));
        assert_eq!(std::fs::read_to_string(f.root.join("seen.rb")).unwrap(), f.rendered(), "brew ran with nerd's formula");
        assert_eq!(std::fs::read_to_string(f.root.join("upgraded")).unwrap(), "upgrade nerdterm/tap/nerd\n");
        assert_eq!(std::fs::read_to_string(f.root.join("profile")).unwrap(), PROFILE);
        assert_eq!(f.formula_now(), TAPS_OWN, "the tap's own formula back");
        let key = sha256_hex(super::super::asset_url(TAG, NAME).as_bytes());
        assert_eq!(std::fs::read(f.cache().join(format!("downloads/{key}--{NAME}"))).unwrap(), std::fs::read(f.root.join(format!("run/{NAME}"))).unwrap());
        assert!(!f.saved().exists(), "the saved copy is gone");
        assert_eq!(std::fs::metadata(f.root.join("run/tmp")).unwrap().permissions().mode() & 0o777, 0o700, "Homebrew staged in nerd's own folder");
    }

    #[test]
    fn a_brew_that_would_stage_or_cache_elsewhere_is_refused_before_the_tap_is_touched() {
        let untouched = |f: &Fake| {
            assert_eq!(f.formula_now(), TAPS_OWN);
            assert!(!f.root.join("upgraded").exists() && !f.saved().exists() && !f.cache().exists(), "no brew upgrade, nothing primed");
        };
        let staged = "failed at Homebrew's temp folder: Homebrew stages in /private/tmp, not in nerd's own folder (HOMEBREW_TEMP in a brew.env, or the folder not writable); remove HOMEBREW_TEMP from your brew.env, or run brew update && brew upgrade nerd";
        let f = fake("brew-temp");
        f.at("config", "echo 'HOMEBREW_TEMP: /private/tmp'; exit 0");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(staged.into()), "a brew.env's HOMEBREW_TEMP");
        untouched(&f);
        f.at("config", "echo 'HOMEBREW_PREFIX: /opt/homebrew'; exit 0");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(staged.into()), "none shown: Homebrew's default");
        untouched(&f);

        let f = fake("brew-cache-elsewhere");
        f.at("cache", "echo /Users/someone/Homebrew; exit 0");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at Homebrew's cache: Homebrew's cache is /Users/someone/Homebrew, not ~/Library/Caches/Homebrew; remove HOMEBREW_CACHE from your brew.env, or run brew update && brew upgrade nerd".into()));
        untouched(&f);
    }

    #[test]
    fn an_archive_that_changed_since_its_check_never_reaches_homebrew_s_cache() {
        let f = fake("brew-swapped");
        f.at("cache", &format!("printf 'swapped' > '{}'", f.root.join(format!("run/{NAME}")).display()));
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at checksum: the archive changed after install.sh checked it; try again".into()));
        assert!(!f.cache().exists(), "nothing in the cache");
        assert_eq!(f.formula_now(), TAPS_OWN);
        assert!(!f.root.join("upgraded").exists());
    }

    #[test]
    fn an_archive_tar_cannot_read_nerd_from_stops_the_run_before_the_tap_is_touched() {
        let f = fake("brew-no-nerd");
        std::fs::rename(f.root.join("build/nerd"), f.root.join("build/other")).unwrap();
        let archive = f.root.join(format!("run/{NAME}"));
        assert!(Command::new("/usr/bin/tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg(&archive).arg("-C").arg(f.root.join("build")).arg("other").status().unwrap().success());
        std::fs::write(f.root.join(format!("run/{NAME}.sha256")), format!("{}  {NAME}\n", sha256_hex(&std::fs::read(&archive).unwrap()))).unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at the archive: tar: nerd: Not found in archive; try again".into()));
        assert!(!f.cache().exists() && !f.root.join("upgraded").exists() && !f.saved().exists(), "nothing primed, no brew upgrade");
        assert_eq!(f.formula_now(), TAPS_OWN);
    }

    #[test]
    fn homebrew_s_temp_folder_that_cannot_be_made_stops_the_run_before_brew() {
        let f = fake("brew-temp-file");
        write(&f.root.join("run/tmp"), "", 0o600);
        let e = upgrade(&f, BREW_LIMIT).unwrap_err();
        assert!(e.starts_with(&format!("failed at Homebrew's temp folder: {}: ", f.root.join("run/tmp").display())) && e.ends_with("; try again"), "{e}");
        assert!(!f.root.join("profile").exists(), "no brew ran");
    }

    #[test]
    fn after_the_upgrade_homebrew_s_nerd_must_be_the_checked_release_and_is_never_run() {
        let f = fake("brew-nothing");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(format!("failed at brew upgrade: Homebrew did not install nerd 0.99.0: {} leads to {}; run brew update && brew upgrade nerd", f.brew.prefix.join("opt/nerd").display(), f.brew.prefix.join("Cellar/nerd/0.21.0").display())), "brew did nothing and said nothing");
        assert_eq!(f.formula_now(), TAPS_OWN);

        let f = fake("brew-other-bytes");
        let ran = f.root.join("ran");
        write(&f.root.join("other"), &format!("#!/bin/sh\ntouch '{}'\necho 'nerd 0.99.0'\n", ran.display()), 0o755);
        f.does(&f.keg(&f.root.join("other")));
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew upgrade: the nerd Homebrew installed is not the release nerd checked; it will not run: run brew reinstall nerd".into()));
        let exe = f.brew.prefix.join("Cellar/nerd/0.99.0/bin/nerd");
        assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o644, "kept, not runnable");
        assert!(!ran.exists(), "nerd ran nothing Homebrew installed");
        assert_eq!(f.formula_now(), TAPS_OWN);

        // A link Homebrew moved into the keg as it was staged: whatever it leads to must not run through <prefix>/bin/nerd.
        let f = fake("brew-link");
        let (k, p) = (f.brew.prefix.join("Cellar/nerd/0.99.0"), f.brew.prefix.clone());
        let planted = f.root.join("planted");
        write(&planted, &format!("#!/bin/sh\ntouch '{}'\n", f.root.join("ran").display()), 0o755);
        f.does(&format!("mkdir -p '{k}/bin' && ln -s '{}' '{k}/bin/nerd' && ln -sfn '{k}' '{p}/opt/nerd' && ln -sfn '{k}/bin/nerd' '{p}/bin/nerd'", planted.display(), k = k.display(), p = p.display()));
        let bin = k.join("bin");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(format!("failed at brew upgrade: Homebrew's nerd 0.99.0 has no plain file at {}; it will not run: run chmod 755 {}, then brew reinstall nerd", bin.join("nerd").display(), bin.display())));
        assert_eq!(std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777, 0o644, "the keg's bin cannot be searched");
        assert!(Command::new(p.join("bin/nerd")).status().is_err(), "nothing runs through Homebrew's link");
        assert!(!f.root.join("ran").exists(), "the planted program never ran");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let f = fake("brew-unreadable");
        f.does(&format!("{} && chmod 311 '{}'", f.installs(), f.brew.prefix.join("Cellar/nerd/0.99.0/bin/nerd").display()));
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew upgrade: the nerd Homebrew installed is not the release nerd checked; it will not run: run brew reinstall nerd".into()), "a keg nerd cannot read");
        assert_eq!(std::fs::metadata(f.brew.prefix.join("Cellar/nerd/0.99.0/bin/nerd")).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn a_pinned_nerd_is_told_to_unpin() {
        let f = fake("brew-pinned");
        f.does("echo 'Error: Not upgrading 1 pinned package:' >&2; echo 'nerd 0.21.0' ; exit 1");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew upgrade: Homebrew said \"Error: Not upgrading 1 pinned package:\"; run brew unpin nerd, then brew update && brew upgrade nerd".into()));
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
        assert!(!f.root.join("upgraded").exists() && !f.cache().exists(), "no brew upgrade, nothing primed");
        assert_eq!(f.formula_now(), TAPS_OWN);
    }

    #[test]
    fn a_change_to_the_tap_during_the_upgrade_wins() {
        let f = fake("brew-race");
        f.does(&format!("{}; printf 'upstream\\n' > '{}'", f.installs(), f.tap().join("Formula/nerd.rb").display()));
        assert!(upgrade(&f, BREW_LIMIT).is_ok());
        assert_eq!(f.formula_now(), "upstream\n", "not overwritten");
        assert!(!f.saved().exists());
    }

    #[test]
    fn a_killed_run_s_formula_is_put_back_by_the_next_run() {
        let f = fake("brew-killed");
        f.does(&f.installs());
        let saved = [sha256_hex(f.rendered().as_bytes()).as_bytes(), b"\n", TAPS_OWN.as_bytes()].concat();
        std::fs::write(f.tap().join("Formula/nerd.rb"), f.rendered()).unwrap();
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
        f.does(&f.installs());
        let saved = [sha256_hex(f.rendered().as_bytes()).as_bytes(), b"\n", TAPS_OWN.as_bytes()].concat();
        std::fs::write(f.tap().join("Formula/nerd.rb"), f.rendered()).unwrap();
        crate::paths::write_private(&f.saved(), "tmp", &saved).unwrap();
        git(&f.tap(), &["commit", "-qam", "nerd 0.99.0"]);
        assert_eq!(upgrade(&f, BREW_LIMIT), Ok("nerd 0.99.0".into()));
        assert_eq!(f.formula_now(), f.rendered(), "upstream's commit, not the older bytes");
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
        let f = fake("brew-config-fails");
        f.at("config", "echo 'Error: no config' >&2; exit 1");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew config: Homebrew said \"Error: no config\"; try again".into()));
        assert!(!f.cache().exists());
        untouched(&f);

        let f = fake("brew-archive-gone");
        std::fs::remove_file(f.root.join(format!("run/{NAME}"))).unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err(format!("failed at checksum: {}: No such file or directory (os error 2); try again", f.root.join(format!("run/{NAME}")).display())));
        assert!(!f.cache().exists());
        untouched(&f);

        let f = fake("brew-sum");
        std::fs::write(f.root.join(format!("run/{NAME}.sha256")), format!("{SHA}  other.tar.gz\n")).unwrap();
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at checksum: the checksum file is not one line of shasum".into()));
        assert!(!f.cache().exists());
        untouched(&f);

        let f = fake("brew-cache-fails");
        f.at("cache", "echo 'Error: no cache' >&2; exit 1");
        assert_eq!(upgrade(&f, BREW_LIMIT), Err("failed at brew --cache: Homebrew said \"Error: no cache\"".into()));
        assert!(!f.cache().exists());
        untouched(&f);

        let f = fake("brew-put");
        std::fs::set_permissions(f.tap().join("Formula/nerd.rb"), std::fs::Permissions::from_mode(0o444)).unwrap();
        let e = upgrade(&f, BREW_LIMIT).unwrap_err();
        assert!(e.starts_with("failed at formula: "), "{e}");
        assert_eq!(std::fs::read_dir(f.cache().join("downloads")).unwrap().count(), 0, "the primed archive removed");
        untouched(&f);
    }
}
