//! Every server on this Mac, from its folder and its socket: what `--list` prints and Servers… shows.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::proto::{ClientMsg, ProbeAnswer};
use crate::session::{instance_label, instances, instances_dir};

/// How long one server may take to answer `Probe`.
pub const PROBE_LIMIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    /// Empty for the legacy server in the state root.
    pub id: String,
    pub label: String,
    pub running: bool,
    /// A session shows it; `None` when it runs and did not answer `Probe` (an older build), or is the caller itself.
    pub shown: Option<bool>,
    /// 0 or 1: a server shows one session at a time.
    pub sessions: u8,
    pub folders: Vec<PathBuf>,
    pub last_used: Option<SystemTime>,
    pub sandbox: String,
    pub version: Option<String>,
    /// Nothing was ever done in it; the file is the server's to write and to remove.
    pub untouched: bool,
    /// A folder a client reserved for a server that never started: no snapshot, no daemon.
    pub reserved_only: bool,
}

/// A `nerd --daemon` of this user that no listed server's socket leads to.
#[derive(Debug, Clone, PartialEq)]
pub struct Stray {
    pub pid: i32,
    pub started: Option<SystemTime>,
    pub exe: PathBuf,
    /// Where it lives instead, as far as its environment says.
    pub note: String,
    /// Started under another HOME: some other home's server, never stopped from here.
    pub other_home: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Probe {
    NotRunning,
    /// Running, and no session shows it.
    Free,
    Shown,
    /// Running, but it did not answer: an older build closes on `Probe`.
    Unknown,
}

fn dir_of(id: &str) -> PathBuf {
    if id.is_empty() { crate::paths::state_root() } else { instances_dir().join(id) }
}

/// The socket of server `id` itself, never `socket_path()`, which `NERD_SOCKET` points elsewhere.
pub fn socket_of(id: &str) -> PathBuf {
    dir_of(id).join("nerd.sock")
}

/// Written by the server when a session of this Mac attaches; never on a start or an update.
pub fn mark_attached(id: &str) -> std::io::Result<()> {
    let dir = instances_dir().join(id);
    // Under `NERD_STATE_DIR` a server has no folder of its own here, and none is made for it.
    if !crate::session::valid_id(id) || !dir.is_dir() {
        return Ok(());
    }
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    crate::paths::write_private(&dir.join("attached"), "tmp", format!("{now}\n").as_bytes())
}

/// `instances/N/untouched` of a server with a folder of its own; never a path for a bad or empty id.
fn untouched_path(id: &str) -> Option<PathBuf> {
    let dir = instances_dir().join(id);
    (crate::session::valid_id(id) && dir.is_dir()).then(|| dir.join("untouched"))
}

/// Written by every way of making a new server, right after its folder is reserved.
pub fn mark_untouched(id: &str) -> std::io::Result<()> {
    match untouched_path(id) {
        Some(p) => crate::paths::write_private(&p, "tmp", b""),
        None => Ok(()),
    }
}

pub fn untouched(id: &str) -> bool {
    untouched_path(id).is_some_and(|p| p.exists())
}

/// The first thing done in server `id`: it is kept from now on, and the mark never comes back.
pub fn clear_untouched(id: &str) {
    if let Some(p) = untouched_path(id) {
        let _ = std::fs::remove_file(p);
    }
}

/// More than one space, tab or pane, a name, isolation, an agent: something was done in the server.
/// A `cd`, a title, a size or closing its only pane is not; they change the snapshot at every prompt.
pub fn touched(s: &crate::session::WorkspaceSnapshot, agent_seen: bool) -> bool {
    let tabs = || s.spaces.iter().flat_map(|sp| &sp.tabs);
    let panes = || tabs().flat_map(|t| &t.panes);
    agent_seen
        || s.spaces.len() > 1
        || tabs().count() > 1
        || panes().count() > 1
        || s.spaces.iter().any(|sp| sp.name.is_some() || sp.isolation_new.is_some())
        || tabs().any(|t| t.name.is_some() || t.isolation_new.is_some())
        || panes().any(|p| p.label.is_some() || p.agent_session.is_some() || !p.isolation.is_open())
}

/// When a session last attached, else when a snapshot with a space in it was saved: an empty one
/// is what a server started with no window writes (the LaunchAgent's), and that is no use.
pub fn last_used(id: &str) -> Option<SystemTime> {
    used(id).map(|(_, t)| t)
}

/// `last_used`, stamped ones first: every stamp is newer than any attach an older build did not stamp.
pub(crate) fn used(id: &str) -> Option<(bool, SystemTime)> {
    let dir = dir_of(id);
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let Some(t) = mtime(&dir.join("attached")) {
        return Some((true, t));
    }
    let snap = dir.join("session.json");
    crate::session::read_snapshot(&snap).filter(|s| !s.spaces.is_empty())?;
    Some((false, mtime(&snap)?))
}

/// Asks the server at `path`; also the pid that listens there, when something does.
fn ask(path: &Path, limit: Duration) -> (Probe, Option<ProbeAnswer>, Option<i32>) {
    let Ok(mut s) = crate::transport::connect_own(path) else { return (Probe::NotRunning, None, None) };
    let pid = nix::sys::socket::getsockopt(&s, nix::sys::socket::sockopt::LocalPeerPid).ok();
    let _ = s.set_read_timeout(Some(limit));
    let _ = s.set_write_timeout(Some(limit));
    if ClientMsg::Probe.write_to(&mut s).is_err() {
        return (Probe::Unknown, None, pid);
    }
    let mut line = String::new();
    let answer = BufReader::new(&s).read_line(&mut line).ok().and_then(|_| serde_json::from_str::<ProbeAnswer>(line.trim_end()).ok());
    match answer {
        Some(a) => (if a.shown { Probe::Shown } else { Probe::Free }, Some(a), pid),
        None => (Probe::Unknown, None, pid),
    }
}

/// Whether a session shows server `id`, asked within `limit`.
pub fn probe(id: &str, limit: Duration) -> Probe {
    ask(&socket_of(id), limit).0
}

/// "work (3)" when a name hides the id.
fn shown_as(id: &str) -> String {
    match instance_label(id) {
        l if l == id || id.is_empty() => l,
        l => format!("{l} ({id})"),
    }
}

/// Sends `msg` to server `id` at `sock`: `Ok(None)` accepted, `Ok(Some)` a line to say instead, `Err` its refusal.
fn tell(id: &str, sock: &Path, msg: ClientMsg) -> Result<Option<String>, String> {
    let n = instance_label(id);
    let mut s = match crate::transport::connect_own(sock) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(Some(format!("{n}: {e}"))),
        Err(_) => return Ok(Some(format!("{n}: not running"))),
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    msg.write_to(&mut s).map_err(|e| format!("{n}: {e}"))?;
    crate::client::refusal(&s).map_or(Ok(None), Err)
}

/// Stops server `id` listening at `sock` and waits up to 5 s for it to go, so never on a main loop.
/// Ok is the line to show, Err the server's refusal.
pub fn stop(id: &str, sock: &Path) -> Result<String, String> {
    if let Some(said) = tell(id, sock, ClientMsg::Shutdown)? {
        return Ok(said);
    }
    let n = instance_label(id);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if sock.exists() {
        return Ok(format!("{n}: did not respond within 5 s"));
    }
    let again = if id.is_empty() { "nerd".to_string() } else { format!("nerd --resume {n}") };
    Ok(format!("{n}: stopped, resume with {again}"))
}

/// Has server `id` listening at `sock` swap itself for the installed build.
pub fn update(id: &str, sock: &Path) -> Result<String, String> {
    Ok(tell(id, sock, ClientMsg::Update)?.unwrap_or_else(|| format!("{}: updating in place", instance_label(id))))
}

/// Names server `id`, or clears its name when `name` is empty. Only files: no server is needed.
pub fn rename(id: &str, name: &str) -> Result<String, String> {
    crate::session::set_name(id, name)?;
    Ok(if name.is_empty() { format!("{id}: name cleared") } else { format!("{id} is now {name}: nerd --resume {name}") })
}

/// What a removal takes, and what it leaves.
pub const GOES: &str = "its layout, pane text, messages, routes, logs and saved agent session ids";
pub const STAYS: &str = "config.json, plugins and the agents' own sessions stay";

/// Removes stopped server `id`'s folder; `own` is the caller's own server. Only ever `instances/<id>`, and
/// never one a server runs from: renamed first, then asked again, so a server started meanwhile keeps it.
pub fn remove(id: &str, own: Option<&str>) -> Result<String, String> {
    let n = shown_as(id);
    if id.is_empty() {
        return Err("refused: the legacy server keeps its state in nerd's own folder and is not removed".into());
    }
    if !crate::session::valid_id(id) || !instances().iter().any(|i| i == id) {
        return Err(format!("no server {id}"));
    }
    if own == Some(id) {
        return Err(format!("refused: {n} is the server this session shows"));
    }
    if probe(id, PROBE_LIMIT) != Probe::NotRunning {
        return Err(format!("refused: {n} is running; stop it first with nerd --stop {}", instance_label(id)));
    }
    let (dir, gone) = (instances_dir().join(id), instances_dir().join(format!(".gone-{id}")));
    let folders: Vec<String> = crate::session::load_from(&dir)
        .map(|s| s.spaces.iter().map(|sp| crate::render::unspoofed(&crate::log::escape(&sp.cwd.display().to_string()))).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&gone);
    std::fs::rename(&dir, &gone).map_err(|e| format!("{n}: not removed: {e}"))?;
    // A server started between the probe and the rename listens in the folder that moved.
    if ask(&gone.join("nerd.sock"), PROBE_LIMIT).0 != Probe::NotRunning {
        let back = std::fs::rename(&gone, &dir).err().map(|e| format!("; its folder stays at {}: {e}", gone.display())).unwrap_or_default();
        return Err(format!("refused: {n} started while it was being removed{back}"));
    }
    std::fs::remove_dir_all(&gone).map_err(|e| format!("{n}: {} not removed: {e}", gone.display()))?;
    crate::log::info(&format!("server {id} removed"));
    let spaces = if folders.is_empty() { "no spaces".into() } else { folders.join(", ") };
    Ok(format!("{n} removed ({spaces}): {GOES} went; {STAYS}"))
}

/// Every server, the legacy one only while it runs, and the daemons none of them is. `own` is the
/// calling server's id: it is not asked, it knows itself. Each probe waits up to `PROBE_LIMIT`, all
/// at once, so this is never called on a main loop.
pub fn servers(own: Option<&str>) -> (Vec<Server>, Vec<Stray>) {
    let mut ids = instances();
    ids.push(String::new());
    let asked: Vec<_> = std::thread::scope(|s| {
        let jobs: Vec<_> = ids
            .iter()
            .map(|id| s.spawn(move || if Some(id.as_str()) == own { (Probe::Unknown, None, None) } else { ask(&socket_of(id), PROBE_LIMIT) }))
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap_or((Probe::NotRunning, None, None))).collect()
    });
    let mut known = known(asked.iter().filter_map(|a| a.2));
    known.push(std::process::id() as i32);
    let list = ids
        .into_iter()
        .zip(asked)
        .filter_map(|(id, (probe, answer, _))| {
            let me = Some(id.as_str()) == own;
            let running = me || probe != Probe::NotRunning;
            if id.is_empty() && !running {
                return None;
            }
            let dir = dir_of(&id);
            let shown = match probe {
                _ if me => None,
                Probe::NotRunning | Probe::Free => Some(false),
                Probe::Shown => Some(true),
                Probe::Unknown => None,
            };
            Some(Server {
                label: instance_label(&id),
                running,
                shown,
                sessions: u8::from(shown == Some(true)),
                folders: crate::session::load_from(&dir).map(|s| s.spaces.into_iter().map(|sp| sp.cwd).collect()).unwrap_or_default(),
                last_used: last_used(&id),
                sandbox: crate::log::sandbox_status_in(&dir).unwrap_or_else(|| "unknown".into()),
                version: if me { Some(crate::version_line()) } else { answer.map(|a| a.version) },
                untouched: !id.is_empty() && dir.join("untouched").exists(),
                reserved_only: !id.is_empty() && !running && !dir.join("session.json").exists(),
                id,
            })
        })
        .collect();
    (list, strays(&daemons(), &known))
}

/// The pids listening on the sockets that answered, and on `NERD_SOCKET`: the server this process
/// was pointed at is the one it works with, not a stray.
fn known(answered: impl Iterator<Item = i32>) -> Vec<i32> {
    let mut known: Vec<i32> = answered.collect();
    if let Ok(p) = std::env::var("NERD_SOCKET") {
        known.extend(ask(Path::new(&p), PROBE_LIMIT).2);
    }
    known
}

/// This user's `nerd --daemon` processes: pid, executable, environment.
fn daemons() -> Vec<(i32, PathBuf, Vec<String>)> {
    // `PROC_UID_ONLY` from libproc.h, which nix's libc does not name.
    const PROC_UID_ONLY: u32 = 4;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { nix::libc::geteuid() };
    // SAFETY: a null buffer only asks for the size; the second call writes at most the buffer's length.
    let bytes = unsafe { nix::libc::proc_listpids(PROC_UID_ONLY, me, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; bytes as usize / 4 + 64];
    let n = unsafe { nix::libc::proc_listpids(PROC_UID_ONLY, me, pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    pids.truncate(n.max(0) as usize / 4);
    pids.into_iter()
        .filter(|p| *p > 0)
        .filter_map(|pid| {
            let (exe, argv, env) = crate::pty::procargs(pid)?;
            let nerd = exe.file_name().is_some_and(|n| n == "nerd") && argv.get(1).is_some_and(|a| a == "--daemon");
            nerd.then_some((pid, exe, env))
        })
        .collect()
}

/// SIGTERM to stray `pid`, only while it is still a `nerd --daemon` of this user started from `exe`: checked at the
/// click, since a pid listed a while ago may be another program by now.
pub fn stop_stray(pid: i32, exe: &Path) -> Result<String, String> {
    let home = std::env::var("HOME").ok();
    let Some((.., env)) = daemons().into_iter().find(|(p, e, _)| *p == pid && e == exe) else {
        return Err(format!("pid {pid} is no longer a nerd server of yours; nothing was stopped"));
    };
    if other_home(&env, home.as_deref()) {
        return Err(format!("pid {pid} is a server of another home; stop it from a window of that home"));
    }
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM).map_err(|e| format!("pid {pid} was not stopped: {e}"))?;
    crate::log::info(&format!("stray server pid {pid} ({}) sent SIGTERM", exe.display()));
    Ok(format!("pid {pid}: asked to stop"))
}

fn strays(daemons: &[(i32, PathBuf, Vec<String>)], known: &[i32]) -> Vec<Stray> {
    daemons
        .iter()
        .filter(|(pid, ..)| !known.contains(pid))
        .map(|(pid, exe, env)| Stray {
            pid: *pid,
            started: crate::transport::start_time(*pid).map(|us| SystemTime::UNIX_EPOCH + Duration::from_micros(us)),
            exe: exe.clone(),
            note: note(env, std::env::var("HOME").ok().as_deref()),
            other_home: other_home(env, std::env::var("HOME").ok().as_deref()),
        })
        .collect()
}

/// A daemon whose HOME differs from ours belongs to that home's windows: an agent's test nerd must never stop the owner's.
fn other_home(env: &[String], home: Option<&str>) -> bool {
    env.iter().find_map(|e| e.strip_prefix("HOME=")).is_some_and(|h| Some(h) != home)
}

/// Where a stray lives, from the environment it was started with.
fn note(env: &[String], home: Option<&str>) -> String {
    let var = |k: &str| env.iter().find_map(|e| e.strip_prefix(k)?.strip_prefix('='));
    if let Some(s) = var("NERD_SOCKET") {
        return format!("its own socket {s}");
    }
    if let Some(d) = var("NERD_STATE_DIR") {
        return format!("its own state folder {d}");
    }
    match (var("HOME"), var("NERD_INSTANCE")) {
        (Some(h), _) if Some(h) != home => format!("another home {h}"),
        (_, Some(n)) if !n.is_empty() => format!("server {n}, whose socket is gone"),
        _ => "no server folder leads to it".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::with_temp_state_dir;
    use std::io::Write as _;
    use std::time::Duration;

    fn snapshot(dir: &Path, spaces: usize) {
        let spaces: Vec<_> = (0..spaces).map(|_| serde_json::json!({"name": null, "cwd": "/tmp", "tabs": [], "active_tab": 0})).collect();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("session.json"), serde_json::json!({"version": 2, "spaces": spaces}).to_string()).unwrap();
    }

    fn age(p: &Path, secs_ago: u64) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
    }

    #[test]
    fn the_last_attached_server_beats_a_later_start() {
        with_temp_state_dir("servers-last", || {
            let (one, two) = (instances_dir().join("1"), instances_dir().join("2"));
            snapshot(&one, 1);
            snapshot(&two, 1);
            mark_attached("1").unwrap();
            age(&one.join("attached"), 600);
            // 2 was attached by an older build, an hour ago; 1 was restarted just now.
            age(&two.join("session.json"), 60);
            assert_eq!(crate::session::last_instance().as_deref(), Some("1"), "an attach stamp outranks any start");
            assert!(last_used("1").unwrap() < SystemTime::now() - Duration::from_secs(500), "the stamp, not the snapshot rewritten at start");
            mark_attached("2").unwrap();
            assert_eq!(crate::session::last_instance().as_deref(), Some("2"));
        });
    }

    #[test]
    fn an_empty_snapshot_is_never_the_last_used() {
        with_temp_state_dir("servers-empty", || {
            snapshot(&instances_dir().join("1"), 0);
            snapshot(&instances_dir().join("2"), 1);
            age(&instances_dir().join("2").join("session.json"), 3600);
            assert_eq!(last_used("1"), None, "the LaunchAgent's server, started with no window");
            assert_eq!(crate::session::last_instance().as_deref(), Some("2"));
            std::fs::remove_dir_all(instances_dir().join("2")).unwrap();
            assert_eq!(crate::session::last_instance(), None);
        });
    }

    #[test]
    fn dot_folders_are_not_servers_and_a_new_one_is_reserved() {
        with_temp_state_dir("servers-dots", || {
            std::fs::create_dir_all(instances_dir().join(".gone-3")).unwrap();
            std::fs::create_dir_all(instances_dir().join("1")).unwrap();
            assert_eq!(instances(), vec!["1".to_string()]);
            let a = crate::session::new_instance_name();
            let b = crate::session::new_instance_name();
            assert_eq!((a.as_str(), b.as_str()), ("2", "3"), "each call holds its number");
            assert!(instances_dir().join("3").is_dir());
        });
    }

    #[test]
    fn a_folder_with_no_snapshot_and_no_server_is_reserved_only() {
        with_temp_state_dir("servers-reserved", || {
            std::fs::create_dir_all(instances_dir().join("1")).unwrap();
            snapshot(&instances_dir().join("2"), 1);
            std::fs::write(instances_dir().join("2").join("untouched"), "").unwrap();
            let (list, _) = servers(None);
            let one = list.iter().find(|s| s.id == "1").unwrap();
            let two = list.iter().find(|s| s.id == "2").unwrap();
            assert!(one.reserved_only && !one.running && !one.untouched, "{one:?}");
            assert!(!two.reserved_only && two.untouched && two.folders == vec![PathBuf::from("/tmp")], "{two:?}");
            assert_eq!((two.shown, two.sessions, two.version.as_deref()), (Some(false), 0, None));
            assert!(list.iter().all(|s| !s.id.is_empty()), "the legacy server only when it runs");
        });
    }

    #[test]
    fn a_server_answers_the_probe_and_its_own_id_is_not_asked() {
        short_state("probe", || {
            let dir = instances_dir().join("1");
            snapshot(&dir, 1);
            let listener = std::os::unix::net::UnixListener::bind(dir.join("nerd.sock")).unwrap();
            let serve = std::thread::spawn(move || {
                for shown in [true, false] {
                    let (s, _) = listener.accept().unwrap();
                    let mut r = BufReader::new(&s);
                    assert!(matches!(ClientMsg::read_from(&mut r).unwrap(), Some(ClientMsg::Probe)));
                    let a = ProbeAnswer { shown, version: "nerd 9".into(), spaces: 1 };
                    (&s).write_all(format!("{}\n", serde_json::to_string(&a).unwrap()).as_bytes()).unwrap();
                }
                // An older build closes on what it does not know.
                drop(listener.accept().unwrap());
                listener
            });
            assert_eq!(probe("1", PROBE_LIMIT), Probe::Shown);
            let (list, _) = servers(None);
            assert_eq!((list[0].running, list[0].shown, list[0].version.as_deref()), (true, Some(false), Some("nerd 9")));
            assert_eq!(probe("1", PROBE_LIMIT), Probe::Unknown);
            let listener = serve.join().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (list, _) = servers(Some("1"));
            assert!(list[0].running && list[0].shown.is_none(), "{:?}", list[0]);
            assert!(listener.accept().is_err(), "the server's own socket was not asked");
            assert_eq!(probe("2", PROBE_LIMIT), Probe::NotRunning);
        });
    }

    #[test]
    fn a_daemon_no_folder_leads_to_is_a_stray_and_one_at_nerd_socket_is_not() {
        let d = |pid, env: &[&str]| (pid, PathBuf::from("/x/nerd"), env.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let found = strays(&[d(10, &["HOME=/h"]), d(11, &["HOME=/other"]), d(12, &["HOME=/h", "NERD_SOCKET=/tmp/nt/s.sock"])], &[10]);
        assert_eq!(found.iter().map(|s| s.pid).collect::<Vec<_>>(), [11, 12]);
        assert_eq!(found[1].note, "its own socket /tmp/nt/s.sock");
        assert_eq!(note(&["HOME=/other".into()], Some("/h")), "another home /other");
        assert!(other_home(&["HOME=/other".into()], Some("/h")) && !other_home(&["HOME=/h".into()], Some("/h")) && !other_home(&[], Some("/h")));
        assert_eq!(note(&["HOME=/h".into(), "NERD_STATE_DIR=/tmp/nt".into()], Some("/h")), "its own state folder /tmp/nt");
        assert_eq!(note(&["HOME=/h".into(), "NERD_INSTANCE=7".into()], Some("/h")), "server 7, whose socket is gone");
        assert_eq!(note(&["HOME=/h".into()], Some("/h")), "no server folder leads to it");

        short_state("socket", || {
            let path = crate::paths::state_root().join("s.sock");
            let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            assert!(known(std::iter::empty()).is_empty());
            unsafe { std::env::set_var("NERD_SOCKET", &path) };
            let known = known(std::iter::empty());
            unsafe { std::env::remove_var("NERD_SOCKET") };
            assert_eq!(known, [std::process::id() as i32], "the server at NERD_SOCKET, by the pid that listens there");
        });
    }

    #[test]
    fn a_stray_gets_sigterm_only_while_it_is_still_a_nerd_server() {
        let (me, exe) = (std::process::id() as i32, std::env::current_exe().unwrap());
        assert!(stop_stray(me, &exe).unwrap_err().contains("no longer a nerd server"), "this test is no nerd --daemon");
    }

    fn one_shell() -> crate::session::WorkspaceSnapshot {
        serde_json::from_value(serde_json::json!({"version": 2, "spaces": [{"name": null, "cwd": "/tmp", "tabs": [{
            "name": null, "layout": {"Pane": 1}, "focused": 1,
            "panes": [{"id": 1, "cwd": "/tmp", "argv": ["/bin/zsh"], "agent_session": null, "title": "zsh", "cols": 80, "rows": 24}],
        }]}]}))
        .unwrap()
    }

    #[test]
    fn a_cd_a_title_a_resize_or_the_last_pane_closing_is_not_a_touch() {
        let mut s = one_shell();
        assert!(!touched(&s, false));
        let p = &mut s.spaces[0].tabs[0].panes[0];
        (p.cwd, p.title, p.cols, p.rows) = ("/Users".into(), Some("~/src".into()), 200, 60);
        assert!(!touched(&s, false), "these change at every prompt");
        s.spaces.clear();
        assert!(!touched(&s, false), "`exit` in the only pane");
    }

    #[test]
    fn each_structural_change_is_a_touch() {
        type Change = fn(&mut crate::session::WorkspaceSnapshot);
        let changes: [(&str, Change); 10] = [
            ("a second space", |s| s.spaces.push(s.spaces[0].clone())),
            ("a second tab", |s| {
                let t = s.spaces[0].tabs[0].clone();
                s.spaces[0].tabs.push(t);
            }),
            ("a split", |s| {
                let mut p = s.spaces[0].tabs[0].panes[0].clone();
                p.id = 2;
                s.spaces[0].tabs[0].panes.push(p);
            }),
            ("a space name", |s| s.spaces[0].name = Some("work".into())),
            ("a tab name", |s| s.spaces[0].tabs[0].name = Some("main".into())),
            ("a pane name", |s| s.spaces[0].tabs[0].panes[0].label = Some("build".into())),
            ("an agent session", |s| s.spaces[0].tabs[0].panes[0].agent_session = Some("abc".into())),
            ("a pane's isolation", |s| s.spaces[0].tabs[0].panes[0].isolation.files = crate::policy::Files::Folder),
            ("a space's isolation for new panes", |s| s.spaces[0].isolation_new = Some(Default::default())),
            ("a tab's isolation for new panes", |s| s.spaces[0].tabs[0].isolation_new = Some(Default::default())),
        ];
        for (what, change) in changes {
            let mut s = one_shell();
            change(&mut s);
            assert!(touched(&s, false), "{what}");
        }
        assert!(touched(&one_shell(), true), "an agent seen in front");
    }

    #[test]
    fn the_mark_is_written_for_a_folder_and_gone_for_good_once_named() {
        with_temp_state_dir("servers-untouched", || {
            let id = crate::session::new_instance_name();
            mark_untouched(&id).unwrap();
            assert!(untouched(&id));
            mark_untouched("").unwrap();
            mark_untouched("../x").unwrap();
            mark_untouched("9").unwrap();
            assert!(!crate::paths::state_root().join("untouched").exists() && !instances_dir().join("9").exists(), "no folder is made");
            crate::session::set_name(&id, "work").unwrap();
            assert!(!untouched(&id), "a server name is a touch");
            crate::session::set_name(&id, "").unwrap();
            assert!(!untouched(&id), "and the mark does not come back");
        });
    }

    #[test]
    fn a_removal_refuses_what_it_must_and_takes_only_that_folder() {
        short_state("remove", || {
            let root = crate::paths::state_root();
            let spaced = |id: &str| {
                let cwd = root.join(format!("proj{id}"));
                std::fs::create_dir_all(&cwd).unwrap();
                std::fs::create_dir_all(instances_dir().join(id)).unwrap();
                let s = serde_json::json!({"version": 2, "spaces": [{"name": null, "cwd": cwd, "tabs": [], "active_tab": 0}]});
                std::fs::write(instances_dir().join(id).join("session.json"), s.to_string()).unwrap();
                cwd
            };
            let (_, _, three) = (spaced("1"), spaced("2"), spaced("3"));
            let five = spaced(".gone-5");
            let _listener = std::os::unix::net::UnixListener::bind(socket_of("1")).unwrap();

            assert!(remove("", None).unwrap_err().starts_with("refused: the legacy server"));
            assert_eq!(remove("9", None).unwrap_err(), "no server 9");
            assert_eq!(remove("../1", None).unwrap_err(), "no server ../1");
            assert_eq!(remove(".gone-5", None).unwrap_err(), "no server .gone-5");
            assert_eq!(remove("2", Some("2")).unwrap_err(), "refused: 2 is the server this session shows");
            assert_eq!(remove("1", None).unwrap_err(), "refused: 1 is running; stop it first with nerd --stop 1");
            assert!(instances_dir().join("1").is_dir() && instances_dir().join("2").is_dir());

            assert!(crate::session::recent_spaces().contains(&three));
            assert!(!crate::session::recent_spaces().contains(&five), "a folder on its way out is no server");
            let said = remove("3", Some("2")).unwrap();
            assert!(said.starts_with(&format!("3 removed ({}): its layout", three.display())), "{said}");
            assert!(!instances_dir().join("3").exists() && !instances_dir().join(".gone-3").exists());
            assert!(!crate::session::recent_spaces().contains(&three), "its spaces are no longer offered");
            assert_eq!(instances(), ["1", "2"]);
        });
    }

    /// A socket path has room for 103 bytes, which the usual temp folder leaves too little of.
    fn short_state<R>(name: &str, f: impl FnOnce() -> R) -> R {
        with_temp_state_dir(name, || {
            let dir = PathBuf::from(format!("/tmp/nerd-sv-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            unsafe { std::env::set_var("NERD_STATE_DIR", &dir) };
            let r = f();
            let _ = std::fs::remove_dir_all(&dir);
            r
        })
    }
}
