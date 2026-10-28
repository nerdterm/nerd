//! Facts about a git repository and worktree management, via the git binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What kind of folder this is from git's point of view; determines the menu contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoKind {
    None,
    Main,
    /// A git worktree.
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
    pub is_current: bool,
}

/// An error is always git's own trimmed stderr, shown to the user as-is. stdin is
/// `null`, or an interactive git could hang waiting on a prompt.
fn run_git<I, S>(dir: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = crate::spawn::cmd("git");
    cmd.arg("-C").arg(dir).args(args);
    run(cmd)
}

/// A folder the user only opened: its own config may not run anything (fsmonitor, lazy fetch, and an
/// index write, which runs post-index-change: diff ignores GIT_OPTIONAL_LOCKS, so autoRefreshIndex is off too).
// Known ceiling: a denylist over git's mechanisms; gits without GIT_NO_LAZY_FETCH still lazy-fetch, and stat-dirty files are re-hashed each call.
fn read_git<I, S>(dir: &Path, args: I, env: &[(String, String)]) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = crate::spawn::cmd("git");
    cmd.envs(env.iter().map(|(k, v)| (k, v))).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_NO_LAZY_FETCH", "1").env("GIT_LITERAL_PATHSPECS", "1");
    cmd.arg("-C").arg(dir).args(["-c", "core.fsmonitor=false", "-c", "diff.autoRefreshIndex=false"]).args(args);
    run(cmd)
}

/// The repo's own filter drivers and git-lfs extensions (local and worktree scope), blanked through env pairs,
/// since a name may hold `=`, which `-c` would split; the user's global ones, such as LFS, still run.
/// An unreadable config is an error: the caller must not run git at all.
fn filters_off(root: &Path) -> Result<Vec<(String, String)>, String> {
    let pattern = r"^(filter\..*\.(clean|smudge|process)|lfs\.extension\..*\.(clean|smudge))$";
    let raw = read_git(root, ["config", "--show-scope", "--name-only", "-z", "--get-regexp", pattern], &[]).or_else(|e| {
        // No matching key: exit 1 with nothing said.
        if !e.is_empty() {
            return Err(e);
        }
        Ok(String::new())
    })?;
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut blank = Vec::new();
    for f in fields.chunks(2).filter(|f| f.len() == 2 && matches!(f[0], "local" | "worktree")) {
        let Some((section, _)) = f[1].rsplit_once('.') else { continue };
        let keys: &[(&str, &str)] =
            if section.starts_with("filter.") { &[("clean", ""), ("smudge", ""), ("process", ""), ("required", "false")] } else { &[("clean", ""), ("smudge", "")] };
        blank.extend(keys.iter().map(|(key, value)| (format!("{section}.{key}"), *value)));
    }
    blank.sort_unstable();
    blank.dedup();
    let mut env = Vec::new();
    for (i, (key, value)) in blank.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), key.clone()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), value.to_string()));
    }
    if !blank.is_empty() {
        env.push(("GIT_CONFIG_COUNT".to_string(), blank.len().to_string()));
    }
    Ok(env)
}

/// Logs the first failure only: callers retry on every refresh.
fn filters_unknown(e: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| crate::log::warn(&format!("git status and diff skipped: the repository's filters could not be read: {e}")));
}

fn run(cmd: Command) -> Result<String, String> {
    run_within(cmd, std::time::Duration::from_secs(10))
}

/// Trimmed stdout of a program that must answer within `limit`, or is killed.
pub fn run_within(mut cmd: Command, limit: std::time::Duration) -> Result<String, String> {
    // Called from the daemon's loop: without a timeout, a stuck lock or network drive
    // would stop all panes. Thread + channel, same trick as ps.
    let program = cmd.get_program().to_string_lossy().into_owned();
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = match rx.recv_timeout(limit) {
        Ok(output) => output.map_err(|e| e.to_string())?,
        Err(_) => {
            // A git left behind would pile up, one per refresh (a FIFO as core.excludesFile blocks forever).
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            return Err(format!("{program} did not respond within {}s", limit.as_secs()));
        }
    };
    if output.status.success() {
        // trim_end, not trim: a `git status -z` entry can legitimately start with a
        // space (code X == ' '), and a full trim() would shift the whole line's parsing.
        Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Walks up to the disk root looking for `.git`: a directory means the main checkout,
/// a `gitdir:` file means a linked one — same test git itself uses, without the binary.
pub fn repo_kind(dir: &Path) -> RepoKind {
    let mut current = dir;
    loop {
        let candidate = current.join(".git");
        if candidate.is_dir() {
            return RepoKind::Main;
        }
        if candidate.is_file() {
            return RepoKind::Linked;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return RepoKind::None,
        }
    }
}

/// The folder holding `.git` (a directory, or a linked worktree's file) at or above `dir`, found on disk without git.
pub fn work_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    read_git(dir, ["rev-parse", "--show-toplevel"], &[]).ok().map(PathBuf::from)
}

/// Checkouts of the same repo share this path, which is how they recognize each other.
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    let raw = read_git(dir, ["rev-parse", "--git-common-dir"], &[]).ok()?;
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() { path } else { dir.join(path) };
    path.canonicalize().ok()
}

/// Sidebar grouping key; kept separate from `common_dir` so its meaning can change independently.
pub fn group_key(dir: &Path) -> Option<PathBuf> {
    common_dir(dir)
}

/// Cheaper than `common_dir`: checks the `.git` marker on disk, no git binary launched.
pub fn is_main_checkout(dir: &Path) -> bool {
    repo_kind(dir) == RepoKind::Main
}

/// Rejects a leading `-` (could pass as a git flag) or path separators (could escape the sibling folder).
fn validate_branch_name(branch: &str) -> Result<(), String> {
    if branch.is_empty() {
        return Err("branch name cannot be empty".to_string());
    }
    if branch.starts_with('-') {
        return Err("branch name cannot start with '-'".to_string());
    }
    if branch.contains(std::path::is_separator) {
        return Err("branch name cannot contain path separators".to_string());
    }
    Ok(())
}

/// Path for the new checkout next to the repository: `<root's parent>/<root name>-<branch>`.
fn sibling_path(dir: &Path, branch: &str) -> Result<PathBuf, String> {
    let root = repo_root(dir).ok_or_else(|| "folder is not inside a git repository".to_string())?;
    let parent = root.parent().ok_or_else(|| "repository root has no parent folder".to_string())?;
    let name = root.file_name().ok_or_else(|| "repository root has no name".to_string())?;
    // A branch namespace like feature/foo doesn't work as a folder name.
    let suffix = branch.replace('/', "-");
    Ok(parent.join(format!("{}-{}", name.to_string_lossy(), suffix)))
}

/// Branch existence isn't checked here; git refuses with its own message, which is
/// exactly what the user should see.
pub fn add_worktree(dir: &Path, branch: &str) -> Result<PathBuf, String> {
    validate_branch_name(branch)?;
    let dest = sibling_path(dir, branch)?;
    let dest_str = dest.to_string_lossy().into_owned();
    // `--` keeps a name from an input field from ever being read as a flag.
    run_git(dir, ["worktree", "add", "-b", branch, "--", dest_str.as_str()]).map(|_| dest)
}

/// Parses `git worktree list --porcelain`: blank-line-separated entries with
/// `worktree`/`HEAD`/`branch refs/heads/<name>` keys. Git guarantees the first entry is main.
pub fn worktrees(dir: &Path) -> Vec<Worktree> {
    let Ok(raw) = read_git(dir, ["worktree", "list", "--porcelain"], &[]) else {
        return Vec::new();
    };
    let current = dir.canonicalize().ok();

    let mut result = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut is_first = true;

    // Synthetic trailing empty line closes the last entry: run_git's trim() may have
    // eaten the real separator after it.
    for line in raw.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(p) = path.take() {
                let is_current = p.canonicalize().ok() == current;
                result.push(Worktree { is_main: is_first, is_current, branch: branch.take(), path: p });
                is_first = false;
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        }
    }
    result
}

/// Removes a checkout from disk. Doesn't touch the branch.
pub fn remove_worktree(dir: &Path, path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().into_owned();
    run_git(dir, ["worktree", "remove", "--", path_str.as_str()]).map(|_| ())
}

/// `--porcelain=v1 -z`: unescaped paths, NUL-separated entries. On rename/copy an
/// entry carries two NUL-separated paths, new then old; we mark the new one.
/// The diff of one file, as git prints it: the working tree first, then the index, since a
/// staged change has nothing left in the working tree. Empty when there is nothing to show
/// (an untracked file has no diff, and the preview falls back to the file itself).
pub fn diff(dir: &Path, path: &Path) -> String {
    let Some(root) = repo_root(dir) else {
        return String::new();
    };
    let p = path.to_string_lossy();
    let env = match filters_off(&root) {
        Ok(env) => env,
        Err(e) => {
            filters_unknown(&e);
            return String::new();
        }
    };
    let worktree = read_git(&root, ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--ignore-submodules=dirty", "--submodule=short", "--", &p], &env).unwrap_or_default();
    if !worktree.is_empty() {
        return worktree;
    }
    read_git(&root, ["diff", "--no-color", "--cached", "--no-ext-diff", "--no-textconv", "--ignore-submodules=dirty", "--submodule=short", "--", &p], &env).unwrap_or_default()
}

pub fn status_marks(dir: &Path) -> HashMap<PathBuf, char> {
    let mut marks = HashMap::new();
    let Some(root) = repo_root(dir) else {
        return marks;
    };
    // `all`, not `normal`: `normal` answers with the top untracked directory, and the changes
    // view would name a folder where the files that changed are what is wanted.
    let env = match filters_off(&root) {
        Ok(env) => env,
        Err(e) => {
            filters_unknown(&e);
            return marks;
        }
    };
    let Ok(raw) = read_git(&root, ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=dirty"], &env) else {
        return marks;
    };

    // run_git's trim doesn't touch the trailing NUL, so split('\0') leaves an empty
    // trailing token; drop it.
    let mut fields = raw.split('\0').filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let x = entry.as_bytes()[0] as char;
        let y = entry.as_bytes()[1] as char;
        let path = &entry[3..];

        // Old path on rename/copy is the next field; must be consumed or parsing drifts.
        if x == 'R' || x == 'C' {
            fields.next();
        }

        // `N`, not git's `??`: the column is read as a state, and a question mark asks one.
        let mark = if x == '?' {
            'N'
        } else if x == 'D' || y == 'D' {
            'D'
        } else if x == 'R' || x == 'C' || x == 'M' || y == 'M' {
            'M'
        } else if x == 'A' {
            'A'
        } else {
            // Merge conflict (U) and other rare codes: something definitely changed.
            'M'
        };

        // A submodule or an ignored-but-listed entry can still arrive as "dir/".
        let path = path.trim_end_matches('/');
        marks.insert(root.join(path), mark);
    }
    marks
}

/// What a repository with no commits is compared with, when git cannot say it in the repository's own hash.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// An untracked file is read this far for its lines; past it, and a binary one, counts no lines.
const NEW_FILE_READ: u64 = 256 * 1024;
/// New files a review lists.
const NEW_FILES: usize = 500;

/// A file a review shows, its path from the repository's root.
#[derive(Clone, Debug, PartialEq)]
pub struct Changed {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// What a review compares with: the default branch's name and where this branch left it, when that is not
/// HEAD itself; and HEAD, or the empty tree in a repository with no commits.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewBase {
    pub branch: Option<String>,
    pub since: Option<String>,
    pub head: String,
}

/// The default branch (`origin/HEAD`, else `main`, else `master`) and its merge-base with HEAD.
pub fn review_base(root: &Path) -> ReviewBase {
    let head = read_git(root, ["rev-parse", "--verify", "--quiet", "HEAD"], &[])
        .or_else(|_| read_git(root, ["hash-object", "-t", "tree", "/dev/null"], &[]))
        .unwrap_or_else(|_| EMPTY_TREE.to_string());
    // A full ref name: the agent writes origin/HEAD, and a short one could start with "-".
    let full = read_git(root, ["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"], &[])
        .ok()
        .filter(|r| r.starts_with("refs/remotes/origin/"))
        .or_else(|| ["refs/heads/main", "refs/heads/master"].into_iter().find(|r| read_git(root, ["rev-parse", "--verify", "--quiet", r], &[]).is_ok()).map(String::from));
    let since = full.as_deref().and_then(|r| read_git(root, ["merge-base", "HEAD", r], &[]).ok()).filter(|s| *s != head);
    let branch = full.map(|r| r.trim_start_matches("refs/remotes/origin/").trim_start_matches("refs/heads/").to_string());
    ReviewBase { branch, since, head }
}

/// Tracked files changed since `base`, committed or not, then new files; renames are a deletion and an addition.
pub fn review_files(root: &Path, base: &str) -> Vec<Changed> {
    let Ok(env) = filters_off(root) else { return Vec::new() };
    let tracked = read_git(root, ["diff", "--numstat", "-z", "--no-renames", "--no-ext-diff", "--no-textconv", "--ignore-submodules=dirty", base, "--"], &env).unwrap_or_default();
    let mut out: Vec<Changed> = tracked
        .split('\0')
        .filter_map(|f| {
            let mut it = f.splitn(3, '\t');
            let (a, r, path) = (it.next()?, it.next()?, it.next()?);
            Some(Changed { path: path.to_string(), added: a.parse().unwrap_or(0), removed: r.parse().unwrap_or(0) })
        })
        .collect();
    let new = read_git(root, ["ls-files", "--others", "--exclude-standard", "-z"], &env).unwrap_or_default();
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for path in new.split('\0').filter(|p| !p.is_empty() && !p.ends_with('/')).take(NEW_FILES) {
        let added = new_file(&canon, path).map_or(0, |t| t.lines().count() as u32);
        out.push(Changed { path: path.to_string(), added, removed: 0 });
    }
    out
}

/// A regular file's text under the canonical `root`, read up to `NEW_FILE_READ`; None for a FIFO, a binary or an
/// unreadable one, and when any part of its path is a link.
fn new_file(root: &Path, path: &str) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    // Checked on the open file: a FIFO swapped in after a look by path would block the git worker for good, and a
    // folder swapped for a link would show the agent what its isolation hides from it.
    let f = std::fs::OpenOptions::new().read(true).custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW_ANY).open(root.join(path)).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut b = Vec::new();
    f.take(NEW_FILE_READ).read_to_end(&mut b).ok()?;
    (!b.contains(&0)).then(|| String::from_utf8_lossy(&b).into_owned())
}

/// The diff of `path` since `base`; a new file as all added lines, a binary one as a note.
pub fn review_diff(root: &Path, base: &str, path: &str) -> String {
    let Ok(env) = filters_off(root) else { return String::new() };
    let tracked = read_git(root, ["diff", "--no-color", "--no-renames", "--no-ext-diff", "--no-textconv", "--ignore-submodules=dirty", "--submodule=short", "-U3", base, "--", path], &env).unwrap_or_default();
    if !tracked.is_empty() {
        return tracked;
    }
    match root.canonicalize().ok().and_then(|canon| new_file(&canon, path)) {
        Some(text) => {
            let lines: Vec<&str> = text.lines().collect();
            format!("@@ -0,0 +1,{} @@ new file\n{}", lines.len(), lines.iter().map(|l| format!("+{l}")).collect::<Vec<_>>().join("\n"))
        }
        None => "Binary or unreadable file".to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Skip the module instead of failing the build when git isn't on PATH.
    fn git_available() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn run(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("git should be on PATH");
        assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn a_review_lists_what_changed_since_the_default_branch_and_new_files() {
        if !git_available() {
            return;
        }
        let dir = init_repo("review");
        let on_main = review_base(&dir);
        assert_eq!((on_main.branch.as_deref(), on_main.since.as_deref()), (Some("main"), None), "on the default branch itself: nothing since it");
        run(&dir, &["switch", "-q", "-c", "work"]);
        std::fs::write(dir.join("a.rs"), "one\ntwo\n").unwrap();
        run(&dir, &["add", "a.rs"]);
        run(&dir, &["commit", "-q", "-m", "a"]);
        std::fs::write(dir.join("README.md"), "test\nmore\n").unwrap();
        std::fs::write(dir.join("new file.txt"), "x\ny\nz\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2]).unwrap();
        let b = review_base(&dir);
        let since = b.since.clone().expect("work left main");
        let files = review_files(&dir, &since);
        let find = |p: &str| files.iter().find(|c| c.path == p).cloned();
        assert_eq!(find("a.rs"), Some(Changed { path: "a.rs".into(), added: 2, removed: 0 }), "committed since main");
        assert_eq!(find("README.md").map(|c| (c.added, c.removed)), Some((1, 0)), "not committed yet");
        assert_eq!(find("new file.txt").map(|c| c.added), Some(3), "a new file, its spaces kept");
        assert_eq!(find("blob.bin").map(|c| c.added), Some(0), "a binary file counts no lines");
        let uncommitted = review_files(&dir, &b.head);
        assert!(uncommitted.iter().all(|c| c.path != "a.rs"), "uncommitted only: {uncommitted:?}");
        assert!(review_diff(&dir, &since, "a.rs").contains("+one"));
        assert_eq!(review_diff(&dir, &b.head, "new file.txt"), "@@ -0,0 +1,3 @@ new file\n+x\n+y\n+z");
        assert_eq!(review_diff(&dir, &b.head, "blob.bin"), "Binary or unreadable file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_review_of_a_repository_with_no_commits_shows_every_file_as_new() {
        if !git_available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("nerd-git-test-review-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("first.rs"), "fn main() {}\n").unwrap();
        let b = review_base(&dir);
        assert_eq!((b.branch, b.since, b.head.as_str()), (None, None, EMPTY_TREE));
        assert_eq!(review_files(&dir, &b.head), vec![Changed { path: "first.rs".into(), added: 1, removed: 0 }]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_review_runs_no_program_the_repository_names_and_reads_no_fifo_or_link() {
        if !git_available() {
            return;
        }
        let (dir, _, hit) = hit_repo("review-hostile");
        run(&dir, &["add", "README.md"]);
        run(&dir, &["commit", "-q", "-m", "readme"]);
        let touch = format!("sh -c 'touch \"{}\"; cat'", hit.display());
        std::fs::write(dir.join(".git/info/attributes"), "* diff=y filter=y\n").unwrap();
        for (key, value) in [("diff.y.textconv", &touch), ("diff.y.command", &touch), ("diff.external", &touch), ("filter.y.clean", &touch), ("filter.y.process", &touch), ("core.fsmonitor", &touch)] {
            run(&dir, &["config", key, value]);
        }
        std::fs::write(dir.join("README.md"), "changed again\n").unwrap();
        age(&dir.join("README.md"));
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        let fifo = dir.join("pipe");
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        std::os::unix::fs::symlink("/etc/hosts", dir.join("link")).unwrap();
        let b = review_base(&dir);
        assert!(review_files(&dir, &b.head).iter().any(|c| c.path == "README.md"));
        assert!(review_diff(&dir, &b.head, "README.md").contains("+changed again"));
        assert!(review_diff(&dir, &b.head, "new.txt").contains("+x"));
        assert!(!ran(&hit), "a program the repository named ran");
        assert_eq!(review_diff(&dir, &b.head, "pipe"), "Binary or unreadable file", "a FIFO is never opened for reading");
        assert_eq!(review_diff(&dir, &b.head, "link"), "Binary or unreadable file", "a link is not followed");
        // listed as new in a folder, then the folder swapped for a link out of the repository
        let outside = init_repo("review-hostile-outside");
        std::fs::write(outside.join("secret"), "key\n").unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/secret"), "decoy\n").unwrap();
        assert!(review_diff(&dir, &b.head, "d/secret").contains("+decoy"), "the temp folder's own links are no reason to refuse");
        std::fs::remove_dir_all(dir.join("d")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("d")).unwrap();
        assert_eq!(review_diff(&dir, &b.head, "d/secret"), "Binary or unreadable file", "no folder of its path is followed out");
        std::fs::remove_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_review_base_never_hands_git_an_option_from_the_repository() {
        if !git_available() {
            return;
        }
        let dir = init_repo("review-option");
        run(&dir, &["switch", "-q", "-c", "work"]);
        std::fs::create_dir_all(dir.join(".git/refs/remotes/origin")).unwrap();
        std::fs::write(dir.join(".git/refs/remotes/origin/HEAD"), "ref: refs/heads/--all\n").unwrap();
        let b = review_base(&dir);
        assert_eq!(b.branch.as_deref(), Some("main"), "a symref outside origin is not the default branch");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Sets user.email/user.name locally so the commit succeeds on any CI machine.
    pub(crate) fn init_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nerd-git-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        run(&dir, &["config", "user.email", "test@example.com"]);
        run(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "test\n").unwrap();
        run(&dir, &["add", "README.md"]);
        run(&dir, &["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn repo_facts_and_worktree_lifecycle() {
        if !git_available() {
            return;
        }
        let dir = init_repo("main");

        let outside = std::env::temp_dir().join(format!("nerd-git-test-none-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(repo_kind(&outside), RepoKind::None);
        std::fs::remove_dir_all(&outside).unwrap();

        assert_eq!(repo_kind(&dir), RepoKind::Main);

        let list = worktrees(&dir);
        assert_eq!(list.len(), 1);
        assert!(list[0].is_main);
        assert!(list[0].is_current);

        let added = add_worktree(&dir, "feature-x").expect("creating a checkout");
        assert!(added.is_dir());
        assert_eq!(repo_kind(&added), RepoKind::Linked);

        assert_eq!(group_key(&added), group_key(&dir), "the checkout and the main one are the same repository");

        let list = worktrees(&dir);
        assert_eq!(list.len(), 2);
        let linked = list.iter().find(|w| !w.is_main).expect("a linked checkout should appear");
        assert!(!linked.is_main);
        assert_eq!(linked.branch.as_deref(), Some("feature-x"));

        remove_worktree(&dir, &added).expect("removing a checkout");
        assert_eq!(worktrees(&dir).len(), 1);
        assert!(!added.exists());

        // The branch itself survived remove_worktree, so git refuses to reuse it here.
        let err = add_worktree(&dir, "feature-x").unwrap_err();
        assert!(!err.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn group_key_joins_main_checkout_with_its_worktree() {
        if !git_available() {
            return;
        }
        let dir = init_repo("group");

        let outside = std::env::temp_dir().join(format!("nerd-git-test-nogroup-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(group_key(&outside), None);
        assert!(!is_main_checkout(&outside));
        std::fs::remove_dir_all(&outside).unwrap();

        let added = add_worktree(&dir, "grouped").expect("creating a checkout");
        let main_key = group_key(&dir).expect("main checkout's key");
        assert_eq!(group_key(&added), Some(main_key));
        assert!(is_main_checkout(&dir));
        assert!(!is_main_checkout(&added));

        remove_worktree(&dir, &added).expect("removing a checkout");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_marks_reports_each_kind() {
        if !git_available() {
            return;
        }
        let dir = init_repo("status");
        // macOS /tmp is itself a symlink, and rev-parse --show-toplevel resolves it.
        let root = dir.canonicalize().unwrap();

        let outside = std::env::temp_dir().join(format!("nerd-git-test-status-none-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert!(status_marks(&outside).is_empty());
        std::fs::remove_dir_all(&outside).unwrap();

        // Committed separately so it doesn't get swept into the staged.txt commit below.
        std::fs::write(dir.join("todelete.txt"), "bye\n").unwrap();
        run(&dir, &["add", "todelete.txt"]);
        run(&dir, &["commit", "-q", "-m", "add todelete"]);

        std::fs::write(dir.join("README.md"), "changed\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
        std::fs::write(dir.join("sp file.txt"), "space\n").unwrap();
        std::fs::create_dir_all(dir.join("newdir")).unwrap();
        std::fs::write(dir.join("newdir/f.txt"), "x\n").unwrap();
        std::fs::write(dir.join("staged.txt"), "new\n").unwrap();
        run(&dir, &["add", "staged.txt"]);
        std::fs::remove_file(dir.join("todelete.txt")).unwrap();

        let marks = status_marks(&dir);

        assert_eq!(marks.get(&root.join("README.md")), Some(&'M'), "a modified tracked file");
        assert_eq!(marks.get(&root.join("untracked.txt")), Some(&'N'), "a new untracked file");
        assert_eq!(marks.get(&root.join("sp file.txt")), Some(&'N'), "a name with a space");
        assert_eq!(marks.get(&root.join("newdir/f.txt")), Some(&'N'), "a file inside an untracked directory, not the directory");
        assert_eq!(marks.get(&root.join("newdir")), None);
        assert_eq!(marks.get(&root.join("staged.txt")), Some(&'A'), "a file added to the index");
        assert_eq!(marks.get(&root.join("todelete.txt")), Some(&'D'), "a deleted file");

        // What the preview shows for a row: the working tree while it still holds the change,
        // the index once the change has been staged out of it.
        assert!(diff(&dir, &root.join("README.md")).contains("+changed"), "a change in the working tree");
        run(&dir, &["add", "--", "todelete.txt"]);
        let staged = diff(&dir, &root.join("todelete.txt"));
        assert!(staged.contains("-bye"), "a staged deletion, which the working tree no longer knows: {staged:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A repo with a changed README.md and a file whose appearance says a command ran.
    fn hit_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = init_repo(tag);
        let hit = dir.join(".git").join("hit");
        std::fs::write(dir.join("README.md"), "changed\n").unwrap();
        let root = dir.canonicalize().unwrap();
        (dir, root, hit)
    }

    /// Whether the hit file appeared since the last look.
    fn ran(hit: &Path) -> bool {
        std::fs::remove_file(hit).is_ok()
    }

    /// Makes a file stat-dirty, so git would refresh the index on its next read; a new time each call.
    fn age(file: &Path) {
        static DAY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let at = format!("202001{:02}0000", DAY.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 28 + 1);
        let ok = Command::new("touch").args(["-t", &at]).arg(file).status().unwrap().success();
        assert!(ok);
    }

    #[test]
    fn a_folders_fsmonitor_does_not_run() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("fsmonitor");
        let mut fired = Vec::new();
        run(&dir, &["config", "core.fsmonitor", &format!("touch '{}'; false", hit.display())]);
        assert!(status_marks(&dir).contains_key(&root.join("README.md")));
        if ran(&hit) {
            fired.push("status");
        }
        assert!(diff(&dir, &root.join("README.md")).contains("+changed"));
        if ran(&hit) {
            fired.push("diff");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_post_index_change_hook_does_not_run() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("pic");
        let mut fired = Vec::new();
        let hook = dir.join(".git/hooks/post-index-change");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", hit.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        run(&dir, &["checkout", "-q", "--", "README.md"]);
        run(&dir, &["config", "hook.x.command", &format!("touch '{}.cfg'", hit.display())]);
        run(&dir, &["config", "hook.x.event", "post-index-change"]);
        let _ = std::fs::remove_file(&hit);
        age(&dir.join("README.md"));
        status_marks(&dir);
        if ran(&hit) {
            fired.push("a hook file");
        }
        if ran(&hit.with_extension("cfg")) {
            fired.push("a hook in config");
        }
        age(&dir.join("README.md"));
        diff(&dir, &root.join("README.md"));
        if ran(&hit) {
            fired.push("diff, a hook file");
        }
        if ran(&hit.with_extension("cfg")) {
            fired.push("diff, a hook in config");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_partial_clone_does_not_fetch_a_missing_blob() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("lazy");
        let mut fired = Vec::new();
        let blob = Command::new("git").arg("-C").arg(&dir).args(["rev-parse", ":README.md"]).output().unwrap();
        let blob = String::from_utf8_lossy(&blob.stdout).trim().to_string();
        run(&dir, &["config", "core.repositoryformatversion", "1"]);
        run(&dir, &["config", "extensions.partialClone", "origin"]);
        run(&dir, &["config", "remote.origin.url", "ssh://h/x"]);
        run(&dir, &["config", "remote.origin.promisor", "true"]);
        run(&dir, &["config", "core.sshCommand", &format!("touch '{}' #", hit.display())]);
        std::fs::remove_file(dir.join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
        diff(&dir, &root.join("README.md"));
        if ran(&hit) {
            fired.push("diff");
        }
        status_marks(&dir);
        if ran(&hit) {
            fired.push("status");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_submodules_clean_filter_does_not_run() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("submodule");
        let mut fired = Vec::new();
        let sub = init_repo("submodule-src");
        run(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sm"]);
        run(&dir, &["commit", "-q", "-m", "sm"]);
        std::fs::write(dir.join(".git/modules/sm/info/attributes"), "* filter=zz\n").unwrap();
        run(&dir.join("sm"), &["config", "filter.zz.clean", &format!("touch '{}'; cat", hit.display())]);
        age(&dir.join("sm/README.md"));
        status_marks(&dir);
        if ran(&hit) {
            fired.push("status");
        }
        age(&dir.join("sm/README.md"));
        diff(&dir, &root.join("sm"));
        if ran(&hit) {
            fired.push("diff");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&sub).unwrap();
    }

    #[test]
    fn a_folders_diff_programs_do_not_run() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("extdiff");
        let mut fired = Vec::new();
        let touch = format!("sh -c 'touch \"{}\"; cat \"$0\"'", hit.display());
        let readme = root.join("README.md");
        run(&dir, &["config", "diff.external", &touch]);
        diff(&dir, &readme);
        if ran(&hit) {
            fired.push("diff.external");
        }
        run(&dir, &["config", "--unset", "diff.external"]);
        std::fs::write(dir.join(".git/info/attributes"), "* diff=y\n").unwrap();
        run(&dir, &["config", "diff.y.command", &touch]);
        diff(&dir, &readme);
        if ran(&hit) {
            fired.push("diff.<driver>.command");
        }
        run(&dir, &["config", "--unset", "diff.y.command"]);
        run(&dir, &["config", "diff.y.textconv", &touch]);
        assert!(diff(&dir, &readme).contains("+changed"), "the plain diff still shows");
        if ran(&hit) {
            fired.push("textconv");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folders_filter_drivers_do_not_run() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("filters");
        let mut fired = Vec::new();
        let touch = format!("sh -c 'touch \"{}\"; cat'", hit.display());
        // `x=y` would be split by `-c`.
        for name in ["x", "x=y"] {
            run(&dir, &["config", &format!("filter.{name}.clean"), &touch]);
            std::fs::write(dir.join(".git/info/attributes"), format!("* filter={name}\n")).unwrap();
            // Same size as the index, so status must hash it through the filter.
            std::fs::write(dir.join("README.md"), "test\n").unwrap();
            age(&dir.join("README.md"));
            status_marks(&dir);
            if ran(&hit) {
                fired.push(format!("status, {name}"));
            }
            std::fs::write(dir.join("README.md"), "changed\n").unwrap();
            assert!(diff(&dir, &root.join("README.md")).contains("+changed"));
            if ran(&hit) {
                fired.push(format!("diff, {name}"));
            }
            run(&dir, &["config", "--unset", &format!("filter.{name}.clean")]);
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Runs in a child process that has a global config of its own: set only by the test below.
    #[test]
    fn a_global_required_driver_still_runs_child() {
        let Some(hit) = std::env::var_os("NERD_GIT_GLOBAL_HIT").map(PathBuf::from) else {
            return;
        };
        let dir = init_repo("global-driver");
        let root = dir.canonicalize().unwrap();
        std::fs::write(dir.join(".git/info/attributes"), "* filter=g\n").unwrap();
        std::fs::write(dir.join("README.md"), "changed\n").unwrap();
        let d = diff(&dir, &root.join("README.md"));
        assert!(d.contains("+changed"), "a required driver left on still diffs: {d:?}");
        assert!(ran(&hit), "the user's own global driver runs");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_global_required_driver_still_runs() {
        if !git_available() {
            return;
        }
        let tmp = std::env::temp_dir().join(format!("nerd-git-test-global-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let (global, hit, clean) = (tmp.join("gitconfig"), tmp.join("hit"), tmp.join("clean"));
        std::fs::write(&clean, format!("#!/bin/sh\ntouch '{}'\nexec cat\n", hit.display())).unwrap();
        std::fs::set_permissions(&clean, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(&global, format!("[filter \"g\"]\n\tclean = \"{}\"\n\trequired = true\n", clean.display())).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["git::tests::a_global_required_driver_still_runs_child", "--exact", "--test-threads=1"])
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("NERD_GIT_GLOBAL_HIT", &hit)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&out.stderr));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn adding_a_worktree_still_runs_the_repos_smudge() {
        if !git_available() {
            return;
        }
        let (dir, _, hit) = hit_repo("smudge");
        run(&dir, &["checkout", "-q", "--", "README.md"]);
        run(&dir, &["config", "filter.s.smudge", &format!("sh -c 'touch \"{}\"; cat'", hit.display())]);
        std::fs::write(dir.join(".git/info/attributes"), "* filter=s\n").unwrap();
        let added = add_worktree(&dir, "smudged").expect("creating a checkout");
        assert!(ran(&hit), "a worktree the user asked for checks out as their git would");
        remove_worktree(&dir, &added).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_submodules_diff_programs_do_not_run_through_the_parent() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("subdiff");
        let sub = init_repo("subdiff-src");
        run(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sub"]);
        run(&dir, &["commit", "-q", "-m", "sub"]);
        run(&dir, &["config", "diff.submodule", "diff"]);
        let touch = format!("sh -c 'touch \"{}\"; cat \"$0\"'", hit.display());
        let inner = dir.join("sub");
        run(&inner, &["config", "user.email", "test@example.com"]);
        run(&inner, &["config", "user.name", "Test"]);
        std::fs::write(inner.join("README.md"), "moved\n").unwrap();
        run(&inner, &["commit", "-q", "-am", "moved"]);
        std::fs::write(dir.join(".git/modules/sub/info/attributes"), "* diff=y\n").unwrap();
        // A path with glob characters in it would match `sub` as a pathspec.
        std::fs::write(dir.join("s*"), "x\n").unwrap();
        let mut fired = Vec::new();
        for (key, value) in [("diff.external", &touch), ("diff.y.textconv", &touch)] {
            run(&inner, &["config", key, value]);
            diff(&dir, &root.join("s*"));
            diff(&dir, &root.join("sub"));
            if ran(&hit) {
                fired.push(key);
            }
            run(&inner, &["config", "--unset", key]);
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&sub).unwrap();
    }

    #[test]
    fn a_file_named_star_previews_only_itself() {
        if !git_available() {
            return;
        }
        let (dir, root, _) = hit_repo("star");
        std::fs::write(dir.join("*"), "star\n").unwrap();
        run(&dir, &["add", "--", ":(literal)*"]);
        run(&dir, &["commit", "-q", "-m", "star"]);
        std::fs::write(dir.join("*"), "star changed\n").unwrap();
        let d = diff(&dir, &root.join("*"));
        assert!(d.contains("+star changed") && !d.contains("README.md"), "{d}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// As `git lfs install --local` leaves it: blanked but still required, git would refuse the diff.
    #[test]
    fn a_local_required_driver_is_blanked_and_the_diff_still_shows() {
        if !git_available() {
            return;
        }
        let (dir, root, hit) = hit_repo("required");
        run(&dir, &["config", "filter.lfsx.clean", &format!("sh -c 'touch \"{}\"; cat'", hit.display())]);
        run(&dir, &["config", "filter.lfsx.required", "true"]);
        std::fs::write(dir.join(".git/info/attributes"), "* filter=lfsx\n").unwrap();
        let d = diff(&dir, &root.join("README.md"));
        assert!(d.contains("+changed"), "{d:?}");
        assert!(!ran(&hit));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The (key, value) pairs a GIT_CONFIG_* env sets, checking its count.
    fn blanked(root: &Path) -> Vec<(String, String)> {
        let env: HashMap<String, String> = filters_off(root).unwrap().into_iter().collect();
        let n: usize = env.get("GIT_CONFIG_COUNT").map_or(0, |c| c.parse().unwrap());
        assert_eq!(env.len(), 2 * n + usize::from(n > 0), "{env:?}");
        (0..n).map(|i| (env[&format!("GIT_CONFIG_KEY_{i}")].clone(), env[&format!("GIT_CONFIG_VALUE_{i}")].clone())).collect()
    }

    /// Runs in a child process whose global config has an lfs extension and a filter driver: set only by the test below.
    #[test]
    fn a_repos_own_lfs_extensions_are_blanked_child() {
        if std::env::var_os("NERD_GIT_GLOBAL_LFS").is_none() {
            return;
        }
        let (dir, root, _) = hit_repo("lfs-env");
        assert!(blanked(&root).is_empty(), "the user's global keys stay");
        run(&dir, &["config", "lfs.extension.a.b.clean", "x"]);
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(blanked(&root), [pair("lfs.extension.a.b.clean", ""), pair("lfs.extension.a.b.smudge", "")]);
        run(&dir, &["config", "lfs.extension.x=y.smudge", "x"]);
        run(&dir, &["config", "filter.f.process", "x"]);
        run(&dir, &["config", "extensions.worktreeConfig", "true"]);
        run(&dir, &["config", "--worktree", "lfs.extension.w.clean", "x"]);
        let want = [
            pair("filter.f.clean", ""),
            pair("filter.f.process", ""),
            pair("filter.f.required", "false"),
            pair("filter.f.smudge", ""),
            pair("lfs.extension.a.b.clean", ""),
            pair("lfs.extension.a.b.smudge", ""),
            pair("lfs.extension.w.clean", ""),
            pair("lfs.extension.w.smudge", ""),
            pair("lfs.extension.x=y.clean", ""),
            pair("lfs.extension.x=y.smudge", ""),
        ];
        assert_eq!(blanked(&root), want);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repos_own_lfs_extensions_are_blanked() {
        if !git_available() {
            return;
        }
        let tmp = std::env::temp_dir().join(format!("nerd-git-test-global-lfs-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let global = tmp.join("gitconfig");
        std::fs::write(&global, "[lfs \"extension.g\"]\n\tclean = x\n\tsmudge = x\n[filter \"g\"]\n\tclean = x\n").unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["git::tests::a_repos_own_lfs_extensions_are_blanked_child", "--exact", "--test-threads=1"])
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("NERD_GIT_GLOBAL_LFS", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&out.stderr));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Runs in a child process with git-lfs installed globally, as `git lfs install` leaves it: set only by the test below.
    #[test]
    fn a_folders_lfs_extension_does_not_run_child() {
        if std::env::var_os("NERD_GIT_LFS").is_none() {
            return;
        }
        let (dir, root, hit) = hit_repo("lfs-ext");
        std::fs::write(dir.join(".gitattributes"), "*.bin filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        std::fs::write(dir.join("a.bin"), "one\n").unwrap();
        run(&dir, &["add", ".gitattributes", "a.bin"]);
        run(&dir, &["commit", "-q", "-m", "lfs"]);
        // git-lfs splits the command on spaces, so no `sh -c` here.
        let touch = dir.join(".git/touch");
        std::fs::write(&touch, format!("#!/bin/sh\ntouch '{}'\nexec cat\n", hit.display())).unwrap();
        std::fs::set_permissions(&touch, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        run(&dir, &["config", "lfs.extension.x.clean", &touch.to_string_lossy()]);
        run(&dir, &["config", "lfs.extension.x.smudge", &touch.to_string_lossy()]);
        run(&dir, &["config", "lfs.extension.x.priority", "0"]);
        let mut fired = Vec::new();
        // Same size as the index, so status must hash it through the filter.
        std::fs::write(dir.join("a.bin"), "two\n").unwrap();
        age(&dir.join("a.bin"));
        status_marks(&dir);
        if ran(&hit) {
            fired.push("status");
        }
        diff(&dir, &root.join("a.bin"));
        if ran(&hit) {
            fired.push("diff");
        }
        assert!(fired.is_empty(), "ran: {fired:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folders_lfs_extension_does_not_run() {
        if !git_available() {
            return;
        }
        if !Command::new("git-lfs").arg("version").output().is_ok_and(|o| o.status.success()) {
            println!("skipped: git-lfs is not on PATH");
            return;
        }
        let tmp = std::env::temp_dir().join(format!("nerd-git-test-lfs-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let global = tmp.join("gitconfig");
        std::fs::write(&global, "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = true\n").unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["git::tests::a_folders_lfs_extension_does_not_run_child", "--exact", "--test-threads=1"])
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("HOME", &tmp)
            .env("NERD_GIT_LFS", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&out.stderr));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Runs in a child process whose git fails `config`: set only by the test below.
    #[test]
    fn an_unreadable_config_runs_no_status_or_diff_child() {
        let Some(calls) = std::env::var_os("NERD_GIT_CALLS").map(PathBuf::from) else {
            return;
        };
        let dir = std::env::var_os("NERD_GIT_REPO").map(PathBuf::from).unwrap();
        std::fs::write(dir.join("README.md"), "changed\n").unwrap();
        assert!(status_marks(&dir).is_empty());
        assert!(diff(&dir, &dir.canonicalize().unwrap().join("README.md")).is_empty());
        let log = std::fs::read_to_string(&calls).unwrap();
        assert!(log.contains(" config ") && !log.contains(" status ") && !log.contains(" diff "), "{log}");
    }

    #[test]
    fn an_unreadable_config_runs_no_status_or_diff() {
        if !git_available() {
            return;
        }
        let dir = init_repo("unreadable");
        let bin = dir.join(".git/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
        let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
        let calls = dir.join(".git/calls");
        let fake = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in *\" config \"*) echo 'error: unknown option' >&2; exit 129 ;; esac\nexec '{real}' \"$@\"\n",
            calls.display()
        );
        std::fs::write(bin.join("git"), fake).unwrap();
        std::fs::set_permissions(bin.join("git"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["git::tests::an_unreadable_config_runs_no_status_or_diff_child", "--exact", "--test-threads=1"])
            .env("PATH", path)
            .env("NERD_GIT_CALLS", &calls)
            .env("NERD_GIT_REPO", &dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&out.stderr));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_git_that_hangs_is_killed_at_the_timeout() {
        if !git_available() {
            return;
        }
        let (dir, _, _) = hit_repo("fifo");
        let fifo = dir.join(".git/fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        run(&dir, &["config", "core.excludesFile", &fifo.to_string_lossy()]);
        assert!(status_marks(&dir).is_empty(), "a git that never answers gives no marks");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let left = Command::new("pgrep").args(["-f", &format!("{}.*status", dir.display())]).output().unwrap();
        // A failure leaves nothing behind: the stray git is this test's own.
        let _ = Command::new("pkill").args(["-9", "-f", &format!("{}.*status", dir.display())]).status();
        assert!(left.stdout.is_empty(), "git still running: {}", String::from_utf8_lossy(&left.stdout));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
