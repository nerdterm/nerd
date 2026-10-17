//! Pane isolation: the wrapper that starts a pane in its folder or account, and its network proxy.

use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction, signal};

use crate::layout::PaneId;
use crate::plugins::{canon, quote};
use crate::policy::{Files, Isolation, Network};

impl crate::app::App {
    /// The pane split from, else the tab's default, else the space's: a split of an isolated pane never opens a hole.
    pub(crate) fn isolation_for_new(&self, si: usize, ti: Option<usize>, from: Option<PaneId>) -> Isolation {
        let sp = self.spaces.get(si);
        from.and_then(|id| self.pane(id))
            .map(|p| p.isolation.clone())
            .or_else(|| ti.and_then(|ti| sp?.tabs.get(ti)?.isolation_new.clone()))
            .or_else(|| sp?.isolation_new.clone())
            .unwrap_or_default()
    }
}

fn other(e: impl Into<String>) -> std::io::Error {
    std::io::Error::other(e.into())
}

fn set(env: &mut Vec<(String, String)>, k: &str, v: String) {
    env.retain(|(n, _)| n != k);
    env.push((k.to_string(), v));
}

/// The pane's argv: unchanged when open, else `nerd --pane-exec` with everything it needs as arguments (sudo resets the environment).
pub fn wrap(id: PaneId, argv: &[String], cwd: &Path, iso: &Isolation, env: &mut Vec<(String, String)>) -> std::io::Result<Vec<String>> {
    if iso.is_open() {
        return Ok(argv.to_vec());
    }
    env.retain(|(k, _)| k != "NERD_STATE");
    let var = |env: &[(String, String)], k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let network = match iso.network {
        Network::Open => "open",
        Network::None => "none",
        Network::Providers(_) => "provider",
    };
    let mut flags: Vec<String> = ["--pane", &id.to_string(), "--network", network].map(String::from).into();
    if let Network::Providers(agents) = &iso.network {
        let net = providers_net(agents).map_err(other)?;
        flags.extend(["--hosts".into(), crate::proxy::Net::env(&net.hosts, &net.ports)]);
    }
    let (head, bin, msock, uid, tail) = match &iso.files {
        Files::Account(name) => {
            if !crate::policy::current().accounts.iter().any(|a| &a.name == name) {
                return Err(other(format!("{name} is not an admitted account")));
            }
            // Checked again at every spawn: the pane never starts against a door that failed them.
            let sock = ensure_shared()?;
            let shell = login_shell(name).ok_or_else(|| other(format!("{name} has no login shell")))?;
            let exe = std::env::current_exe()?;
            // A folder that is not root's is not fixed by installing again, so only the copy's own faults name the commands.
            verify_root_copy(Path::new(ROOT_COPY), &exe, 0, Path::new("/")).map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => other(format!("{e}, so panes of other accounts cannot start")),
                _ => other(format!("{e}; run: {}", install_commands(&exe).join(" && "))),
            })?;
            let bin = ROOT_COPY.to_string();
            let (sock, uid) = (sock.to_string_lossy().into_owned(), crate::policy::owner_uid().to_string());
            set(env, "NERD_MSOCK", sock.clone());
            set(env, "NERD_MSOCK_UID", uid.clone());
            // The account's own login shell runs: the pane's argv is the owner's.
            (["/usr/bin/sudo", "-u", name, "-i", &bin].map(String::from).to_vec(), bin, sock, uid, vec![shell, "-l".into()])
        }
        _ => {
            let msock = var(env, "NERD_MSOCK").unwrap_or_else(|| crate::session::msock_path().to_string_lossy().into_owned());
            let uid = var(env, "NERD_MSOCK_UID").unwrap_or_else(|| crate::policy::owner_uid().to_string());
            (vec![exe.clone()], exe, msock, uid, argv.to_vec())
        }
    };
    let files = if iso.files == Files::Folder { "folder" } else { "everything" };
    flags.extend(["--files", files, "--cwd", &cwd.to_string_lossy(), "--msock", &msock, "--msock-uid", &uid, "--bin", &bin].map(String::from));
    if let Ok(dump) = std::env::var("NERD_HOOK_DUMP") {
        flags.extend(["--dump".into(), dump]);
    }
    set(env, "NERD_BIN", bin);
    Ok(head.into_iter().chain(["--pane-exec".into()]).chain(flags).chain(["--".to_string()]).chain(tail).collect())
}

static SHARED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// What a pane set to "only these providers" reaches: the chosen agents' hosts on the ports they all name.
pub fn providers_net(agents: &[String]) -> Result<crate::proxy::Net, String> {
    merge(agents.iter().map(|a| (a.as_str(), crate::agents::get(a).map(|m| m.agent.network).unwrap_or_default())))
}

/// The proxy keeps one port list for all its hosts, so agents naming different ports cannot share a pane.
fn merge<'a>(agents: impl Iterator<Item = (&'a str, crate::agents::Network)>) -> Result<crate::proxy::Net, String> {
    let (mut hosts, mut ports) = (Vec::new(), None::<(&str, Vec<u16>)>);
    for (agent, network) in agents {
        let crate::agents::Network::Internet { hosts: h, ports: p } = network else { return Err(format!("{agent} names no hosts")) };
        match &ports {
            Some((first, q)) if *q != p => return Err(format!("{first} and {agent} name different ports; choose them in separate panes")),
            Some(_) => {}
            None => ports = Some((agent, p)),
        }
        hosts.extend(h);
    }
    let (_, ports) = ports.ok_or("no provider is chosen")?;
    hosts.sort();
    hosts.dedup();
    Ok(crate::proxy::Net { hosts, ports })
}

/// `/tmp/nerd-<owner uid>/<instance>.msock`, where admitted accounts' panes reach messages: bound once, checked at every use,
/// and bound again only when it went missing.
pub fn ensure_shared() -> std::io::Result<PathBuf> {
    if std::env::var_os("NERD_SOCKET").is_some() || std::env::var_os("NERD_STATE_DIR").is_some() {
        return Err(other("other accounts need a server at the default state root"));
    }
    let uid = crate::policy::owner_uid();
    let dir = PathBuf::from(format!("/tmp/nerd-{uid}"));
    let mut bound = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sock) = bound.as_ref()
        && verify_dir(&dir, uid).is_ok()
        && std::fs::symlink_metadata(sock).is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()))
    {
        return Ok(sock.clone());
    }
    let name = match crate::session::instance_name() {
        n if n.is_empty() => "main.msock".to_string(),
        n => format!("{n}.msock"),
    };
    let (listener, sock) = open_door(&dir, uid, &name).inspect_err(|e| crate::log::error(&format!("{e}; other accounts stay off")))?;
    if !crate::msock::start_shared(listener) {
        let _ = std::fs::remove_file(&sock);
        return Err(other("the message socket is not running, so other accounts stay off"));
    }
    crate::log::info(&format!("other accounts reach messages at {}", sock.display()));
    *bound = Some(sock.clone());
    Ok(sock)
}

/// The shared socket this server bound, at its stop.
pub fn remove_shared() {
    if let Some(sock) = SHARED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = std::fs::remove_file(sock);
    }
}

/// An account's login shell from the user database.
fn login_shell(name: &str) -> Option<String> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: a zeroed passwd is a valid value; getpwnam_r writes into it and `buf` only, and `out` points at `pw` or is null.
    let mut pw: nix::libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as nix::libc::c_char; 4096];
    let mut out: *mut nix::libc::passwd = std::ptr::null_mut();
    let r = unsafe { nix::libc::getpwnam_r(c.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r != 0 || out.is_null() || pw.pw_shell.is_null() {
        return None;
    }
    // SAFETY: pw_shell points into `buf`, which is still alive.
    Some(unsafe { std::ffi::CStr::from_ptr(pw.pw_shell) }.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// What a profile names, gathered once so tests can give their own.
struct Places {
    home: PathBuf,
    root: PathBuf,
    tmp: PathBuf,
    exe: PathBuf,
    /// Every agent's: the agent is chosen after the pane starts.
    data: Vec<PathBuf>,
    /// Every agent's settings folder, and the file beside it of the same name (`~/.claude.json`): not writable but for `data`.
    roots: Vec<PathBuf>,
    hook_files: Vec<PathBuf>,
    hook_dirs: Vec<PathBuf>,
    /// `/private/var/run`: launchd's ssh agent and Docker's socket.
    run: PathBuf,
    /// The pane's own `SSH_AUTH_SOCK`.
    agent: Option<PathBuf>,
}

impl Places {
    fn here() -> Result<Places, String> {
        let home = crate::paths::home().unwrap_or_default();
        if !home.is_absolute() || home == Path::new("/") {
            return Err("nerd has no home folder, so it cannot keep this pane out of ~/.ssh".into());
        }
        let agents = crate::agents::load_admitting(&crate::paths::state_root().join("plugins"), &|_| true);
        Ok(Places {
            root: crate::paths::state_root(),
            tmp: PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/private/tmp".into())),
            exe: std::env::current_exe().map_err(|e| e.to_string())?,
            data: agents.iter().flat_map(|m| m.data_paths(&home)).collect(),
            roots: agents.iter().flat_map(|m| m.dir(&home).into_iter().chain(m.agent.data.iter().filter_map(|d| crate::agents::data_root(d)).map(|r| home.join(r)))).flat_map(|r| [r.with_extension("json"), r]).collect(),
            hook_files: agents.iter().filter_map(|m| m.config(&home)).collect(),
            hook_dirs: agents.iter().filter_map(|m| m.dir(&home)).map(|d| d.join("hooks")).collect(),
            run: "/private/var/run".into(),
            agent: std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from).filter(|p| p.is_absolute()),
            home,
        })
    }
}

/// Under HOME, run later by a shell, git, ssh, an editor, tmux, a package manager or toolchain from its cache, launchd or `defaults`, or ahead on PATH:
/// never writable, even when the pane's folder is HOME itself.
const RUN_LATER: &[&str] = &[".zshrc", ".zprofile", ".zshenv", ".zlogin", ".zlogout", ".bashrc", ".bash_profile", ".bash_login", ".bash_logout", ".profile", ".gitconfig", ".netrc", ".npmrc", ".cargo/config.toml", ".vimrc", ".tmux.conf", ".emacs"];
const RUN_LATER_DIRS: &[&str] = &[
    ".config/git", ".config/fish", "Library/LaunchAgents", ".ssh", ".aws", ".gnupg", ".docker", ".kube", ".azure", ".config/gh", ".config/gcloud", ".cargo/bin", ".local/bin",
    ".vim", ".config/nvim", ".config/tmux", ".emacs.d", "Library/Preferences", ".pyenv/shims", ".rbenv/shims", ".nvm", ".bun/bin", "go/bin", ".volta/bin", ".deno/bin", ".asdf/shims", ".local/share/nvim",
    "Library/Caches/pnpm", "Library/Caches/uv", "Library/Caches/deno", "Library/Caches/go-build", "Library/Caches/ms-playwright", "Library/Caches/pypoetry", "Library/Caches/Yarn", "Library/Caches/node-gyp",
    "Library/Caches/pip", "Library/Caches/Cypress", "Library/Caches/org.swift.swiftpm", "Library/Caches/copilot", "go/pkg/mod",
    // Agents' own: sourced before each command, ahead on PATH, downloaded tools, git repos the agent runs git in, tasks other sessions take up.
    ".claude/shell-snapshots", ".claude/session-env", ".codex/shell_snapshots", ".codex/tmp", ".codex/.tmp", ".gemini/tmp/bin", ".gemini/history", ".local/share/opencode/snapshot", ".claude/tasks",
];
/// Under HOME, sockets that act as the owner outside the pane: ssh agents and connections, Docker, colima, OrbStack, Rancher, lima, podman.
const SOCKETS: &[&str] = &[
    ".ssh", ".docker", "Library/Containers/com.docker.docker", ".colima", ".orbstack", ".rd", ".lima", "Library/Application Support/rancher-desktop", ".local/share/containers/podman",
    "Library/Group Containers/2BUA8C4S2C.com.1password/t", "Library/Containers/com.maxgoedjen.Secretive.SecretAgent",
];
const UNREAD: &[&str] = &[".ssh", ".aws", ".gnupg", ".netrc", ".docker", ".kube", ".azure", ".config/gh", ".config/gcloud", "Library/Mail", "Library/Messages", "Library/Cookies"];

/// The pane's Seatbelt profile. An account pane passes `Everything` or `Account`: the account confines its files.
pub fn profile(files: &Files, network: &Network, cwd: &Path, msock: &Path, port: Option<u16>) -> Result<String, String> {
    text(&Places::here()?, files, network, cwd, msock, port)
}

fn rules(kind: &str, ps: impl IntoIterator<Item = PathBuf>) -> Result<String, String> {
    ps.into_iter().map(|p| quote(&canon(&p)).map(|s| format!(" ({kind} {s})"))).collect()
}

/// A data path as a rule: a subpath, or a regex when its last part has a `*` (`~/.copilot/session-store.db*`).
fn data_rules(ps: &[PathBuf]) -> Result<String, String> {
    ps.iter()
        .map(|p| match p.file_name().map(|n| n.to_string_lossy()).filter(|n| n.contains('*')) {
            Some(name) => {
                let dir = canon(p.parent().unwrap_or(Path::new("/")));
                quote(&dir)?;
                let name = name.split('*').map(crate::plugins::regex_escape).collect::<Vec<_>>().join("[^/]*");
                Ok(format!(" (regex #\"^{}/{name}$\")", crate::plugins::regex_escape(&dir.to_string_lossy())))
            }
            None => rules("subpath", [p.clone()]),
        })
        .collect()
}

fn text(p: &Places, files: &Files, network: &Network, cwd: &Path, msock: &Path, port: Option<u16>) -> Result<String, String> {
    let root = canon(&p.root);
    let tmp = canon(&p.tmp);
    let msock = quote(&canon(msock))?;
    // Allow default: a pane needs its tty. The last matching rule wins, so the denies after the allows hold.
    let mut out = String::from("(version 1)\n(allow default)\n");
    if *files == Files::Folder {
        // Temp folders and caches stay writable: compilers, package managers and agents need them.
        let writes = [cwd.to_path_buf(), tmp.clone(), "/private/tmp".into(), "/private/var/folders".into(), p.home.join("Library/Caches")];
        // A folder holding HOME writes no dot entry, ~/Library or ~/bin there but caches and the data paths below; cfprefsd still writes `defaults`.
        let home = canon(&p.home);
        quote(&home)?;
        let esc = crate::plugins::regex_escape(&home.to_string_lossy());
        let dots = if home.starts_with(canon(cwd)) {
            format!(
                "(deny file-write* (regex #\"^{esc}/\\.[^/]\"){})\n(allow file-write*{})\n",
                rules("subpath", ["Library", "bin"].map(|d| home.join(d)))?,
                rules("subpath", writes[1..].iter().cloned())?
            )
        } else {
            String::new()
        };
        let literal = [p.exe.clone()].into_iter().chain(RUN_LATER.iter().map(|f| p.home.join(f))).chain(p.hook_files.iter().cloned());
        let dirs = [root.clone()].into_iter().chain(RUN_LATER_DIRS.iter().map(|d| p.home.join(d))).chain(p.hook_dirs.iter().cloned());
        // The door other accounts exec nerd from, Claude Code's message sockets, agents' memory (Claude, Qwen), Codex's databases shared by its sessions (queued prompts, remote control) and Qwen's workflow scripts a resume runs, which the owner's own sessions read.
        let doors = format!(
            " (regex #\"^/private/tmp/nerd-[0-9]+(/|$)\") (regex #\"^/private/tmp/cc-socks\") (regex #\"^{}/cc-socks\") (regex #\"^{esc}/\\.[^/]+/projects/[^/]+/memory(/|$)\") (regex #\"^{esc}/\\.codex/[^/]*\\.sqlite\") (regex #\"^{esc}/\\.qwen/projects/[^/]+/workflows(/|$)\")",
            crate::plugins::regex_escape(&tmp.to_string_lossy())
        );
        out += &format!(
            "(deny file-write*)\n(allow file-write*{} (regex #\"^/dev/\"))\n{dots}(deny file-write*{})\n(allow file-write*{})\n(deny file-write*{}{}{doors})\n(deny file-read-data{})\n",
            rules("subpath", writes)?,
            rules("subpath", p.roots.iter().cloned())?,
            data_rules(&p.data)?,
            rules("subpath", dirs)?,
            rules("literal", literal)?,
            rules("subpath", UNREAD.iter().map(|s| p.home.join(s)).chain([root.clone()]))?,
        );
    }
    match network {
        Network::Open if *files == Files::Folder => {
            // DNS's own socket must keep working, so unix sockets are denied by place; Codex's daemon runs its tools outside the pane, an ssh agent signs and Docker runs as the owner.
            let run = canon(&p.run);
            out += &format!(
                "(deny network-outbound (regex #\"^/private/tmp/cc-socks\") (regex #\"^{}/cc-socks\") (regex #\"^{}/com\\.apple\\.launchd\\.[^/]+/Listeners$\"){}{})\n(allow network-outbound (literal {msock}))\n",
                crate::plugins::regex_escape(&tmp.to_string_lossy()),
                crate::plugins::regex_escape(&run.to_string_lossy()),
                rules("literal", [run.join("docker.sock")].into_iter().chain(p.agent.clone()))?,
                rules("subpath", [root.clone(), p.home.join(".codex"), tmp.join("podman")].into_iter().chain(SOCKETS.iter().map(|s| p.home.join(s))))?
            );
        }
        Network::Open => {}
        Network::None | Network::Providers(_) => {
            let local = match (network, port) {
                (Network::None, _) => "*".to_string(),
                (_, Some(port)) => port.to_string(),
                _ => return Err("only these providers needs the proxy's port".into()),
            };
            out += &format!("(deny network*)\n(allow network-bind network-inbound (local unix-socket))\n(allow network-outbound (literal {msock}))\n(allow network-outbound (remote ip \"localhost:{local}\"))\n");
        }
    }
    Ok(out)
}

/// `/tmp/nerd-<n>` when `msock` is inside one, with `n`.
fn shared_dir_of(msock: &Path) -> Option<(&Path, &str)> {
    let dir = msock.parent()?;
    let n = dir.file_name()?.to_str()?.strip_prefix("nerd-")?;
    matches!(dir.parent()?.to_str()?, "/tmp" | "/private/tmp").then_some((dir, n))
}

/// The directory is nerd's own: a real directory, owned by `uid`, mode exactly 0711.
fn verify_dir(dir: &Path, uid: u32) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(dir)?;
    let mode = m.mode() & 0o7777;
    if !m.file_type().is_dir() || m.uid() != uid || mode != 0o711 {
        return Err(other(format!("{} is not nerd's own (owner {}, mode {mode:o})", dir.display(), m.uid())));
    }
    Ok(())
}

/// The nerd account panes run: sudo execs it after asking for a password, so only root may change it.
/// Not under /usr/local: Intel Homebrew gives that folder to the user.
pub const ROOT_COPY: &str = "/Library/Application Support/nerd/bin/nerd";

/// What the owner runs once, and again after each update, to put this nerd at `ROOT_COPY`.
pub fn install_commands(exe: &Path) -> [String; 2] {
    let dir = Path::new(ROOT_COPY).parent().unwrap_or(Path::new("/")).display().to_string();
    [format!("sudo install -d -o root -g wheel -m 755 \"{dir}\""), format!("sudo install -o root -g wheel -m 755 {} \"{ROOT_COPY}\"", exe.display())]
}

/// `at` is a regular file and every folder above it up to `top` a real one, all owned by `owner` (root in use) and
/// writable by nobody else, with the same bytes as `exe`: a folder anyone else may write could be renamed away and
/// replaced while sudo asks for the password.
fn verify_root_copy(at: &Path, exe: &Path, owner: u32, top: &Path) -> std::io::Result<()> {
    let say = |why: &str| other(format!("{} {why}", at.display()));
    for dir in at.ancestors().skip(1) {
        let d = std::fs::symlink_metadata(dir).map_err(|_| say("is missing"))?;
        if !d.file_type().is_dir() || d.uid() != owner || d.mode() & 0o022 != 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} is not a folder only root can change", dir.display())));
        }
        if dir == top {
            break;
        }
    }
    let m = std::fs::symlink_metadata(at).map_err(|_| say("is missing"))?;
    if !m.file_type().is_file() || m.uid() != owner || m.mode() & 0o022 != 0 {
        return Err(say("is not a file only root can change"));
    }
    let same = m.len() == std::fs::metadata(exe)?.len() && {
        use std::io::Read as _;
        let (mut a, mut b) = (std::io::BufReader::new(std::fs::File::open(at)?), std::io::BufReader::new(std::fs::File::open(exe)?));
        let (mut x, mut y) = ([0u8; 65536], [0u8; 65536]);
        loop {
            let n = a.read(&mut x)?;
            if n == 0 {
                break true;
            }
            b.read_exact(&mut y[..n])?;
            if x[..n] != y[..n] {
                break false;
            }
        }
    };
    if !same {
        return Err(say("is not the nerd that runs now"));
    }
    Ok(())
}

/// Makes `dir` (mode 0711) or checks the one there, and binds `name` there, open to every account.
fn open_door(dir: &Path, uid: u32, name: &str) -> std::io::Result<(std::os::unix::net::UnixListener, PathBuf)> {
    use std::os::unix::fs::DirBuilderExt as _;
    match std::fs::DirBuilder::new().mode(0o711).create(dir) {
        // The umask may have taken bits off; /tmp is sticky, so nobody else can swap it meanwhile.
        Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o711))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    verify_dir(dir, uid)?;
    // A copy an earlier build put here; unlink takes a link itself, never what it points at.
    let _ = std::fs::remove_file(dir.join("nerd"));
    let sock = dir.join(name);
    let _ = std::fs::remove_file(&sock);
    let l = std::os::unix::net::UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666))?;
    Ok((l, sock))
}

fn fail(why: impl std::fmt::Display) -> ! {
    let _ = writeln!(std::io::stderr(), "nerd: {why}");
    std::process::exit(1)
}

/// The first data path that exists and cannot be written here.
fn unwritable(data: &[PathBuf]) -> Option<&Path> {
    data.iter().map(PathBuf::as_path).find(|p| match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => {
            let probe = p.join(format!(".nerd-write-check-{}", std::process::id()));
            std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).and_then(|_| std::fs::remove_file(&probe)).is_err()
        }
        Ok(_) => std::fs::OpenOptions::new().append(true).open(p).is_err(),
        Err(_) => false,
    })
}

/// Applies `profile` and execs `cmd`; returns only on failure.
fn become_pane(mut cmd: std::process::Command, profile: Option<&str>, data: &[PathBuf]) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    // Whatever nerd was started under (nohup, a background job), the pane's program gets the default dispositions.
    for s in [Signal::SIGHUP, Signal::SIGINT, Signal::SIGQUIT, Signal::SIGTERM, Signal::SIGPIPE] {
        // SAFETY: setting dispositions installs no handler.
        let _ = unsafe { signal(s, SigHandler::SigDfl) };
    }
    if let Some(p) = profile
        && let Err(e) = crate::sandbox::apply_text(p)
    {
        return other(format!("sandbox not applied: {e}"));
    }
    // An agent that cannot write its session folder must be visible, not silent.
    if let Some(p) = unwritable(data) {
        let _ = writeln!(std::io::stderr(), "nerd: {} is not writable in this pane", p.display());
    }
    cmd.exec()
}

static CHILD: AtomicI32 = AtomicI32::new(0);

/// A hangup or a stop reaches the pane's program, which is in a group of its own.
extern "C" fn pass_on(sig: nix::libc::c_int) {
    // SAFETY: kill is async-signal-safe.
    unsafe { nix::libc::kill(CHILD.load(Ordering::Relaxed), sig) };
}

/// `nerd --pane-exec <flags> -- <argv>`: the pane's own environment, its profile, then its program; forked off a proxy for "only these providers".
pub fn pane_exec_main(args: &[String]) -> std::io::Result<()> {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (flags, argv) = (&args[..end], args.get(end + 1..).unwrap_or_default());
    let get = |k: &str| flags.chunks(2).find(|c| c[0] == k).and_then(|c| c.get(1)).map(String::as_str);
    let need = |k: &str| get(k).unwrap_or_else(|| fail(format!("--pane-exec: no {k}")));
    let Some(program) = argv.first() else { fail("--pane-exec: no program given") };
    let pane: PaneId = need("--pane").parse().unwrap_or_else(|_| fail("--pane-exec: --pane is not a number"));
    let uid: u32 = need("--msock-uid").parse().unwrap_or_else(|_| fail("--pane-exec: --msock-uid is not a number"));
    let msock = PathBuf::from(need("--msock"));
    let cwd = PathBuf::from(need("--cwd"));
    if let Some((dir, n)) = shared_dir_of(&msock)
        && (n != uid.to_string() || verify_dir(dir, uid).is_err())
    {
        fail(format!("{} is not nerd's own; this pane does not start", dir.display()));
    }
    let files = match need("--files") {
        "folder" => Files::Folder,
        "everything" => Files::Everything,
        f => fail(format!("--pane-exec: --files {f}?")),
    };
    let mut cmd = crate::spawn::cmd(program);
    cmd.args(&argv[1..]).env("NERD_PANE", pane.to_string()).env("NERD_MSOCK", &msock).env("NERD_MSOCK_UID", uid.to_string()).env("NERD_BIN", need("--bin")).env_remove("NERD_STATE");
    if let Some(d) = get("--dump") {
        cmd.env("NERD_HOOK_DUMP", d);
    }
    let data = if files == Files::Folder { Places::here().map(|p| p.data).unwrap_or_default() } else { Vec::new() };
    let network = match need("--network") {
        "open" => Network::Open,
        "none" => Network::None,
        "provider" => Network::Providers(Vec::new()),
        n => fail(format!("--pane-exec: --network {n}?")),
    };
    if !matches!(network, Network::Providers(_)) {
        let confined = files == Files::Folder || network != Network::Open;
        let profile = profile(&files, &network, &cwd, &msock, None).unwrap_or_else(|e| fail(e));
        fail(become_pane(cmd, confined.then_some(profile.as_str()), &data));
    }
    let net = crate::proxy::Net::parse(need("--hosts")).unwrap_or_else(|| fail("--pane-exec: --hosts does not parse"));
    let listeners = crate::proxy::bind().unwrap_or_else(|e| fail(format!("no port for the pane's proxy: {e}")));
    let port = listeners.0.local_addr()?.port();
    for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        cmd.env(k, format!("http://127.0.0.1:{port}"));
    }
    cmd.env("NODE_USE_ENV_PROXY", "1").env("NO_PROXY", "").env("no_proxy", "");
    let profile = profile(&files, &network, &cwd, &msock, Some(port)).unwrap_or_else(|e| fail(e));
    // SAFETY: no thread exists yet, so the child may allocate; it never returns into the proxy's code.
    match unsafe { nix::unistd::fork() }? {
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::setpgid(nix::unistd::Pid::from_raw(0), nix::unistd::Pid::from_raw(0));
            // A group not yet in front would be stopped by its own tcsetpgrp; with no terminal (a test) there is nothing to take.
            // SAFETY: setting dispositions installs no handler.
            let _ = unsafe { signal(Signal::SIGTTOU, SigHandler::SigIgn) };
            let _ = nix::unistd::tcsetpgrp(std::io::stdin(), nix::unistd::getpgrp());
            let _ = unsafe { signal(Signal::SIGTTOU, SigHandler::SigDfl) };
            let e = become_pane(cmd, Some(&profile), &data);
            let _ = writeln!(std::io::stderr(), "nerd: {e}");
            // SAFETY: ends the child without running the parent's exit handlers.
            unsafe { nix::libc::_exit(1) }
        }
        nix::unistd::ForkResult::Parent { child } => {
            CHILD.store(child.as_raw(), Ordering::Relaxed);
            // Ctrl+C and the job-control keys are the program's: none of them may stop its proxy.
            // SAFETY: SigIgn installs no handler; pass_on only calls kill.
            for s in [Signal::SIGINT, Signal::SIGQUIT, Signal::SIGTSTP, Signal::SIGTTIN, Signal::SIGTTOU, Signal::SIGPIPE] {
                let _ = unsafe { signal(s, SigHandler::SigIgn) };
            }
            for s in [Signal::SIGHUP, Signal::SIGTERM] {
                let _ = unsafe { sigaction(s, &SigAction::new(SigHandler::Handler(pass_on), SaFlags::SA_RESTART, SigSet::empty())) };
            }
            crate::proxy::run(child, net, listeners, Some(pane))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder outside the temp trees a "this folder only" pane may write: the build's own target folder.
    fn scratch(name: &str) -> PathBuf {
        let d = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).join(format!("nt-s5-{name}-{}", std::process::id()));
        let real = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert!(!["/private/tmp", "/private/var/folders"].iter().any(|t| real.starts_with(t)), "the isolation tests need the checkout outside the temp folders a pane may write, not in {}", real.display());
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["home/.claude/hooks", "home/.claude/projects", "home/.claude/skills", "home/.codex", "home/.copilot", "home/.gemini/tmp", "home/.qwen/projects", "home/.ssh", "home/Library/LaunchAgents", "home/.config/git", "state", "work"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::write(d.join("home/.ssh/id"), "secret").unwrap();
        d.canonicalize().unwrap()
    }

    fn places(d: &Path) -> Places {
        let home = d.join("home");
        Places {
            root: d.join("state"),
            tmp: d.join("tmp"),
            exe: d.join("nerd"),
            // session-env and Codex's databases stand for a manifest that still lists what the profile denies after the data allows.
            data: vec![home.join(".claude/projects"), home.join(".copilot/session-store.db*"), home.join(".codex/*.sqlite*"), home.join(".claude/tasks"), home.join(".claude/session-env"), home.join(".gemini/tmp"), home.join(".qwen/projects")],
            roots: vec![home.join(".claude.json"), home.join(".claude"), home.join(".codex.json"), home.join(".codex")],
            hook_files: vec![home.join(".claude/settings.json")],
            hook_dirs: vec![home.join(".claude/hooks")],
            run: d.join("run"),
            agent: Some(d.join("agent/s.sock")),
            home,
        }
    }

    /// `script` under `profile` through sandbox-exec, its exit code; None when Seatbelt cannot be applied from here.
    fn sandboxed(profile: &str, script: &str) -> Option<i32> {
        let out = std::process::Command::new("/usr/bin/sandbox-exec").args(["-p", profile, "/bin/sh", "-c", script]).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("sandbox_apply") {
            println!("skipped: this test process already runs under Seatbelt: {err}");
            return None;
        }
        Some(out.status.code().unwrap_or(-1))
    }

    #[test]
    fn the_profile_text_for_each_combination() {
        let d = Path::new("/Users/u/x");
        let p = Places {
            home: "/Users/u".into(),
            root: "/Users/u/Library/Application Support/nerd".into(),
            tmp: "/private/var/folders/ab/T".into(),
            exe: "/Users/u/.local/bin/nerd".into(),
            data: vec!["/Users/u/.claude/projects".into(), "/Users/u/.codex/*.sqlite*".into()],
            roots: vec!["/Users/u/.claude.json".into(), "/Users/u/.claude".into()],
            hook_files: vec!["/Users/u/.claude/settings.json".into()],
            hook_dirs: vec!["/Users/u/.claude/hooks".into()],
            run: "/private/var/run".into(),
            agent: Some("/Users/u/.1p/agent.sock".into()),
        };
        let m = Path::new("/Users/u/Library/Application Support/nerd/nerd.msock");
        let t = |f: Files, n: Network, port| text(&p, &f, &n, d, m, port).unwrap();
        let folder = "(deny file-write*)\n(allow file-write* (subpath \"/Users/u/x\") (subpath \"/private/var/folders/ab/T\") (subpath \"/private/tmp\") (subpath \"/private/var/folders\") (subpath \"/Users/u/Library/Caches\") (regex #\"^/dev/\"))\n\
             (deny file-write* (subpath \"/Users/u/.claude.json\") (subpath \"/Users/u/.claude\"))\n\
             (allow file-write* (subpath \"/Users/u/.claude/projects\") (regex #\"^/Users/u/\\.codex/[^/]*\\.sqlite[^/]*$\"))\n\
             (deny file-write* (subpath \"/Users/u/Library/Application Support/nerd\") (subpath \"/Users/u/.config/git\") (subpath \"/Users/u/.config/fish\") (subpath \"/Users/u/Library/LaunchAgents\") (subpath \"/Users/u/.ssh\") (subpath \"/Users/u/.aws\") (subpath \"/Users/u/.gnupg\") (subpath \"/Users/u/.docker\") (subpath \"/Users/u/.kube\") (subpath \"/Users/u/.azure\") (subpath \"/Users/u/.config/gh\") (subpath \"/Users/u/.config/gcloud\") (subpath \"/Users/u/.cargo/bin\") (subpath \"/Users/u/.local/bin\") \
             (subpath \"/Users/u/.vim\") (subpath \"/Users/u/.config/nvim\") (subpath \"/Users/u/.config/tmux\") (subpath \"/Users/u/.emacs.d\") (subpath \"/Users/u/Library/Preferences\") (subpath \"/Users/u/.pyenv/shims\") (subpath \"/Users/u/.rbenv/shims\") (subpath \"/Users/u/.nvm\") (subpath \"/Users/u/.bun/bin\") (subpath \"/Users/u/go/bin\") (subpath \"/Users/u/.volta/bin\") (subpath \"/Users/u/.deno/bin\") (subpath \"/Users/u/.asdf/shims\") (subpath \"/Users/u/.local/share/nvim\") \
             (subpath \"/Users/u/Library/Caches/pnpm\") (subpath \"/Users/u/Library/Caches/uv\") (subpath \"/Users/u/Library/Caches/deno\") (subpath \"/Users/u/Library/Caches/go-build\") (subpath \"/Users/u/Library/Caches/ms-playwright\") (subpath \"/Users/u/Library/Caches/pypoetry\") (subpath \"/Users/u/Library/Caches/Yarn\") (subpath \"/Users/u/Library/Caches/node-gyp\") \
             (subpath \"/Users/u/Library/Caches/pip\") (subpath \"/Users/u/Library/Caches/Cypress\") (subpath \"/Users/u/Library/Caches/org.swift.swiftpm\") (subpath \"/Users/u/Library/Caches/copilot\") (subpath \"/Users/u/go/pkg/mod\") \
             (subpath \"/Users/u/.claude/shell-snapshots\") (subpath \"/Users/u/.claude/session-env\") (subpath \"/Users/u/.codex/shell_snapshots\") (subpath \"/Users/u/.codex/tmp\") (subpath \"/Users/u/.codex/.tmp\") (subpath \"/Users/u/.gemini/tmp/bin\") (subpath \"/Users/u/.gemini/history\") (subpath \"/Users/u/.local/share/opencode/snapshot\") (subpath \"/Users/u/.claude/tasks\") (subpath \"/Users/u/.claude/hooks\") \
             (literal \"/Users/u/.local/bin/nerd\") (literal \"/Users/u/.zshrc\") (literal \"/Users/u/.zprofile\") (literal \"/Users/u/.zshenv\") (literal \"/Users/u/.zlogin\") (literal \"/Users/u/.zlogout\") (literal \"/Users/u/.bashrc\") (literal \"/Users/u/.bash_profile\") (literal \"/Users/u/.bash_login\") (literal \"/Users/u/.bash_logout\") (literal \"/Users/u/.profile\") (literal \"/Users/u/.gitconfig\") (literal \"/Users/u/.netrc\") (literal \"/Users/u/.npmrc\") (literal \"/Users/u/.cargo/config.toml\") (literal \"/Users/u/.vimrc\") (literal \"/Users/u/.tmux.conf\") (literal \"/Users/u/.emacs\") (literal \"/Users/u/.claude/settings.json\") \
             (regex #\"^/private/tmp/nerd-[0-9]+(/|$)\") (regex #\"^/private/tmp/cc-socks\") (regex #\"^/private/var/folders/ab/T/cc-socks\") (regex #\"^/Users/u/\\.[^/]+/projects/[^/]+/memory(/|$)\") (regex #\"^/Users/u/\\.codex/[^/]*\\.sqlite\") (regex #\"^/Users/u/\\.qwen/projects/[^/]+/workflows(/|$)\"))\n\
             (deny file-read-data (subpath \"/Users/u/.ssh\") (subpath \"/Users/u/.aws\") (subpath \"/Users/u/.gnupg\") (subpath \"/Users/u/.netrc\") (subpath \"/Users/u/.docker\") (subpath \"/Users/u/.kube\") (subpath \"/Users/u/.azure\") (subpath \"/Users/u/.config/gh\") (subpath \"/Users/u/.config/gcloud\") (subpath \"/Users/u/Library/Mail\") (subpath \"/Users/u/Library/Messages\") (subpath \"/Users/u/Library/Cookies\") (subpath \"/Users/u/Library/Application Support/nerd\"))\n";
        let head = "(version 1)\n(allow default)\n";
        let msock = "(allow network-outbound (literal \"/Users/u/Library/Application Support/nerd/nerd.msock\"))\n";
        let closed = |local: &str| format!("(deny network*)\n(allow network-bind network-inbound (local unix-socket))\n{msock}(allow network-outbound (remote ip \"localhost:{local}\"))\n");
        assert_eq!(
            t(Files::Folder, Network::Open, None),
            format!(
                "{head}{folder}(deny network-outbound (regex #\"^/private/tmp/cc-socks\") (regex #\"^/private/var/folders/ab/T/cc-socks\") (regex #\"^/private/var/run/com\\.apple\\.launchd\\.[^/]+/Listeners$\") \
                 (literal \"/private/var/run/docker.sock\") (literal \"/Users/u/.1p/agent.sock\") (subpath \"/Users/u/Library/Application Support/nerd\") (subpath \"/Users/u/.codex\") (subpath \"/private/var/folders/ab/T/podman\") \
                 (subpath \"/Users/u/.ssh\") (subpath \"/Users/u/.docker\") (subpath \"/Users/u/Library/Containers/com.docker.docker\") (subpath \"/Users/u/.colima\") (subpath \"/Users/u/.orbstack\") (subpath \"/Users/u/.rd\") (subpath \"/Users/u/.lima\") \
                 (subpath \"/Users/u/Library/Application Support/rancher-desktop\") (subpath \"/Users/u/.local/share/containers/podman\") (subpath \"/Users/u/Library/Group Containers/2BUA8C4S2C.com.1password/t\") \
                 (subpath \"/Users/u/Library/Containers/com.maxgoedjen.Secretive.SecretAgent\"))\n{msock}"
            )
        );
        assert_eq!(t(Files::Folder, Network::None, None), format!("{head}{folder}{}", closed("*")));
        let dots = "(deny file-write* (regex #\"^/Users/u/\\.[^/]\") (subpath \"/Users/u/Library\") (subpath \"/Users/u/bin\"))\n\
             (allow file-write* (subpath \"/private/var/folders/ab/T\") (subpath \"/private/tmp\") (subpath \"/private/var/folders\") (subpath \"/Users/u/Library/Caches\"))\n";
        let at_home = folder.replacen("(subpath \"/Users/u/x\")", "(subpath \"/Users/u\")", 1).replacen("(regex #\"^/dev/\"))\n", &format!("(regex #\"^/dev/\"))\n{dots}"), 1);
        assert_eq!(text(&p, &Files::Folder, &Network::None, Path::new("/Users/u"), m, None).unwrap(), format!("{head}{at_home}{}", closed("*")), "the folder is HOME");
        assert!(text(&p, &Files::Folder, &Network::None, Path::new("/"), m, None).unwrap().contains(dots), "the folder holds HOME");
        assert_eq!(t(Files::Folder, Network::Providers(vec!["claude".into()]), Some(4100)), format!("{head}{folder}{}", closed("4100")));
        assert_eq!(t(Files::Everything, Network::None, None), format!("{head}{}", closed("*")));
        assert_eq!(t(Files::Everything, Network::Providers(vec!["claude".into()]), Some(4100)), format!("{head}{}", closed("4100")));
        assert_eq!(t(Files::Account("scout".into()), Network::None, None), format!("{head}{}", closed("*")), "the account confines its files");
        assert!(text(&p, &Files::Folder, &Network::Providers(vec!["claude".into()]), d, m, None).is_err(), "no proxy port, no profile");
        assert!(text(&p, &Files::Folder, &Network::None, Path::new("/a\"b"), m, None).is_err(), "a path that cannot be quoted");
    }

    #[test]
    fn a_split_inherits_the_isolation_of_its_pane() {
        use crate::app::{App, Pane, Space, Tab};
        use crate::layout::Layout;
        let mut app = App::new(crate::config::Config::default());
        let (pty, _) = crate::pty::Pty::fake(1, std::fs::File::create("/dev/null").unwrap());
        let folder = Isolation { files: Files::Folder, network: Network::None };
        let mut pane = Pane::new(1, crate::term::Term::new(80, 24, 100), pty, "/".into(), vec!["/bin/zsh".into()]);
        pane.isolation = folder.clone();
        let tab_default = Isolation { files: Files::Everything, network: Network::None };
        let space_default = Isolation { files: Files::Folder, network: Network::Open };
        app.spaces.push(Space {
            name: None,
            cwd: "/".into(),
            tabs: vec![
                Tab { name: None, layout: Layout::new(1), panes: [(1, pane)].into(), isolation_new: Some(tab_default.clone()) },
                Tab { name: None, layout: Layout::new(9), panes: Default::default(), isolation_new: None },
            ],
            active_tab: 0,
            isolation_new: Some(space_default.clone()),
        });
        assert_eq!(app.isolation_for_new(0, Some(0), Some(1)), folder, "the pane split from");
        assert_eq!(app.isolation_for_new(0, Some(0), Some(99)), tab_default, "a pane that is gone: the tab's");
        assert_eq!(app.isolation_for_new(0, Some(1), None), space_default, "a tab with none: the space's");
        assert_eq!(app.isolation_for_new(0, None, None), space_default);
        assert_eq!(app.isolation_for_new(5, None, None), Isolation::default(), "no such space: open");
    }

    #[test]
    fn a_pane_started_again_goes_through_wrap_with_its_isolation() {
        use crate::app::{App, Pane, Space, Tab};
        crate::session::tests::with_temp_state_dir("respawn", || {
            let mut app = App::new(crate::config::Config::default());
            let (pty, _) = crate::pty::Pty::fake(1, std::fs::File::create("/dev/null").unwrap());
            let mut pane = Pane::new(1, crate::term::Term::new(80, 24, 100), pty, "/".into(), vec!["/bin/zsh".into()]);
            pane.isolation = Isolation { files: Files::Folder, network: Network::Providers(vec!["no-such-agent".into()]) };
            let tab = Tab { name: None, layout: crate::layout::Layout::new(1), panes: [(1, pane)].into(), isolation_new: None };
            app.spaces.push(Space { name: None, cwd: "/".into(), tabs: vec![tab], active_tab: 0, isolation_new: None });
            assert!(!app.restart_pane(1), "its shell is refused, never started unwrapped");
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert!(log.contains("the shell would not start again: no-such-agent names no hosts"), "{log}");
        });
    }

    #[test]
    fn providers_naming_different_ports_cannot_share_a_pane() {
        use crate::agents::Network::Internet;
        let net = |h: &str, p: &[u16]| Internet { hosts: vec![h.into()], ports: p.to_vec() };
        let got = merge([("a", net("x.com", &[443])), ("b", net("y.com", &[443])), ("c", net("x.com", &[443]))].into_iter()).unwrap();
        assert_eq!((got.hosts, got.ports), (vec!["x.com".to_string(), "y.com".into()], vec![443]));
        let e = merge([("a", net("x.com", &[443])), ("local", net("127.0.0.1", &[11434]))].into_iter()).unwrap_err();
        assert_eq!(e, "a and local name different ports; choose them in separate panes");
        assert_eq!(merge([("a", net("x.com", &[443])), ("b", crate::agents::Network::None)].into_iter()).unwrap_err(), "b names no hosts");
        assert_eq!(merge(std::iter::empty()).unwrap_err(), "no provider is chosen");
    }

    #[test]
    fn an_open_pane_is_not_wrapped() {
        let argv = vec!["/bin/zsh".to_string(), "-l".into()];
        let mut env = vec![("NERD_STATE".to_string(), "/s".to_string())];
        assert_eq!(wrap(3, &argv, Path::new("/w"), &Isolation::default(), &mut env).unwrap(), argv);
        assert_eq!(env, [("NERD_STATE".to_string(), "/s".to_string())], "an open pane keeps its environment");
    }

    #[test]
    fn a_wrapped_pane_has_no_nerd_state() {
        let argv = vec!["/bin/zsh".to_string(), "-l".into()];
        let mut env = vec![("NERD_STATE".to_string(), "/s".to_string()), ("NERD_MSOCK".into(), "/m.sock".into()), ("NERD_MSOCK_UID".into(), "501".into())];
        let got = wrap(3, &argv, Path::new("/w"), &Isolation { files: Files::Folder, network: Network::None }, &mut env).unwrap();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let want: Vec<String> = [&exe, "--pane-exec", "--pane", "3", "--network", "none", "--files", "folder", "--cwd", "/w", "--msock", "/m.sock", "--msock-uid", "501", "--bin", &exe].iter().map(|s| s.to_string()).collect();
        assert_eq!(got[..want.len()], want[..], "{got:?}");
        assert_eq!(got[got.len() - 3..], ["--", "/bin/zsh", "-l"], "{got:?}");
        assert!(!env.iter().any(|(k, _)| k == "NERD_STATE"), "{env:?}");
        assert!(env.contains(&("NERD_BIN".into(), exe)), "{env:?}");
        let mut env = Vec::new();
        let e = wrap(3, &argv, Path::new("/w"), &Isolation { files: Files::Everything, network: Network::Providers(vec!["claude".into(), "no-such-agent".into()]) }, &mut env).unwrap_err();
        assert_eq!(e.to_string(), "no-such-agent names no hosts");
        let e = wrap(3, &argv, Path::new("/w"), &Isolation { files: Files::Everything, network: Network::Providers(Vec::new()) }, &mut env).unwrap_err();
        assert_eq!(e.to_string(), "no provider is chosen");
        let got = wrap(3, &argv, Path::new("/w"), &Isolation { files: Files::Everything, network: Network::Providers(vec!["claude".into(), "pi".into(), "gemini".into()]) }, &mut env).unwrap();
        let hosts = got.windows(2).find(|w| w[0] == "--hosts").map(|w| w[1].clone()).unwrap_or_default();
        let net = crate::proxy::Net::parse(&hosts).unwrap();
        assert!(["api.anthropic.com", "accounts.google.com", "api.openai.com"].iter().all(|h| net.hosts.iter().filter(|n| n == h).count() == 1), "every chosen agent's hosts, each once: {hosts}");
        assert_eq!(net.ports, [443], "{hosts}");
        let e = wrap(3, &argv, Path::new("/w"), &Isolation { files: Files::Account("nobody-here".into()), network: Network::Open }, &mut env).unwrap_err();
        assert_eq!(e.to_string(), "nobody-here is not an admitted account");
    }

    #[test]
    fn a_shared_dir_owned_by_another_or_not_0711_or_a_link_is_refused() {
        let base = PathBuf::from(format!("/tmp/nt-s5-{}-door", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let me = crate::policy::owner_uid();
        let dir = base.join("nerd-x");
        let (_l, sock) = open_door(&dir, me, "main.msock").unwrap();
        let m = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!((m.mode() & 0o7777, m.uid()), (0o711, me), "made 0711 and ours");
        assert_eq!(std::fs::symlink_metadata(&sock).unwrap().mode() & 0o777, 0o666, "every account may connect");
        let kept = base.join("kept");
        std::fs::write(&kept, "x").unwrap();
        std::os::unix::fs::symlink(&kept, dir.join("nerd")).unwrap();
        assert!(open_door(&dir, me, "main.msock").is_ok(), "nerd's own is taken again");
        assert!(std::fs::symlink_metadata(dir.join("nerd")).is_err() && kept.exists(), "a stale nerd link goes, what it points at stays");
        std::fs::write(dir.join("nerd"), "an earlier build").unwrap();
        assert!(open_door(&dir, me, "main.msock").is_ok());
        assert!(std::fs::symlink_metadata(dir.join("nerd")).is_err(), "a stale nerd copy goes");
        let e = open_door(&dir, me + 1, "main.msock").unwrap_err();
        assert!(e.to_string().ends_with(&format!("is not nerd's own (owner {me}, mode 711)")), "{e}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(open_door(&dir, me, "main.msock").unwrap_err().to_string().contains("mode 755"));
        let link = base.join("nerd-link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o711)).unwrap();
        assert!(verify_dir(&dir, me).is_ok());
        assert!(verify_dir(&link, me).is_err(), "a link to nerd's own is still a link");
        assert_eq!(shared_dir_of(Path::new("/tmp/nerd-501/main.msock")), Some((Path::new("/tmp/nerd-501"), "501")));
        assert_eq!(shared_dir_of(Path::new("/Users/u/nerd.msock")), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_account_pane_starts_only_through_the_shared_door_of_a_server_at_the_default_root() {
        let _lock = crate::policy::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::session::tests::with_temp_state_dir("account", || {
            let before = crate::policy::current();
            crate::policy::replace(crate::policy::Policy { accounts: vec![crate::policy::Account { name: "scout".into(), uid: 502 }], ..Default::default() });
            let mut env = Vec::new();
            let e = wrap(3, &["/bin/zsh".into()], Path::new("/w"), &Isolation { files: Files::Account("scout".into()), network: Network::None }, &mut env).unwrap_err();
            crate::policy::replace((*before).clone());
            assert_eq!(e.to_string(), "other accounts need a server at the default state root");
            assert!(env.is_empty(), "nothing is started: {env:?}");
        });
    }

    #[test]
    fn an_account_pane_runs_only_a_copy_only_root_can_change_that_is_this_nerd() {
        let base = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).join(format!("nt-s5-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("local/libexec");
        std::fs::create_dir_all(&dir).unwrap();
        let (exe, at) = (base.join("exe"), dir.join("nerd"));
        std::fs::write(&exe, vec![7u8; 200_000]).unwrap();
        let me = crate::policy::owner_uid();
        let check = |owner| verify_root_copy(&at, &exe, owner, &base).map_err(|e| e.to_string());
        assert!(check(me).unwrap_err().ends_with("libexec/nerd is missing"));
        std::fs::write(&at, vec![7u8; 200_000]).unwrap();
        for (path, mode) in [(&base, 0o755), (&base.join("local"), 0o755), (&dir, 0o755), (&at, 0o755)] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(check(me), Ok(()), "a matching copy, owned as asked (root in use; this user here)");
        assert!(check(0).unwrap_err().ends_with("libexec is not a folder only root can change"), "owned by this user, not root");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(check(me).unwrap_err().ends_with("is not a folder only root can change"), "a folder the group may write");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(base.join("local"), std::fs::Permissions::from_mode(0o757)).unwrap();
        assert!(check(me).unwrap_err().ends_with("/local is not a folder only root can change"), "a parent others may write: the folder could be renamed away");
        std::fs::set_permissions(base.join("local"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o757)).unwrap();
        assert!(check(me).unwrap_err().ends_with("nerd is not a file only root can change"), "a file others may write");
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut other = vec![7u8; 200_000];
        other[150_000] = 8;
        std::fs::write(&at, &other).unwrap();
        assert!(check(me).unwrap_err().ends_with("nerd is not the nerd that runs now"), "same length, one byte differs");
        std::fs::write(&at, vec![7u8; 10]).unwrap();
        assert!(check(me).unwrap_err().ends_with("is not the nerd that runs now"), "another length");
        std::fs::remove_file(&at).unwrap();
        std::os::unix::fs::symlink(&exe, &at).unwrap();
        assert!(check(me).unwrap_err().ends_with("is not a file only root can change"), "a link to this nerd is still a link");
        assert_eq!(verify_root_copy(Path::new("/usr/bin/true"), Path::new("/usr/bin/true"), 0, Path::new("/")).map_err(|e| e.to_string()), Ok(()), "root's own file, every folder up to / root's");
        assert!(verify_root_copy(&at, &exe, 0, Path::new("/")).is_err(), "a file under this user's folders");
        assert_eq!(
            install_commands(Path::new("/Users/u/.local/bin/nerd")),
            ["sudo install -d -o root -g wheel -m 755 \"/Library/Application Support/nerd/bin\"", "sudo install -o root -g wheel -m 755 /Users/u/.local/bin/nerd \"/Library/Application Support/nerd/bin/nerd\""]
        );
        for dir in Path::new(ROOT_COPY).ancestors().skip(3) {
            let m = std::fs::symlink_metadata(dir).unwrap();
            assert!(m.uid() == 0 && m.mode() & 0o022 == 0, "{} is root's and no one else may write it on every Mac", dir.display());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn folder_only_cannot_write_the_state_root_config_a_hook_file_or_zshrc() {
        let d = scratch("write");
        let profile = text(&places(&d), &Files::Folder, &Network::Open, &d.join("work"), Path::new("/tmp/none.msock"), None).unwrap();
        let try_ = |path: &str| sandboxed(&profile, &format!("echo x > '{}/{path}'", d.display()));
        let Some(ok) = try_("work/ok") else { return };
        assert_eq!(ok, 0, "its own folder");
        assert_eq!(try_("home/.claude/projects/session"), Some(0), "an agent's session folder");
        assert_ne!(try_("home/.claude/session"), Some(0), "the rest of its own folder");
        for no in ["state/config.json", "home/.claude/settings.json", "home/.claude/hooks/x", "home/.zshrc", "home/x"] {
            assert_ne!(try_(no), Some(0), "{no}");
            assert!(!d.join(no).exists(), "{no}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_only_keeps_agents_settings_and_what_runs_later_even_when_the_folder_is_home() {
        let d = scratch("roots");
        let home = d.join("home");
        let door = PathBuf::from(format!("/tmp/nerd-{}", 4_000_000_000u32 + std::process::id() % 1000));
        for cwd in [d.join("work"), home.clone()] {
            let profile = text(&places(&d), &Files::Folder, &Network::None, &cwd, Path::new("/tmp/none.msock"), None).unwrap();
            let write = |path: &Path| sandboxed(&profile, &format!("mkdir -p \"$(dirname '{0}')\" 2>/dev/null; echo x > '{0}'", path.display()));
            let Some(rc) = write(&home.join(".claude/projects/session.jsonl")) else { return };
            assert_eq!(rc, 0, "a session folder, cwd {cwd:?}");
            assert_eq!(write(&home.join(".claude/projects/p/session.jsonl")), Some(0), "a project's sessions, beside its memory, cwd {cwd:?}");
            assert_eq!(write(&home.join("Library/Caches/other/x")), Some(0), "caches that hold no code, cwd {cwd:?}");
            assert_eq!(write(&home.join(".gemini/tmp/p/chats/x.json")), Some(0), "Gemini's chats beside its tools, cwd {cwd:?}");
            assert_eq!(write(&home.join(".qwen/projects/p/chats/x.jsonl")), Some(0), "Qwen's chats beside its memory, cwd {cwd:?}");
            assert_eq!(write(&home.join(".copilot/session-store.db-wal")), Some(0), "a data name with a *, cwd {cwd:?}");
            for no in [
                ".claude.json",
                ".claude/CLAUDE.md",
                ".claude/skills/x/SKILL.md",
                ".claude/settings.local.json",
                ".codex/config.toml",
                ".gitconfig",
                ".zlogin",
                ".config/git/config",
                "Library/LaunchAgents/x.plist",
                ".zshrc",
                ".ssh/authorized_keys",
                ".ssh/config",
                ".aws/config",
                ".docker/config.json",
                ".config/gh/hosts.yml",
                ".netrc",
                ".npmrc",
                ".cargo/config.toml",
                ".cargo/bin/git",
                ".local/bin/claude",
                ".vimrc",
                ".vim/plugin/x.vim",
                ".config/nvim/init.lua",
                ".tmux.conf",
                ".config/tmux/tmux.conf",
                ".emacs",
                ".emacs.d/init.el",
                "Library/Preferences/com.example.plist",
                ".pyenv/shims/python",
                ".rbenv/shims/ruby",
                ".nvm/nvm.sh",
                ".bun/bin/bun",
                "go/bin/gopls",
                ".volta/bin/node",
                ".deno/bin/deno",
                ".asdf/shims/node",
                ".local/share/nvim/lazy/x/plugin/x.lua",
                "Library/Caches/pnpm/store/x",
                "Library/Caches/uv/x/bin/python",
                "Library/Caches/deno/npm/x.js",
                "Library/Caches/go-build/ab/x",
                "Library/Caches/ms-playwright/chromium/chrome",
                "Library/Caches/pypoetry/virtualenvs/x/bin/python",
                "Library/Caches/Yarn/v6/x/index.js",
                "Library/Caches/node-gyp/x/include/node.h",
                "Library/Caches/pip/wheels/x.whl",
                "Library/Caches/Cypress/x/Cypress.app/y",
                "Library/Caches/org.swift.swiftpm/repositories/x/HEAD",
                "Library/Caches/copilot/pkg/x/index.js",
                ".claude/projects/p/memory/MEMORY.md",
                ".claude/projects/p/memory",
                "Library/Caches/PNPM/store/x",
                ".claude/projects/p/MEMORY/x.md",
                ".qwen/projects/p/memory/MEMORY.md",
                ".qwen/projects/p/workflows/generated/inline/x.js",
                ".qwen/projects/p/workflows",
                ".claude/shell-snapshots/snapshot-zsh-1-x.sh",
                ".claude/session-env/s/hook-0.sh",
                ".codex/shell_snapshots/x.sh",
                ".codex/tmp/arg0/codex-arg0x/git",
                ".codex/.tmp/plugins/.git/config",
                ".gemini/tmp/bin/rg",
                ".gemini/history/p/.git/config",
                ".local/share/opencode/snapshot/p/h/config",
                ".codex/queue_1.sqlite",
                ".codex/queue_1.sqlite-wal",
                ".codex/memories_1.sqlite-shm",
                ".codex/goals_1.sqlite",
                ".codex/state_5.sqlite",
                ".claude/tasks/team/1.json",
            ] {
                assert_ne!(write(&home.join(no)), Some(0), "{no}, cwd {cwd:?}");
                assert!(!home.join(no).exists(), "{no}, cwd {cwd:?}");
            }
            assert_ne!(write(&door.join("nerd")), Some(0), "the door other accounts exec nerd from, cwd {cwd:?}");
            assert_ne!(write(Path::new("/tmp/cc-socks-nt-s5/x")), Some(0), "Claude Code's sockets, cwd {cwd:?}");
            assert_eq!(write(&std::env::temp_dir().join(format!("nt-s5-{}", std::process::id()))), Some(0), "the temp folder stays writable");
            if cwd == home {
                assert_eq!(write(&home.join("notes.txt")), Some(0), "the rest of a home folder chosen as the pane's");
                assert_eq!(write(&home.join("Documents/x.txt")), Some(0), "a folder of the owner's files");
                for no in [
                    ".oh-my-zsh/custom/x.zsh",
                    ".p10k.zsh",
                    ".rustup/toolchains/x/bin/cargo",
                    ".pyenv/versions/x/bin/python",
                    ".gradle/init.d/x.gradle",
                    ".ipython/profile_default/startup/x.py",
                    ".envrc",
                    ".config/starship.toml",
                    "Library/Application Support/x/y",
                    "Library/Preferences/x.plist",
                    "Library/Scripts/x.scpt",
                    "Library/Services/x.workflow/y",
                    "bin/x",
                    "go/pkg/mod/example.com/x@v1/x.go",
                ] {
                    assert_ne!(write(&home.join(no)), Some(0), "{no} in a home folder chosen as the pane's");
                    assert!(!home.join(no).exists(), "{no}");
                }
            }
        }
        assert!(!door.exists());
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("nt-s5-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_only_cannot_read_ssh() {
        let d = scratch("read");
        let profile = text(&places(&d), &Files::Folder, &Network::None, &d.join("work"), Path::new("/tmp/none.msock"), None).unwrap();
        let Some(rc) = sandboxed(&profile, &format!("cat '{}/home/.ssh/id'", d.display())) else { return };
        assert_ne!(rc, 0);
        assert_eq!(sandboxed(&profile, &format!("ls '{}/home' >/dev/null", d.display())), Some(0), "the home folder itself is listed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unlisted_unix_socket_is_refused_and_the_msock_is_reached() {
        let base = PathBuf::from(format!("/tmp/nt-s5-{}-sock", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (own, msock, daemon) = (base.join("s.sock"), base.join("m.sock"), base.join("home/.codex/app-server-daemon/d.sock"));
        // launchd's ssh agent, the pane's own agent elsewhere, Docker's sockets, colima's.
        let owners = ["run/com.apple.launchd.x/Listeners", "agent/s.sock", "run/docker.sock", "home/.docker/run/docker.sock", "home/.colima/default/docker.sock"].map(|p| base.join(p));
        let other = base.join("run/com.apple.launchd.x/Other");
        let _l: Vec<_> = [&own, &msock, &daemon, &other].into_iter().chain(&owners).map(|p| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::os::unix::net::UnixListener::bind(p).unwrap()
        }).collect();
        for (files, network) in [(Files::Everything, Network::None), (Files::Folder, Network::Providers(Vec::new())), (Files::Folder, Network::Open)] {
            let profile = text(&places(&base), &files, &network, &base.join("work"), &msock, Some(9)).unwrap();
            let connect = |p: &Path| sandboxed(&profile, &format!("/usr/bin/perl -MIO::Socket::UNIX -e 'IO::Socket::UNIX->new(Peer => \"{}\") or exit 1'", p.display()));
            let Some(rc) = connect(&msock) else { return };
            assert_eq!(rc, 0, "the msock: {files:?} {network:?}");
            if network != Network::Open {
                assert_ne!(connect(&own), Some(0), "nerd.sock: {files:?} {network:?}");
            }
            assert_ne!(connect(&daemon), Some(0), "Codex's daemon: {files:?} {network:?}");
            for p in &owners {
                assert_ne!(connect(p), Some(0), "{p:?}: {files:?} {network:?}");
            }
            if network == Network::Open {
                assert_eq!(connect(&other), Some(0), "another socket beside launchd's agent: {files:?} {network:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn none_reaches_no_tcp() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let d = scratch("none");
        let profile = text(&places(&d), &Files::Everything, &Network::None, &d.join("work"), Path::new("/tmp/none.msock"), None).unwrap();
        let Some(rc) = sandboxed(&profile, "/usr/bin/nc -z -G 3 192.0.2.1 80") else { return };
        assert_ne!(rc, 0, "the internet");
        assert_ne!(sandboxed(&profile, "/usr/bin/nc -z -G 3 example.com 443"), Some(0), "a name");
        assert_eq!(sandboxed(&profile, &format!("/usr/bin/nc -z 127.0.0.1 {port}")), Some(0), "this Mac's own services");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn provider_reaches_only_the_proxy_port() {
        let (a, b) = (std::net::TcpListener::bind("127.0.0.1:0").unwrap(), std::net::TcpListener::bind("127.0.0.1:0").unwrap());
        let (proxy, other) = (a.local_addr().unwrap().port(), b.local_addr().unwrap().port());
        let d = scratch("provider");
        let profile = text(&places(&d), &Files::Folder, &Network::Providers(vec!["claude".into()]), &d.join("work"), Path::new("/tmp/none.msock"), Some(proxy)).unwrap();
        let Some(rc) = sandboxed(&profile, &format!("/usr/bin/nc -z 127.0.0.1 {proxy}")) else { return };
        assert_eq!(rc, 0, "the proxy");
        assert_ne!(sandboxed(&profile, &format!("/usr/bin/nc -z 127.0.0.1 {other}")), Some(0), "another port on this Mac");
        assert_ne!(sandboxed(&profile, "/usr/bin/nc -z -G 3 192.0.2.1 80"), Some(0), "the internet");
        let _ = std::fs::remove_dir_all(&d);
    }
}
