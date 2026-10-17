//! The daemon's log file, and what the client reads back from it.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// What `nerd --disconnect` and the menu's farewell tell the human about a stranger's ssh.
pub const HYGIENE: [&str; 2] = [
    "If you did not start the other connections: turn off Remote Login (System Settings > General > Sharing)",
    "and remove keys you do not know from the authorized_keys file in your .ssh folder.",
];

/// The daemon writes it at a fresh start and leaves it through an in-place update.
pub fn sandbox_path() -> std::path::PathBuf {
    crate::session::state_dir().join("sandbox")
}

/// From the daemon's file, or from its log when a server started by an older build wrote no file.
pub fn sandbox_status() -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(sandbox_path()) {
        return Some(escape(s.trim()).into_owned());
    }
    let text = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).ok()?;
    text.lines().rev().find_map(|l| parse(l)?.text.strip_prefix("sandbox: ").map(str::to_string))
}

/// RFC 5424 severities, the four nerd uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

pub fn error(line: &str) {
    log(Level::Error, line)
}

pub fn warn(line: &str) {
    log(Level::Warn, line)
}

pub fn info(line: &str) {
    log(Level::Info, line)
}

pub fn debug(line: &str) {
    log(Level::Debug, line)
}

static ROLE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// `server`, `broker`, `client` or `cli`, once per process at its entry point.
pub fn set_role(role: &'static str) {
    let _ = ROLE.set(role);
}

/// A log line of either format: `<RFC 3339> <LEVEL> <role[pid]> <text>`, or an older build's `<unix seconds> <text>`.
pub struct Line<'a> {
    pub secs: i64,
    /// An older build's line, with no level or role.
    pub old: bool,
    pub text: &'a str,
}

pub fn parse(line: &str) -> Option<Line<'_>> {
    let (ts, rest) = line.split_once(' ')?;
    if let Ok(secs) = ts.parse::<i64>() {
        return Some(Line { secs, old: true, text: rest });
    }
    let (level, rest) = rest.split_once(' ')?;
    if !["ERROR", "WARN", "INFO", "DEBUG"].contains(&level) {
        return None;
    }
    let (_role, text) = rest.trim_start_matches(' ').split_once(' ')?;
    Some(Line { secs: rfc3339_secs(ts)?, old: false, text })
}

/// `YYYY-MM-DDTHH:MM:SS.mmm±HH:MM` in local time, with the local offset.
pub fn stamp(secs: i64, ms: u32) -> String {
    let t = secs as nix::libc::time_t;
    // SAFETY: localtime_r writes only into the struct it is given.
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    // A time localtime cannot convert still prints a valid stamp: the epoch.
    if unsafe { nix::libc::localtime_r(&t, &mut tm) }.is_null() {
        return stamp(0, 0);
    }
    let off = tm.tm_gmtoff / 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{ms:03}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60
    )
}

fn rfc3339_secs(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != 29 || b[10] != b'T' || !matches!(b[23], b'+' | b'-') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i32>().ok();
    // SAFETY: timegm reads the struct it is given and normalises it in place.
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    (tm.tm_year, tm.tm_mon, tm.tm_mday) = (n(0..4)? - 1900, n(5..7)? - 1, n(8..10)?);
    (tm.tm_hour, tm.tm_min, tm.tm_sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let off = i64::from(n(24..26)? * 3600 + n(27..29)? * 60);
    let utc = unsafe { nix::libc::timegm(&mut tm) };
    Some(utc - if b[23] == b'-' { -off } else { off })
}

/// Control characters as escapes, so text from outside can neither start a line of its own nor drive a terminal.
pub fn escape(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(crate::render::is_spoofing) {
        return s.into();
    }
    s.chars().map(|c| if crate::render::is_spoofing(c) { c.escape_default().to_string() } else { c.to_string() }).collect::<String>().into()
}

/// A warning a plugin can cause at will: at most one a second of each kind, or it could roll the log over.
pub fn warn_limited(kind: &'static str, line: &str) {
    static LAST: std::sync::Mutex<Vec<(&'static str, std::time::Instant, u64)>> = std::sync::Mutex::new(Vec::new());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let held = match last.iter_mut().find(|(k, _, _)| *k == kind) {
        Some((_, at, held)) if at.elapsed() < std::time::Duration::from_secs(1) => {
            *held += 1;
            return;
        }
        Some((_, at, held)) => {
            *at = std::time::Instant::now();
            std::mem::take(held)
        }
        None => {
            last.push((kind, std::time::Instant::now(), 0));
            0
        }
    };
    drop(last);
    // The ones held back are counted, so a flood cannot hide that it happened.
    match held {
        0 => warn(line),
        n => warn(&format!("{line} (and {n} more like it before)")),
    }
}

/// One line at most this long: a plugin's words could otherwise roll the whole log over in two lines.
const LINE_MAX: usize = 1024;

/// Past this the log is rolled into .old, so it can't grow without bound.
const LOG_LIMIT: u64 = 1 << 20;

/// The daemon has no tty to complain to, so this log is the only record of why it did something.
/// Kept open; reopened when the path is not the open file any more (moved, deleted, rotated) or reaches the limit.
pub fn log(level: Level, line: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt;
    static OPEN: std::sync::Mutex<Option<(std::path::PathBuf, std::fs::File, u64, u64)>> = std::sync::Mutex::new(None);
    let path = log_path(crate::session::state_dir(), crate::paths::state_root());
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    let ino = std::fs::metadata(&path).map(|m| m.ino()).ok();
    if open.as_ref().is_none_or(|(p, _, len, i)| *p != path || *len > LOG_LIMIT || Some(*i) != ino) {
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
        *open = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path).ok().map(|f| {
            // A file an older build made keeps its mode through `open`.
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            let (len, ino) = f.metadata().map(|m| (m.len(), m.ino())).unwrap_or((0, 0));
            (path, f, len, ino)
        });
    }
    if let Some((_, f, len, _)) = open.as_mut() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let role = ROLE.get().copied().unwrap_or("cli");
        let line = escape(line);
        let line = match line.char_indices().nth(LINE_MAX) {
            Some((at, _)) => format!("{}… ({} bytes)", &line[..at], line.len()),
            None => line.into_owned(),
        };
        let text = format!("{} {:<5} {role}[{}] {line}\n", stamp(now.as_secs() as i64, now.subsec_millis()), level.name(), std::process::id());
        if f.write_all(text.as_bytes()).is_ok() {
            *len += text.len() as u64;
        }
    }
}

/// An inherited `NERD_INSTANCE` with no folder (its server under another HOME) logs to the root: a folder made here would list as a server.
fn log_path(dir: std::path::PathBuf, root: std::path::PathBuf) -> std::path::PathBuf {
    if dir.is_dir() {
        return dir.join("daemon.log");
    }
    let _ = std::fs::create_dir_all(&root);
    root.join("daemon.log")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_for_an_instance_with_no_folder_lands_in_the_root() {
        crate::session::tests::with_temp_state_dir("log-no-instance", || {
            let root = crate::session::state_dir().join("fresh");
            let gone = root.join("instances/7");
            assert_eq!(super::log_path(gone.clone(), root.clone()), root.join("daemon.log"));
            assert!(root.is_dir() && !gone.exists(), "the root is made, the instance is not");
            std::fs::create_dir_all(&gone).unwrap();
            assert_eq!(super::log_path(gone.clone(), root), gone.join("daemon.log"));
        });
    }

    #[test]
    fn a_warning_a_plugin_can_repeat_is_written_once_a_second() {
        crate::session::tests::with_temp_state_dir("log-limited", || {
            // Words no other test logs: their lines land in this file while it is the state dir.
            for i in 0..5000 {
                super::warn_limited("test flood", &format!("flooded {i}"));
            }
            super::warn_limited("test other", "another kind");
            std::thread::sleep(std::time::Duration::from_millis(1100));
            super::warn_limited("test flood", "flooded again");
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert_eq!(log.lines().filter(|l| l.contains("flooded ")).count(), 2, "{log}");
            assert!(log.contains("flooded again (and 4999 more like it before)"), "{log}");
            assert!(log.contains("another kind"), "each kind has its own second");
        });
    }

    #[test]
    fn a_plugins_words_cannot_roll_the_log_over_or_turn_it() {
        crate::session::tests::with_temp_state_dir("log-cap", || {
            super::info(&format!("unknown target {}", "x".repeat(3_000_000)));
            super::info("unknown role a\u{202E}b");
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            let lines: Vec<&str> = log.lines().collect();
            assert!(lines[0].len() < 1200 && lines[0].ends_with("(3000015 bytes)"), "{}", &lines[0][..80]);
            assert!(lines[1].contains("a\\u{202e}b") && !lines[1].contains('\u{202E}'), "{}", lines[1]);
        });
    }

    #[test]
    fn a_line_from_outside_cannot_start_a_line_of_its_own() {
        crate::session::tests::with_temp_state_dir("log-escape", || {
            super::info("sandbox: did NOT enable (x), running without it");
            super::info("client 1 attached: from /tmp/a\n1 sandbox: enabled\r\x1b[2Jé");
            let text = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert_eq!(text.lines().count(), 2, "{text:?}");
            assert!(text.contains("/tmp/a\\n1 sandbox: enabled\\r\\u{1b}[2Jé"), "{text:?}");
            assert_eq!(super::sandbox_status().as_deref(), Some("did NOT enable (x), running without it"));
            crate::paths::write_private(&super::sandbox_path(), "tmp", b"disabled in config\n").unwrap();
            assert_eq!(super::sandbox_status().as_deref(), Some("disabled in config"), "the file wins over the log");
        });
    }

    #[test]
    fn a_line_has_the_time_the_level_and_the_role() {
        crate::session::tests::with_temp_state_dir("log-format", || {
            super::set_role("cli");
            super::warn("sandbox: did NOT enable (y), running without it");
            let text = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            let line = text.lines().next().unwrap();
            // YYYY-MM-DDTHH:MM:SS.mmm+HH:MM, then the level padded to five, then role[pid].
            let shape: String = line[..29].chars().map(|c| if c.is_ascii_digit() { '9' } else { c }).collect();
            assert!(shape == "9999-99-99T99:99:99.999+99:99" || shape == "9999-99-99T99:99:99.999-99:99", "{line}");
            assert_eq!(&line[29..], format!(" WARN  cli[{}] sandbox: did NOT enable (y), running without it", std::process::id()));
            let parsed = super::parse(line).unwrap();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
            assert!((now - parsed.secs).abs() <= 2 && !parsed.old, "{} vs {now}", parsed.secs);
            assert_eq!(super::sandbox_status().as_deref(), Some("did NOT enable (y), running without it"), "a new line is read");
            assert_eq!(super::parse(&super::stamp(1790438984, 5)).map(|_| ()), None, "a stamp alone is no line");
            assert_eq!(super::parse(&format!("{} INFO  server[1] x", super::stamp(1790438984, 5))).unwrap().secs, 1790438984);
        });
    }

    #[test]
    fn an_older_builds_line_is_still_read() {
        crate::session::tests::with_temp_state_dir("log-old-line", || {
            std::fs::write(crate::session::state_dir().join("daemon.log"), "1790438984 sandbox: disabled in config\n").unwrap();
            assert_eq!(super::sandbox_status().as_deref(), Some("disabled in config"));
            let l = super::parse("1790438984 sandbox: disabled in config").unwrap();
            assert!(l.old && l.secs == 1790438984 && l.text == "sandbox: disabled in config");
        });
    }

    #[test]
    fn the_sandbox_status_outlives_the_log() {
        crate::session::tests::with_temp_state_dir("sandbox-roll", || {
            crate::paths::write_private(&super::sandbox_path(), "tmp", b"enabled, network denied to the daemon").unwrap();
            let path = crate::session::state_dir().join("daemon.log");
            super::info("sandbox: enabled, network denied to the daemon");
            let long = "x".repeat(4096);
            while !path.with_extension("log.old").exists() {
                super::info(&long);
            }
            super::info("after");
            assert_eq!(super::sandbox_status().as_deref(), Some("enabled, network denied to the daemon"));
        });
    }

    #[test]
    fn a_log_deleted_from_outside_comes_back_with_the_next_line() {
        crate::session::tests::with_temp_state_dir("log-deleted", || {
            let path = crate::session::state_dir().join("daemon.log");
            super::info("before");
            std::fs::remove_file(&path).unwrap();
            super::info("after");
            assert!(std::fs::read_to_string(&path).unwrap().ends_with(" after\n"));
        });
    }

    #[test]
    fn the_log_rolls_over_at_its_limit_and_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        crate::session::tests::with_temp_state_dir("log-roll", || {
            let path = crate::session::state_dir().join("daemon.log");
            super::info("first");
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            let long = "x".repeat(4096);
            while !path.with_extension("log.old").exists() {
                assert!(std::fs::metadata(&path).unwrap().len() < 2 * super::LOG_LIMIT, "never rolled");
                super::info(&long);
            }
            super::info("after");
            let now = std::fs::read_to_string(&path).unwrap();
            assert!(now.ends_with(" after\n") && now.len() < 3 * 4096, "a fresh file: {} bytes", now.len());
            assert!(std::fs::read_to_string(path.with_extension("log.old")).unwrap().contains(" first\n"));
        });
    }
}