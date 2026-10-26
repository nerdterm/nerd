//! What's actually running in a pane: argv[0] lies, so we ask the system via `ps`.

use std::time::{Duration, Instant};

/// Known agents, by the name their binary ships under.
pub const AGENTS: &[&str] = &[
    "amp", "antigravity", "claude", "cline", "codex", "cursor", "devin", "droid",
    "gemini", "copilot", "grok", "hermes", "kilo", "kimi", "kiro", "maki", "muse",
    "opencode", "pi", "qodercli", "qwen",
];

/// Wrappers that hide the real command: for these, we look at the arguments.
pub(crate) const LAUNCHERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "python", "python3", "sh", "bash", "zsh", "fish", "env",
];

/// What the agent calls its binary -> the agent's name; agents with a manifest bring their own.
const ALIASES: &[(&str, &str)] = &[
    ("agy", "antigravity"),
    ("amp-local", "amp"),
    ("antigravity-cli", "antigravity"),
    ("devin-cli", "devin"),
    ("grok-build", "grok"),
    ("hermes-agent", "hermes"),
    ("kilo-code", "kilo"),
    ("kimi-code", "kimi"),
    ("kiro-cli", "kiro"),
    ("muse-cli", "muse"),
    ("muse-code", "muse"),
    ("qoder", "qodercli"),
    ("qodercn", "qodercli"),
];

/// Script extensions: `claude.js` and `claude` are the same agent.
const SCRIPT_EXTS: &[&str] = &[".js", ".mjs", ".cjs", ".py", ".ts"];

const PS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    pub pid: i32,
    /// Basename, as the system sees it.
    pub command: String,
    pub argv: Vec<String>,
    /// Set only after unwrapping wrappers (node, bun, deno, python, sh, bash, zsh, env).
    pub agent: Option<String>,
}

pub fn identify(command: &str, argv: &[String]) -> Option<String> {
    if let Some(agent) = agent_of_path(command) {
        return Some(agent);
    }
    if !LAUNCHERS.contains(&base_name(command)) {
        return None;
    }
    // `bash -c "cat codex"` executes text, not a file; ps merges quoting so we can't
    // tell words apart, so any `-c` shell invocation is left unidentified.
    let shell = matches!(base_name(command), "sh" | "bash" | "zsh" | "fish");
    if shell && argv.iter().skip(1).any(|a| a == "-c") {
        return None;
    }
    // The real command is the first arg that isn't a flag, `VAR=x`, or a launch
    // subcommand (`bun run <cmd>`); anything after that is its own arguments.
    argv.iter()
        .skip(1)
        .find(|a| !a.starts_with('-') && !a.contains('=') && !matches!(a.as_str(), "run" | "x" | "exec"))
        .and_then(|a| agent_of_path(a))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proc {
    pid: i32,
    ppid: i32,
    pgid: i32,
    argv: Vec<String>,
}

/// Shared snapshot so ten panes cost one `ps` call, not ten. We skip `comm`
/// (macOS: a full path that can contain spaces, unparseable from `args`) and use
/// argv[0] instead; `-ww` is required or ps truncates argv to terminal width.
pub fn ps_snapshot() -> Option<String> {
    static CACHE: std::sync::Mutex<Option<(Instant, String)>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, text)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(1)
    {
        return Some(text.clone());
    }
    let fresh = ps_snapshot_uncached();
    if let Some(text) = &fresh {
        *cache = Some((Instant::now(), text.clone()));
    }
    fresh
}

fn ps_snapshot_uncached() -> Option<String> {
    static PENDING: Pending = std::sync::Mutex::new(None);
    // Via the broker: `/bin/ps` is setuid, and the sandboxed daemon can't exec setuid.
    carried(&PENDING, crate::broker::ps)
}

type Pending = std::sync::Mutex<Option<std::sync::mpsc::Receiver<Option<String>>>>;

/// `output()` has no timeout, so `run` goes on a thread; one slower than PS_TIMEOUT is
/// waited for again by the next call, so a loaded machine gets late tables, not none.
fn carried(pending: &Pending, run: fn() -> Option<String>) -> Option<String> {
    // Not locked across the wait: a second caller asks on its own rather than wait twice.
    let waiting = pending.lock().unwrap_or_else(|e| e.into_inner()).take();
    let rx = waiting.unwrap_or_else(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run());
        });
        rx
    });
    match rx.recv_timeout(PS_TIMEOUT) {
        Ok(text) => text,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            *pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            None
        }
        Err(_) => None,
    }
}

/// Parses `ps -axww -o pid=,ppid=,pgid=,args=` output, skipping malformed lines.
fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines().filter_map(parse_line).collect()
}

/// The process table, parsed once for every pane that asks of it.
pub fn procs() -> Option<Vec<Proc>> {
    ps_snapshot_uncached().map(|t| parse_ps(&t))
}

fn parse_line(line: &str) -> Option<Proc> {
    let mut rest = line.trim_start();
    let mut nums = [0i32; 3];
    for n in &mut nums {
        let end = rest.find(' ')?;
        *n = rest[..end].parse().ok()?;
        rest = rest[end..].trim_start();
    }
    let argv: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return None;
    }
    Some(Proc { pid: nums[0], ppid: nums[1], pgid: nums[2], argv })
}

/// Whether an ancestor of `pid` is a `nerd --broker`, that is, `pid` runs inside some nerd's pane.
/// The working directory of `pid`, as the kernel keeps it: where its shell has `cd`'d to.
pub fn cwd_of(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: a zeroed proc_vnodepathinfo is a valid value, and proc_pidinfo writes at most its size.
    let mut info: nix::libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<nix::libc::proc_vnodepathinfo>() as i32;
    let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    // SAFETY: the path is MAXPATHLEN bytes, NUL-terminated by the kernel.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast(), 32 * 32) };
    let path = &bytes[..bytes.iter().position(|b| *b == 0)?];
    (!path.is_empty()).then(|| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

/// Every process below `root`, however deep, by parent; one reparented to launchd is no longer found.
pub(crate) fn descendants(procs: &[Proc], root: i32) -> Vec<i32> {
    let mut ours = vec![root];
    while let Some(p) = procs.iter().find(|p| !ours.contains(&p.pid) && ours.contains(&p.ppid)) {
        ours.push(p.pid);
    }
    ours.split_off(1)
}

pub(crate) fn under_broker(procs: &[Proc], pid: i32) -> bool {
    let parent = |pid: i32| procs.iter().find(|p| p.pid == pid).map(|p| p.ppid);
    let mut pid = pid;
    for _ in 0..32 {
        let Some(ppid) = parent(pid).filter(|p| *p > 1) else { return false };
        let Some(p) = procs.iter().find(|p| p.pid == ppid) else { return false };
        if let [.., exe, flag] = p.argv.as_slice()
            && flag == "--broker"
            && exe.ends_with("nerd")
        {
            return true;
        }
        pid = ppid;
    }
    false
}

/// `tcgetpgrp` isn't used, so this approximates the foreground as the newest descendant
/// that's its own group leader. A later background job or a pipeline can fool it.
pub(crate) fn foreground_of(procs: &[Proc], child_pid: i32) -> Option<Foreground> {
    let root = procs.iter().find(|p| p.pid == child_pid)?;

    // Walk down by ppid. ceiling: O(n) table scans per level, cheaper than
    // building an index for a couple hundred processes.
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i].pid;
        for p in procs {
            if p.ppid == parent && p.pid != parent {
                tree.push(p);
            }
        }
        i += 1;
    }

    // An agent outranks anything it spawned: Claude Code's `caffeinate` sibling would
    // otherwise win as "newest leader".
    let best = tree
        .iter()
        .find(|p| identify(&p.argv[0], &p.argv).is_some())
        .or_else(|| tree.iter().filter(|p| p.pid != child_pid && p.pid == p.pgid).max_by_key(|p| p.pid))
        .or_else(|| tree.iter().filter(|p| p.pid != child_pid).max_by_key(|p| p.pid))
        .unwrap_or(&root);

    Some(Foreground {
        pid: best.pid,
        command: base_name(&best.argv[0]).to_string(),
        agent: identify(&best.argv[0], &best.argv),
        argv: best.argv.clone(),
    })
}

fn agent_of_path(path: &str) -> Option<String> {
    let name = base_name(path);
    // Claude Code names its binary after a version number; recognize it by the path instead.
    if is_version(name) && path.contains("/claude/") {
        return Some("claude".to_string());
    }
    let stem = SCRIPT_EXTS
        .iter()
        .find_map(|e| name.strip_suffix(e))
        .unwrap_or(name);
    if let Some(m) = crate::agents::all().iter().find(|m| m.agent.names.iter().any(|n| n == stem)) {
        return Some(m.name.clone());
    }
    if let Some((_, canon)) = ALIASES.iter().find(|(a, _)| *a == stem) {
        return Some(canon.to_string());
    }
    AGENTS.contains(&stem).then(|| stem.to_string())
}

/// Strips a leading `-`: that's how ps shows a login shell (`-zsh`).
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path).trim_start_matches('-')
}

fn is_version(name: &str) -> bool {
    name.split('.').count() >= 2
        && name
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_process_s_folder_is_where_it_is_now() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(super::cwd_of(std::process::id() as i32).map(|p| p.canonicalize().unwrap()), Some(here));
        assert_eq!(super::cwd_of(-1), None);
    }

    #[test]
    fn a_table_slower_than_the_timeout_reaches_the_next_call() {
        static PENDING: super::Pending = std::sync::Mutex::new(None);
        fn slow() -> Option<String> {
            std::thread::sleep(super::PS_TIMEOUT + Duration::from_millis(500));
            Some("late".into())
        }
        assert_eq!(super::carried(&PENDING, slow), None);
        assert_eq!(super::carried(&PENDING, slow).as_deref(), Some("late"));
    }

    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn plain_agent_binary() {
        assert_eq!(identify("claude", &argv("claude")), Some("claude".into()));
        assert_eq!(
            identify("/opt/bin/codex", &argv("/opt/bin/codex --resume")),
            Some("codex".into())
        );
    }

    #[test]
    fn launchers_are_unwrapped() {
        assert_eq!(identify("node", &argv("node /x/claude.js")), Some("claude".into()));
        assert_eq!(identify("bun", &argv("bun run /y/opencode")), Some("opencode".into()));
        assert_eq!(
            identify("/usr/bin/env", &argv("/usr/bin/env NODE_ENV=x gemini")),
            Some("gemini".into())
        );
    }

    #[test]
    fn claude_versioned_binary() {
        let p = "/Users/x/.local/share/claude/versions/2.1.258";
        assert_eq!(identify(p, &[p.to_string()]), Some("claude".into()));
        assert_eq!(identify("/opt/tools/2.1.258", &argv("/opt/tools/2.1.258")), None);
    }

    #[test]
    fn shells_and_strangers_are_not_agents() {
        assert_eq!(identify("zsh", &argv("zsh")), None);
        assert_eq!(identify("-zsh", &argv("-zsh")), None);
        assert_eq!(identify("python3", &argv("python3 -m something")), None);
        assert_eq!(identify("vim", &argv("vim notes.md")), None);
        assert_eq!(identify("bash", &argv("bash -c cat codex")), None);
        assert_eq!(identify("node", &argv("node /x/server.js --name claude")), None);
        assert_eq!(identify("sh", &argv("sh /tmp/bin/claude")), Some("claude".into()));
    }

    #[test]
    fn aliases_resolve() {
        for m in crate::agents::builtins() {
            for n in &m.agent.names {
                assert_eq!(identify(n, std::slice::from_ref(n)), Some(m.name.clone()), "{n}");
            }
        }
        for (alias, canon) in ALIASES {
            assert_eq!(identify(alias, &[alias.to_string()]), Some((*canon).into()), "{alias}");
        }
        assert_eq!(identify("node", &argv("node /x/cursor-agent.js")), Some("cursor".into()));
    }

    #[test]
    fn golden_detection_lists_stay_as_they_were() {
        let built = crate::agents::builtins();
        let own = built.iter().flat_map(|m| m.agent.names.iter().filter(|n| **n != m.name).map(|n| format!("{n} {}", m.name)));
        let mut aliases: Vec<String> = ALIASES.iter().map(|(a, c)| format!("{a} {c}")).chain(own).collect();
        aliases.sort();
        let text = format!("agents: {}\nlaunchers: {}\naliases:\n{}\n", AGENTS.join(" "), LAUNCHERS.join(" "), aliases.join("\n"));
        crate::hook::tests::golden("detect.txt", &text);
    }

    const PS: &str = "\
    1     0     1 /sbin/launchd
  500     1   500 /bin/login -pf user
  501   500   501 -zsh
  640   501   640 /Users/x/.local/share/claude/versions/2.1.258 --resume
  641   640   640 /usr/bin/grep foo
  642   640   642 /usr/bin/caffeinate -dims
  700     1   700 /usr/bin/ssh host
bogus line without numbers
";

    #[test]
    fn ps_parser_finds_the_foreground_agent() {
        let procs = parse_ps(PS);
        assert_eq!(procs.len(), 7, "the garbage line should be dropped");
        assert_eq!(procs[2].argv, vec!["-zsh".to_string()]);

        let fg = foreground_of(&procs, 501).expect("shell's descendant");
        assert_eq!(fg.pid, 640);
        assert_eq!(fg.command, "2.1.258");
        assert_eq!(fg.agent, Some("claude".into()), "caffeinate doesn't override its agent");

        // with no descendants, the foreground is the process itself
        let alone = foreground_of(&procs, 700).expect("its own foreground");
        assert_eq!(alone.pid, 700);
        assert_eq!(alone.agent, None);

        assert_eq!(foreground_of(&procs, 99999), None);
    }

    #[test]
    fn a_process_under_any_nerd_broker_is_inside_a_pane() {
        let procs = parse_ps(
            "  9618     1  9618 /Users/x/.local/bin/nerd --daemon
  9619  9618  9618 /Users/x/.local/bin/nerd --broker
  9663  9619  9663 -zsh
 11908  9663 11908 claude
 73876 11908 73876 /Users/x/.local/bin/nerd --stop
   500     1   500 /bin/login -pf x
   501   500   501 -zsh
   502   501   502 /Users/x/.local/bin/nerd --stop
   600   601   600 sh
   601   600   601 sh
",
        );
        assert!(under_broker(&procs, 73876), "an agent's tool shell, three levels under the broker");
        assert!(under_broker(&procs, 9663), "the pane's shell itself");
        assert!(!under_broker(&procs, 502), "a terminal's shell");
        assert!(!under_broker(&procs, 9619), "the broker is not under itself");
        assert!(!under_broker(&procs, 600), "a loop ends at the cap");
        assert!(!under_broker(&procs, 4242), "a process not in the listing");
    }
}
