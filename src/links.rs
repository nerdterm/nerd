//! Ctrl+click on a URL or path inside a pane.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Url(String),
    Path {
        path: String,
        /// From `path:line` or `path:line:col`; the column isn't needed by callers, so it isn't stored.
        line: Option<u32>,
    },
}

pub fn link_at(screen: crate::term::Screen<'_>, row: u16, col: u16) -> Option<Link> {
    let (rows, cols) = screen.size();
    if row >= rows || col >= cols || is_blank(screen, row, col) {
        return None;
    }
    let mut l = col;
    while l > 0 && !is_blank(screen, row, l - 1) {
        l -= 1;
    }
    let mut r = col;
    while r + 1 < cols && !is_blank(screen, row, r + 1) {
        r += 1;
    }

    let mut token = String::new();
    for c in l..=r {
        match screen.cell(row, c) {
            Some(cell) if cell.is_wide_continuation() => {}
            Some(cell) if cell.has_contents() => token.push_str(cell.contents()),
            _ => {}
        }
    }

    let trimmed = token
        .trim_start_matches(['(', '[', '<', '"'])
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '\'', '"']);
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if ["http://", "https://", "file://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return Some(Link::Url(trimmed.to_string()));
    }

    let (path, line) = strip_line_suffix(trimmed);
    is_path_like(path).then(|| Link::Path { path: path.to_string(), line })
}

/// A wide character's second cell doesn't count as blank - it holds the same character as the first half.
fn is_blank(screen: crate::term::Screen<'_>, row: u16, col: u16) -> bool {
    match screen.cell(row, col) {
        None => true,
        Some(c) if c.is_wide_continuation() => false,
        Some(c) => !c.has_contents() || c.contents() == " ",
    }
}

/// Strips a trailing `:N` or `:N:M`; the column is dropped, only the line number is kept.
fn strip_line_suffix(s: &str) -> (&str, Option<u32>) {
    let Some(idx) = s.rfind(':') else { return (s, None) };
    let (head, tail) = (&s[..idx], &s[idx + 1..]);
    let Ok(n) = tail.parse::<u32>() else { return (s, None) };
    if let Some(idx2) = head.rfind(':')
        && let Ok(n1) = head[idx2 + 1..].parse::<u32>()
    {
        return (&head[..idx2], Some(n1));
    }
    (head, Some(n))
}

fn is_path_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s.starts_with("../") {
        return true;
    }
    s.contains('/') && s.rsplit('/').next().is_some_and(|last| last.contains('.') && last.len() > 1)
}

/// A relative path resolves against the pane's `cwd`, `~/` against the user's home directory.
pub fn open(link: &Link, cwd: &Path) -> Result<(), String> {
    let target = match link {
        Link::Url(u) => u.clone(),
        Link::Path { path, .. } => {
            let resolved = resolve_path(path, cwd);
            if !resolved.exists() {
                return Err(format!("path not found: {}", resolved.display()));
            }
            resolved.display().to_string()
        }
    };
    if holds_an_address(&target) {
        return Err("this file opens an address nerd does not show; open it from Finder".into());
    }
    spawn_open(open_command(&[&target]))?;
    crate::log::info(&format!("link opened {}", said(&target)));
    Ok(())
}

/// Files whose app opens an address or a path written inside them, which nerd never showed.
const LOCATIONS: [&str; 8] = [".webloc", ".inetloc", ".url", ".fileloc", ".ftploc", ".afploc", ".vncloc", ".mailloc"];

/// Judged on the file a path or `file://` link really names: decoded, without its query, through links.
fn holds_an_address(target: &str) -> bool {
    let path = match target.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")) {
        Some(_) => decoded(target[7..].split(['?', '#']).next().unwrap_or("")),
        None if target.contains("://") || target.to_ascii_lowercase().starts_with("mailto:") => return false,
        None => target.to_string(),
    };
    let real = std::fs::canonicalize(&path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path);
    let lower = real.to_ascii_lowercase();
    LOCATIONS.iter().any(|e| lower.ends_with(e))
}

/// `%XX` escapes decoded; anything malformed stays as written.
fn decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the log keeps of an opened link: the host of a web address, only the kind of anything else.
fn said(target: &str) -> String {
    let lower = target.to_ascii_lowercase();
    match ["http://", "https://"].iter().find(|p| lower.starts_with(**p)) {
        Some(p) => {
            // The authority ends at the first `/`, `?` or `#`, as a browser reads it; a user and password before `@` stay out.
            let authority = target[p.len()..].split(['/', '?', '#']).next().unwrap_or("");
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            let (name, port) = host.rsplit_once(':').filter(|(n, _)| !n.ends_with(']') || n.starts_with('[')).unwrap_or((host, ""));
            let readable = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || ".-[]:".contains(c)) && port.chars().all(|c| c.is_ascii_digit());
            if readable { format!("in the browser: {p}{}", crate::render::unspoofed(host)) } else { format!("in the browser: {p}(an address it could not read)") }
        }
        None if lower.starts_with("mailto:") => "in the mail app".into(),
        None => "in its app: a file".into(),
    }
}

/// A file in the default text editor: `open -t`.
pub fn open_text(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }
    spawn_open(text_command(path))?;
    crate::log::info("file opened in the text editor");
    Ok(())
}

fn text_command(path: &Path) -> std::process::Command {
    open_command(&["-t", &path.display().to_string()])
}

fn open_command(args: &[&str]) -> std::process::Command {
    let mut c = crate::spawn::cmd("open");
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c
}

fn spawn_open(mut c: std::process::Command) -> Result<(), String> {
    crate::spawn::detached(&mut c).map(|_| ()).map_err(|e| format!("failed to open: {e}"))
}

fn resolve_path(path: &str, cwd: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = crate::paths::home()
    {
        return home.join(rest);
    }
    let p = Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_file_opens_in_the_default_text_editor() {
        let c = text_command(Path::new("/x/config.json"));
        assert!(c.get_program().to_string_lossy().ends_with("open"));
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["-t", "/x/config.json"]);
    }

    #[test]
    fn the_log_keeps_a_links_host_or_kind_never_its_path_or_address() {
        assert_eq!(said("https://me:token@example.com:8080/a/b?q=1#f"), "in the browser: https://example.com:8080");
        assert_eq!(said("https://example.com/?email=me@corp.internal/x"), "in the browser: https://example.com", "an @ in the query is not a host");
        assert_eq!(said("https://example.com/u@secret-token-123#f"), "in the browser: https://example.com", "nor one in the path");
        assert_eq!(said("https://u:pa/ss@example.com/x"), "in the browser: https://(an address it could not read)", "a broken address logs none of itself");
        assert_eq!(said("https://[2001:db8::1]:8443/x"), "in the browser: https://[2001:db8::1]:8443");
        assert_eq!(said("HTTP://Example.com"), "in the browser: http://Example.com");
        assert_eq!(said("mailto:someone@example.com"), "in the mail app");
        assert_eq!(said("file:///Users/me/secret.txt"), "in its app: a file");
        assert_eq!(said("/Users/me/notes.md"), "in its app: a file");
    }

    #[test]
    fn a_file_that_opens_a_hidden_web_address_is_not_opened() {
        let dir = std::env::temp_dir().join(format!("nerd-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.webloc", "b.INETLOC", "c.url", "d.fileloc"] {
            std::fs::write(dir.join(name), "").unwrap();
            let link = Link::Path { path: dir.join(name).display().to_string(), line: None };
            assert!(open(&link, &dir).unwrap_err().contains("open it from Finder"), "{name}");
        }
        for url in [format!("file://{}/a.webloc", dir.display()), format!("file://{}/a%2Ewebloc", dir.display()), format!("file://{}/a.webloc?x=1", dir.display())] {
            assert!(open(&Link::Url(url.clone()), &dir).unwrap_err().contains("open it from Finder"), "{url}");
        }
        std::os::unix::fs::symlink(dir.join("a.webloc"), dir.join("plain.txt")).unwrap();
        let link = Link::Path { path: dir.join("plain.txt").display().to_string(), line: None };
        assert!(open(&link, &dir).unwrap_err().contains("open it from Finder"), "a link to one is one");
        std::fs::write(dir.join("e.vncloc"), "").unwrap();
        assert!(holds_an_address(&dir.join("e.vncloc").display().to_string()));
        assert!(!holds_an_address("https://example.com/a.webloc"), "a web address is the browser's to open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn screen_of(cols: u16, rows: u16, bytes: &[u8]) -> crate::term::Term {
        let mut p = crate::term::Term::new(cols, rows, 0);
        p.feed(bytes);
        p
    }

    #[test]
    fn detects_url_and_trims_trailing_punctuation() {
        let p = screen_of(60, 1, b"see (https://example.com/a,b).");
        assert_eq!(
            link_at(p.screen(), 0, 10),
            Some(Link::Url("https://example.com/a,b".to_string()))
        );
    }

    #[test]
    fn detects_mailto_and_file_schemes() {
        let p = screen_of(60, 1, b"mailto:a@b.co file:///etc/hosts");
        assert_eq!(link_at(p.screen(), 0, 2), Some(Link::Url("mailto:a@b.co".to_string())));
        assert_eq!(
            link_at(p.screen(), 0, 20),
            Some(Link::Url("file:///etc/hosts".to_string()))
        );
    }

    #[test]
    fn detects_absolute_and_home_paths() {
        let p = screen_of(60, 1, b"open /etc/hosts or ~/src/mouse.rs now");
        assert_eq!(
            link_at(p.screen(), 0, 6),
            Some(Link::Path { path: "/etc/hosts".to_string(), line: None })
        );
        assert_eq!(
            link_at(p.screen(), 0, 22),
            Some(Link::Path { path: "~/src/mouse.rs".to_string(), line: None })
        );
    }

    #[test]
    fn detects_relative_path_with_extension() {
        let p = screen_of(60, 1, b"see src/links.rs here");
        assert_eq!(
            link_at(p.screen(), 0, 5),
            Some(Link::Path { path: "src/links.rs".to_string(), line: None })
        );
    }

    #[test]
    fn strips_line_and_line_col_suffix() {
        let p = screen_of(60, 1, b"src/mouse.rs:120 and src/select.rs:12:3 end");
        assert_eq!(
            link_at(p.screen(), 0, 2),
            Some(Link::Path { path: "src/mouse.rs".to_string(), line: Some(120) })
        );
        assert_eq!(
            link_at(p.screen(), 0, 25),
            Some(Link::Path { path: "src/select.rs".to_string(), line: Some(12) })
        );
    }

    #[test]
    fn out_of_range_coordinates_give_none() {
        let p = screen_of(10, 1, b"hi");
        assert_eq!(link_at(p.screen(), 5, 0), None);
        assert_eq!(link_at(p.screen(), 0, 50), None);
    }

    #[test]
    fn plain_word_is_not_a_link() {
        let p = screen_of(20, 1, b"hello world");
        assert_eq!(link_at(p.screen(), 0, 2), None);
    }

    #[test]
    fn wide_chars_nearby_do_not_break_the_run() {
        // "あ" is a wide character, occupies two cells; the link sits right after it.
        let p = screen_of(40, 1, b"\xe3\x81\x82 https://x.io/p");
        assert_eq!(link_at(p.screen(), 0, 4), Some(Link::Url("https://x.io/p".to_string())));
        // clicking on the wide character itself is not a link
        assert_eq!(link_at(p.screen(), 0, 0), None);
        assert_eq!(link_at(p.screen(), 0, 1), None);
    }

    #[test]
    fn tilde_path_classification() {
        let p = screen_of(20, 1, b"~/x");
        assert_eq!(
            link_at(p.screen(), 0, 0),
            Some(Link::Path { path: "~/x".to_string(), line: None })
        );
    }
}
