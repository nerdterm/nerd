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
#[allow(dead_code)] // until a bare `nerd` asks it before it attaches
pub fn probe(id: &str, limit: Duration) -> Probe {
    ask(&socket_of(id), limit).0
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

fn strays(daemons: &[(i32, PathBuf, Vec<String>)], known: &[i32]) -> Vec<Stray> {
    daemons
        .iter()
        .filter(|(pid, ..)| !known.contains(pid))
        .map(|(pid, exe, env)| Stray {
            pid: *pid,
            started: crate::transport::start_time(*pid).map(|us| SystemTime::UNIX_EPOCH + Duration::from_micros(us)),
            exe: exe.clone(),
            note: note(env, std::env::var("HOME").ok().as_deref()),
        })
        .collect()
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
