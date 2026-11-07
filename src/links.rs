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
        Link::Path { path, .. } => resolve_path(path, cwd).display().to_string(),
    };
    let home = crate::paths::home().unwrap_or_default();
    let opens = judge(&target, cwd, &home, default_app)?;
    let text = match &opens {
        Opens::Text(p) => text_app(plain_text_app(), p, cwd, &home),
        _ => PathBuf::new(),
    };
    spawn_open(command(&opens, &text))?;
    crate::log::info(&match opens {
        Opens::Text(_) => "file opened in the text editor".to_string(),
        Opens::Reveal(_) => "file shown in Finder".to_string(),
        _ => format!("link opened {}", said(&target)),
    });
    Ok(())
}

/// Files whose app opens an address or a path written inside them, which nerd never showed.
const LOCATIONS: [&str; 8] = [".webloc", ".inetloc", ".url", ".fileloc", ".ftploc", ".afploc", ".vncloc", ".mailloc"];
/// Opened, these start a program, whatever app is theirs.
const RUNS: [&str; 17] = ["command", "tool", "terminal", "term", "app", "workflow", "action", "jar", "its", "itermscript", "itermtab", "iterm2arrangement", "it2triggers", "pyc", "pyo", "playground", "playgroundbook"];
/// Their default apps are terminals and interpreters, so they open as text.
const SCRIPTS: [&str; 10] = ["sh", "bash", "zsh", "fish", "csh", "ksh", "pl", "py", "pyw", "rb"];
/// ceiling: a terminal or script runner not listed, installed under the trusted roots, that becomes a file's default app is handed the file; the way past is treating as a runner any app that claims public.unix-executable or com.apple.terminal.shell-script.
const RUNNERS: [&str; 11] = [
    "com.apple.Terminal", "com.googlecode.iterm2", "com.mitchellh.ghostty", "net.kovidgoyal.kitty", "com.github.wez.wezterm", "com.cmuxterm.app",
    "org.python.PythonLauncher", "com.apple.ScriptEditor2", "com.apple.Automator", "com.apple.AutomatorInstaller", "com.apple.JavaLauncher",
];
const FINDER: &str = "com.apple.finder";
const TEXTEDIT: &str = "/System/Applications/TextEdit.app";

/// An app LaunchServices names, its bundle id and its executable when it has them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct App {
    path: PathBuf,
    id: Option<String>,
    exe: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Opens {
    Target(std::ffi::OsString),
    Text(PathBuf),
    With(PathBuf, PathBuf),
    Reveal(PathBuf),
}

const ROLES_ALL: u32 = 0xFFFF_FFFF;

fn url_path(url: &crate::cf::Owned) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    if url.0.is_null() {
        return None;
    }
    let mut buf = vec![0u8; 4096];
    // SAFETY: url is a live CFURL; the call writes at most buf.len() bytes, NUL included.
    if unsafe { crate::cf::CFURLGetFileSystemRepresentation(url.0, 1, buf.as_mut_ptr(), buf.len() as isize) } == 0 {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(std::ffi::CStr::from_bytes_until_nul(&buf).ok()?.to_bytes())))
}

fn app_at(url: crate::cf::Owned) -> Option<App> {
    let path = url_path(&url)?;
    // SAFETY: url is a live CFURL; every Create/Copy is held in an Owned, the identifier is a Get.
    let bundle = crate::cf::Owned(unsafe { crate::cf::CFBundleCreate(std::ptr::null(), url.0) });
    if bundle.0.is_null() {
        return Some(App { path, id: None, exe: None });
    }
    let id = Some(crate::cf::text(unsafe { crate::cf::CFBundleGetIdentifier(bundle.0) })).filter(|s| !s.is_empty());
    let exe = url_path(&crate::cf::Owned(unsafe { crate::cf::CFBundleCopyExecutableURL(bundle.0) }));
    Some(App { path, id, exe })
}

/// The app `open` would use for this file now; None when LaunchServices names none.
fn default_app(p: &Path) -> Option<App> {
    use std::os::unix::ffi::OsStrExt as _;
    let b = p.as_os_str().as_bytes();
    // SAFETY: b outlives the call; every Create/Copy is held in an Owned.
    let url = crate::cf::Owned(unsafe { crate::cf::CFURLCreateFromFileSystemRepresentation(std::ptr::null(), b.as_ptr(), b.len() as isize, p.is_dir() as u8) });
    if url.0.is_null() {
        return None;
    }
    app_at(crate::cf::Owned(unsafe { crate::cf::LSCopyDefaultApplicationURLForURL(url.0, ROLES_ALL, std::ptr::null_mut()) }))
}

fn plain_text_app() -> Option<App> {
    let uti = crate::cf::Owned::string("public.plain-text");
    // SAFETY: uti is a live CFString; the result is held in an Owned.
    app_at(crate::cf::Owned(unsafe { crate::cf::LSCopyDefaultApplicationURLForContentType(uti.0, ROLES_ALL, std::ptr::null_mut()) }))
}

fn is_finder(a: &App) -> bool {
    a.id.as_deref().is_some_and(|i| i.eq_ignore_ascii_case(FINDER))
}

/// The app's real path when a click may hand it a file: a bundle id, no runner, under the system or Applications folders, no part of it in a folder another account may write, and outside the sealed system neither the clicked path, inside it, nor in the clicked pane's folder.
fn trusted(a: &App, clicked: &Path, cwd: &Path, home: &Path) -> Option<PathBuf> {
    let id = a.id.as_deref()?;
    if RUNNERS.iter().any(|r| r.eq_ignore_ascii_case(id)) {
        return None;
    }
    let (p, cwd, home) = (std::fs::canonicalize(&a.path).ok()?, std::fs::canonicalize(cwd).ok()?, std::fs::canonicalize(home).ok()?);
    // The data volume's other name: /System/Volumes/Data/Users is /Users.
    let sealed = p.starts_with("/System") && !p.starts_with("/System/Volumes/Data");
    let rooted = sealed || p.starts_with("/Applications") || p.starts_with("/Library") || p.starts_with(home.join("Applications"));
    // /Library/Caches is world-writable, some apps group-writable for staff: another account's pane may change what is there.
    let others = |q: &Path| {
        use std::os::unix::fs::MetadataExt as _;
        q.ancestors().any(|a| std::fs::metadata(a).is_ok_and(|m| m.mode() & 0o002 != 0 || (m.mode() & 0o020 != 0 && m.gid() != 0 && m.gid() != 80)))
    };
    let exe = a.exe.as_ref().and_then(|e| std::fs::canonicalize(e).ok());
    let open_to_others = others(&p.join("Contents/MacOS")) || exe.as_deref().is_some_and(others);
    (rooted && !open_to_others && (sealed || !(p.starts_with(clicked) || p.starts_with(&cwd)))).then_some(p)
}

/// The plain-text app under the same trust; TextEdit when it falls short.
fn text_app(found: Option<App>, clicked: &Path, cwd: &Path, home: &Path) -> PathBuf {
    found.filter(|a| !is_finder(a)).and_then(|a| trusted(&a, clicked, cwd, home)).unwrap_or_else(|| TEXTEDIT.into())
}

/// A Finder alias: `kIsAlias` in the flags of its FinderInfo, which `canonicalize` does not follow.
fn alias(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return true };
    let mut info = [0u8; 32];
    // SAFETY: getxattr writes at most info.len() bytes into info.
    let n = unsafe { nix::libc::getxattr(c.as_ptr(), c"com.apple.FinderInfo".as_ptr(), info.as_mut_ptr().cast(), info.len(), 0, 0) };
    n >= 10 && info[8] & 0x80 != 0
}

/// How a link or a path opens, judged on the file it really names (decoded, without its query, through links), with its app chosen now.
/// ceiling: a plain folder whose app is Finder that a pane swaps for a program between this check and `open` is opened by its new type, since Finder pins nothing; the way past is revealing plain folders too.
fn judge(target: &str, cwd: &Path, home: &Path, app: impl Fn(&Path) -> Option<App>) -> Result<Opens, String> {
    let lower = target.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"].iter().any(|p| lower.starts_with(p)) {
        return Ok(Opens::Target(target.into()));
    }
    let path = if lower.starts_with("file://") {
        let rest = &target[7..];
        let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Err("nerd opens file links to this Mac only".into());
        }
        decoded(path.split(['?', '#']).next().unwrap_or(""))
    } else {
        target.to_string()
    };
    let c = std::fs::canonicalize(&path).map_err(|_| format!("path not found: {path}"))?;
    if LOCATIONS.iter().any(|e| c.to_string_lossy().to_ascii_lowercase().ends_with(e)) {
        return Err("this file opens an address nerd does not show; open it from Finder".into());
    }
    let ext = c.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let is = |list: &[&str]| ext.as_deref().is_some_and(|e| list.contains(&e));
    if is(&RUNS) {
        return Err("this file runs a program; open it from Finder".into());
    }
    let m = std::fs::metadata(&c).map_err(|_| format!("path not found: {path}"))?;
    if !m.is_file() && !m.is_dir() {
        return Err("nerd opens files and folders only".into());
    }
    if alias(&c) {
        return Err("this file is a Finder alias; open it from Finder".into());
    }
    let dotted = ext.as_deref().is_some_and(|e| !e.is_empty());
    if m.is_file() {
        if !dotted || is(&SCRIPTS) {
            return Ok(Opens::Text(c));
        }
        // Finder would hand the file to its default app as it is when Finder opens it.
        return Ok(match app(&c).filter(|a| !is_finder(a)).and_then(|a| trusted(&a, &c, cwd, home)) {
            Some(a) => Opens::With(a, c),
            None => Opens::Text(c),
        });
    }
    let found = app(&c);
    let finder = found.as_ref().is_some_and(is_finder);
    match (found.and_then(|a| trusted(&a, &c, cwd, home)), dotted) {
        (Some(_), true) if finder => Ok(Opens::Reveal(c)),
        (Some(_), false) if finder => Ok(Opens::Target(c.into_os_string())),
        (Some(a), _) => Ok(Opens::With(a, c)),
        (None, true) => Ok(Opens::Reveal(c)),
        (None, false) => Err("this folder is a program; open it from Finder".into()),
    }
}

fn command(o: &Opens, text: &Path) -> std::process::Command {
    use std::ffi::OsStr;
    match o {
        Opens::Target(t) => open_command(&[t]),
        Opens::Text(p) => text_command(text, p),
        Opens::With(a, p) => open_command(&[OsStr::new("-a"), a.as_os_str(), p.as_os_str()]),
        Opens::Reveal(p) => open_command(&[OsStr::new("-R"), p.as_os_str()]),
    }
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

/// A file in the plain-text app, chosen now.
pub fn open_text(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }
    let home = crate::paths::home().unwrap_or_default();
    spawn_open(text_command(&text_app(plain_text_app(), path, path.parent().unwrap_or(path), &home), path))?;
    crate::log::info("file opened in the text editor");
    Ok(())
}

fn text_command(app: &Path, path: &Path) -> std::process::Command {
    open_command(&[std::ffi::OsStr::new("-a"), app.as_os_str(), path.as_os_str()])
}

fn open_command<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> std::process::Command {
    let mut c = crate::spawn::cmd("/usr/bin/open");
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
        let c = text_command(Path::new("/A/T.app"), Path::new("/x/config.json"));
        assert!(c.get_program().to_string_lossy().ends_with("open"));
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["-a", "/A/T.app", "/x/config.json"]);
    }

    #[test]
    fn the_default_app_of_a_text_file_is_found() {
        let dir = std::env::temp_dir().join(format!("nerd-links-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.txt"), "x").unwrap();
        let a = default_app(&dir.join("x.txt"));
        let _ = std::fs::remove_dir_all(&dir);
        let a = a.expect("LaunchServices names an app for a .txt");
        assert!(a.id.as_deref().is_some_and(|i| !i.is_empty()) && a.path.extension().is_some_and(|e| e == "app"), "{a:?}");
    }

    #[test]
    fn the_plain_text_app_is_found() {
        let a = plain_text_app().expect("LaunchServices names the plain-text app");
        assert!(a.id.as_deref().is_some_and(|i| !i.is_empty()), "{a:?}");
    }

    #[test]
    fn a_file_that_runs_a_program_is_not_opened() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let base = std::env::temp_dir().join(format!("nerd-judge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("work")).unwrap();
        let d = base.canonicalize().unwrap();
        let (work, home) = (d.join("work"), d.join("home"));
        let apps = home.join("Applications");
        let (te, fi, tm) = (PathBuf::from(TEXTEDIT), PathBuf::from("/System/Library/CoreServices/Finder.app"), PathBuf::from("/System/Applications/Utilities/Terminal.app"));
        let (e_app, h_app) = (d.join("E.app"), apps.join("H.app"));
        let dv_app = PathBuf::from(format!("/System/Volumes/Data{}", e_app.display()));
        let w_app = apps.join("ww/W.app");
        let (g_app, gx_app, gy_app) = (apps.join("gw/G.app"), apps.join("GX.app"), apps.join("GY.app"));
        for dir in [&e_app, &h_app, &w_app, &g_app, &gx_app.join("Contents/MacOS"), &gy_app.join("Contents/MacOS"), &apps.join("pf"), &apps.join("pf.v2"), &apps.join("in.v4/Contents/I.app")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // As /Library/Caches: a trusted root, but anyone may write there.
        std::fs::set_permissions(apps.join("ww"), std::fs::Permissions::from_mode(0o1777)).unwrap();
        // As an app installed group-writable for staff, which another account's panes are in; and one whose executable alone is.
        // SAFETY: getgid has no preconditions.
        let gid = unsafe { nix::libc::getgid() };
        assert!(gid != 0 && gid != 80, "the test needs a group other than wheel and admin");
        std::fs::write(gx_app.join("Contents/MacOS/x"), "").unwrap();
        std::fs::write(gy_app.join("Contents/MacOS/y"), "").unwrap();
        for (p, mode) in [(apps.join("gw"), 0o775), (gx_app.join("Contents/MacOS/x"), 0o775), (gy_app.join("Contents/MacOS/y"), 0o755)] {
            std::os::unix::fs::chown(&p, None, Some(gid)).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let id = |s: &str| Some(s.to_string());
        let fake = |c: &Path| -> Option<App> {
            let name = c.file_name().unwrap().to_string_lossy().into_owned();
            let ext = c.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            let at = |path: &Path, i: Option<String>| Some(App { path: path.to_path_buf(), id: i, exe: None });
            match (name.as_str(), ext.as_str()) {
                ("pf" | "pf.v2", _) => at(c, id("dev.nerdterm.probe")),
                ("in.v4", _) => at(&c.join("Contents/I.app"), id("dev.nerdterm.probe")),
                ("x.bundle" | "tbin", _) | (_, "v2") => at(&tm, id("com.apple.Terminal")),
                (_, "v3") => at(&te, id("NET.KOVIDGOYAL.KITTY")),
                ("x.plugin" | "nbin", _) | (_, "q") => None,
                ("ebin", _) | (_, "e") => at(&e_app, id("dev.nerdterm.editor")),
                ("dvbin", _) | (_, "dv") => at(&dv_app, id("dev.nerdterm.editor")),
                (_, "h") => at(&h_app, id("dev.nerdterm.editor")),
                (_, "n") => at(&te, None),
                ("bin" | "conf.d", _) | (_, "f") => at(&fi, id(FINDER)),
                ("fbin", _) => at(&e_app, id(FINDER)),
                ("wbin", _) | (_, "w") => at(&w_app, id("dev.nerdterm.editor")),
                ("gbin", _) | (_, "g") => at(&g_app, id("dev.nerdterm.editor")),
                (_, "gx") => Some(App { path: gx_app.clone(), id: id("dev.nerdterm.editor"), exe: Some(gx_app.join("Contents/MacOS/x")) }),
                (_, "gy") => Some(App { path: gy_app.clone(), id: id("dev.nerdterm.editor"), exe: Some(gy_app.join("Contents/MacOS/y")) }),
                _ => at(&te, id("dev.nerdterm.editor")),
            }
        };
        let file = |name: &str, mode: u32| {
            let p = work.join(name);
            std::fs::write(&p, "echo hi\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        for n in ["x.command", "X.TOOL", "s.terminal", "s.term", "w.workflow", "a.action", "j.jar", "t.its", "t.itermscript", "t.itermtab", "t.iterm2arrangement", "t.it2triggers", "c.pyc", "c.PYO"] {
            file(n, 0o644);
        }
        for n in ["run.sh", "tool", "run.", "run.v2"] {
            file(n, 0o755);
        }
        for n in ["R.PY", "x.pyw", "x.bash", "x.zsh", "x.fish", "x.csh", "x.ksh", "x.pl", "x.rb", "notes", "k.v3", "odd.q", "x.e", "x.n", "x.f", "x.h", "x.dv", "x.w", "x.g", "x.gx", "x.gy", "readme.md", "doc.md", "doc2.md"] {
            file(n, 0o644);
        }
        for n in ["A.app", "p.playground", "b.PLAYGROUNDBOOK", "P.xcodeproj", "proj", "x.bundle", "x.plugin", "dir.q", "conf.d", "s.e", "n.n", "s.dv", "s.w", "s.g", "bin", "tbin", "nbin", "ebin", "fbin", "dvbin", "wbin", "gbin"] {
            std::fs::create_dir_all(work.join(n)).unwrap();
        }
        symlink(work.join("x.command"), work.join("notes.txt")).unwrap();
        symlink(work.join("run.sh"), work.join("link.txt")).unwrap();
        symlink(work.join("readme.md"), work.join("see.md")).unwrap();
        let _sock = std::os::unix::net::UnixListener::bind(work.join("s.sock")).unwrap();
        let finder_info = |name: &str, byte8: u8| {
            let mut info = [0u8; 32];
            info[8] = byte8;
            let p = std::ffi::CString::new(work.join(name).into_os_string().into_encoded_bytes()).unwrap();
            // SAFETY: both strings are NUL-terminated; info is 32 bytes.
            assert_eq!(unsafe { nix::libc::setxattr(p.as_ptr(), c"com.apple.FinderInfo".as_ptr(), info.as_ptr().cast(), info.len(), 0, 0) }, 0, "{name}");
        };
        finder_info("doc.md", 0x80);
        finder_info("doc2.md", 0);

        let w = |n: &str| work.join(n).display().to_string();
        let runs = Err::<Opens, String>("this file runs a program; open it from Finder".into());
        let program = Err::<Opens, String>("this folder is a program; open it from Finder".into());
        let text = |n: &str| Ok(Opens::Text(work.join(n)));
        let with = |a: &Path, n: &str| Ok(Opens::With(a.to_path_buf(), work.join(n)));
        let reveal = |p: PathBuf| Ok(Opens::Reveal(p));
        let missing = d.join("missing");
        let sys = Path::new("/System/Applications");
        let mut cases: Vec<(String, &Path, Result<Opens, String>)> = Vec::new();
        for n in ["x.command", "X.TOOL", "s.terminal", "s.term", "A.app", "w.workflow", "a.action", "j.jar", "t.its", "t.itermscript", "t.itermtab", "t.iterm2arrangement", "t.it2triggers", "c.pyc", "c.PYO", "p.playground", "b.PLAYGROUNDBOOK", "notes.txt"] {
            cases.push((w(n), &work, runs.clone()));
        }
        cases.push((format!("file://{}/x%2Ecommand", work.display()), &work, runs.clone()));
        cases.push((format!("file://localhost{}/A.app/", work.display()), &work, runs.clone()));
        cases.push((w("doc.md"), &work, Err("this file is a Finder alias; open it from Finder".into())));
        cases.push((w("doc2.md"), &work, with(&te, "doc2.md")));
        cases.push((w("s.sock"), &work, Err("nerd opens files and folders only".into())));
        for n in ["run.sh", "R.PY", "x.pyw", "x.bash", "x.zsh", "x.fish", "x.csh", "x.ksh", "x.pl", "x.rb", "tool", "notes", "run.", "run.v2", "k.v3", "odd.q", "x.e", "x.n", "x.f", "x.dv", "x.w", "x.g", "x.gx"] {
            cases.push((w(n), &work, text(n)));
        }
        cases.push((format!("file://localhost{}/tool", work.display()), &work, text("tool")));
        cases.push((w("link.txt"), &work, text("run.sh")));
        cases.push((w("x.h"), &home, text("x.h")));
        cases.push((w("readme.md"), &missing, text("readme.md")));
        cases.push((w("readme.md"), &work, with(&te, "readme.md")));
        cases.push((w("readme.md"), sys, with(&te, "readme.md")));
        cases.push((w("see.md"), &work, with(&te, "readme.md")));
        cases.push((w("P.xcodeproj"), &work, with(&te, "P.xcodeproj")));
        cases.push((w("proj"), &work, with(&te, "proj")));
        cases.push((w("x.h"), &work, with(&h_app, "x.h")));
        cases.push((w("x.gy"), &work, with(&gy_app, "x.gy")));
        for n in ["x.bundle", "x.plugin", "dir.q", "conf.d", "s.e", "n.n", "s.dv", "s.w", "s.g"] {
            cases.push((w(n), &work, reveal(work.join(n))));
        }
        cases.push((apps.join("pf.v2").display().to_string(), &work, reveal(apps.join("pf.v2"))));
        cases.push((apps.join("in.v4").display().to_string(), &work, reveal(apps.join("in.v4"))));
        cases.push((w("bin"), &work, Ok(Opens::Target(work.join("bin").into()))));
        cases.push(("https://example.com/a.command".into(), &work, Ok(Opens::Target("https://example.com/a.command".into()))));
        cases.push((apps.join("pf").display().to_string(), &work, program.clone()));
        for n in ["tbin", "nbin", "ebin", "fbin", "dvbin", "wbin", "gbin"] {
            cases.push((w(n), &work, program.clone()));
        }
        cases.push(("file://server/x".into(), &work, Err("nerd opens file links to this Mac only".into())));
        cases.push((format!("file://{}/missing", work.display()), &work, Err(format!("path not found: {}/missing", work.display()))));
        let bad: Vec<String> = cases.iter().filter_map(|(t, cwd, want)| {
            let got = judge(t, cwd, &home, fake);
            (got != *want).then(|| format!("{t} (cwd {}): got {got:?}, want {want:?}", cwd.display()))
        }).collect();
        assert!(bad.is_empty(), "{}", bad.join("\n"));

        let app = |p: &Path, i: Option<String>| Some(App { path: p.to_path_buf(), id: i, exe: None });
        let clicked = work.join("readme.md");
        assert_eq!(text_app(app(&fi, id("dev.nerdterm.editor")), &clicked, &work, &home), fi, "a trusted app that is not the fallback");
        for (found, why) in [(app(&fi, id("com.googlecode.iterm2")), "a runner"), (app(&fi, None), "no id"), (app(&e_app, id("dev.nerdterm.editor")), "outside the roots"), (app(&fi, id(FINDER)), "Finder"), (None, "none")] {
            assert_eq!(text_app(found, &clicked, &work, &home), PathBuf::from(TEXTEDIT), "{why}");
        }
        let args = |o: Opens| command(&o, Path::new("/A/T.app")).get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args(Opens::Text("/p".into())), ["-a", "/A/T.app", "/p"]);
        assert_eq!(args(Opens::With("/X.app".into(), "/p".into())), ["-a", "/X.app", "/p"]);
        assert_eq!(args(Opens::Reveal("/p".into())), ["-R", "/p"]);
        assert_eq!(args(Opens::Target("https://x".into())), ["https://x"]);
        let _ = std::fs::remove_dir_all(&d);
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
        assert!(judge(&dir.join("e.vncloc").display().to_string(), &dir, &dir, |_| None).unwrap_err().contains("open it from Finder"));
        assert_eq!(judge("https://example.com/a.webloc", &dir, &dir, |_| None), Ok(Opens::Target("https://example.com/a.webloc".into())), "a web address is the browser's to open");
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
