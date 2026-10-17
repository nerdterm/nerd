//! Where nerd keeps its files, and the one way it writes a private one.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

// Per thread, so a test gets a HOME of its own without touching the process environment others read.
#[cfg(test)]
thread_local! { pub(crate) static TEST_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) }; }

/// A fresh folder under temp_dir for one test; a counter, not the clock, keeps two tests from sharing it.
#[cfg(test)]
pub(crate) fn test_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nerd-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// HOME as the process got it; each caller keeps its own fallback.
pub fn home() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(h) = TEST_HOME.with(|h| h.borrow().clone()) {
        return Some(h);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(test)]
    let home = Some(home.filter(|h| h.starts_with(std::env::temp_dir())).unwrap_or_else(|| std::env::temp_dir().join(format!("nerd-test-home-{}", std::process::id()))));
    home
}

/// nerd's folder under a HOME: config.json, the default state root, the hooks' write root.
pub fn nerd_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/nerd")
}

static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The `paths` setting, set where the frame is drawn so every place that shows a path follows it.
pub fn set_full(on: bool) {
    FULL.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A path as nerd shows it, by the `paths` setting.
pub fn shown(p: &Path) -> String {
    shown_as(p, FULL.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn shown_as(p: &Path, full: bool) -> String {
    let rest = home().filter(|h| !full && h.parent().is_some()).and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf));
    match rest {
        Some(r) if r.as_os_str().is_empty() => "~".into(),
        Some(r) => format!("~/{}", r.display()),
        None => p.display().to_string(),
    }
}

/// A path typed by a human: `~/`, absolute, or relative to `base`.
pub fn resolve(base: &Path, s: &str) -> PathBuf {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("~/") {
        return home().unwrap_or_default().join(rest);
    }
    let p = PathBuf::from(s);
    if p.is_absolute() { p } else { base.join(p) }
}

/// The env var lets tests override the state root without touching process-wide $HOME.
pub fn state_root() -> PathBuf {
    if let Some(p) = std::env::var_os("NERD_STATE_DIR") {
        return PathBuf::from(p);
    }
    // launchd can start us with no HOME; fall back to /tmp instead of panicking.
    nerd_dir(&home().unwrap_or_else(std::env::temp_dir))
}

/// Under HOME, or `/` without one; `NERD_STATE_DIR` does not move it.
pub fn config_path() -> PathBuf {
    nerd_dir(&home().unwrap_or_else(|| PathBuf::from("/"))).join("config.json")
}

/// Via a 0600 temp file (`path` with extension `tmp_ext`) and a rename: no reader sees a partial or wrong-mode file.
pub fn write_private(path: &Path, tmp_ext: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(tmp_ext);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    // A temp file left by an older build keeps its mode through `open`.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_path_shows_from_home_or_in_full() {
        let home = super::home().unwrap();
        let p = home.join("dev/nerd");
        assert_eq!(super::shown_as(&p, false), "~/dev/nerd");
        assert_eq!(super::shown_as(&home, false), "~");
        assert_eq!(super::shown_as(&p, true), p.display().to_string());
        assert_eq!(super::shown_as(std::path::Path::new("/opt/x"), false), "/opt/x", "outside home, in full either way");
        assert_eq!(super::resolve(std::path::Path::new("/"), "~/dev/nerd"), p, "typing ~/ is understood in either mode");
    }

    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_private_creates_the_file_0600() {
        let dir = std::env::temp_dir().join(format!("nerd-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("7.txt");
        write_private(&p, "tmp", b"secret").unwrap();
        assert_eq!(mode(&p), 0o600, "mode {:o}", mode(&p));
        assert_eq!(std::fs::read(&p).unwrap(), b"secret");
        assert!(!p.with_extension("tmp").exists(), "the temporary file is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_temporary_file_is_named_by_the_caller_and_ends_0600() {
        let dir = std::env::temp_dir().join(format!("nerd-private-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, ext, tmp) in [("session.json", "tmp", "session.tmp"), ("config.json", "json.tmp", "config.json.tmp"), ("name", "tmp", "name.tmp")] {
            let p = dir.join(name);
            // A directory in the temp file's place makes the write fail: that pins the name.
            std::fs::create_dir(dir.join(tmp)).unwrap();
            assert!(write_private(&p, ext, b"x").is_err(), "{tmp}");
            std::fs::remove_dir(dir.join(tmp)).unwrap();
            // A stale world-readable temp file does not pass its mode on.
            std::fs::write(dir.join(tmp), "old").unwrap();
            std::fs::set_permissions(dir.join(tmp), std::fs::Permissions::from_mode(0o644)).unwrap();
            write_private(&p, ext, b"new").unwrap();
            assert_eq!(mode(&p), 0o600, "{name}");
            assert_eq!(std::fs::read(&p).unwrap(), b"new");
            assert!(!dir.join(tmp).exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_test_never_resolves_a_path_under_the_real_home() {
        let Some(real) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| !h.starts_with(std::env::temp_dir())) else { return };
        for p in [state_root(), config_path(), crate::session::state_dir(), crate::session::socket_path(), crate::hook::state_dir()] {
            assert!(!p.starts_with(&real), "{p:?} is under the real HOME {real:?}");
        }
    }

    #[test]
    fn the_config_stays_under_home_whatever_the_state_root() {
        crate::session::tests::with_temp_state_dir("config-path", || {
            assert_eq!(state_root(), PathBuf::from(std::env::var_os("NERD_STATE_DIR").unwrap()));
            assert!(!config_path().starts_with(state_root()));
            assert!(config_path().ends_with("Library/Application Support/nerd/config.json"));
        });
    }
}
