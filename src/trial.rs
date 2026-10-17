//! `nerd --plugin-trial <folder>`: the plugin run once in its own sandbox, with no network and nowhere to write but a
//! temporary data folder, sent each event it declared, and reported on. For authors and the catalog's review.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::plugins::Event;

const HANDSHAKE: Duration = Duration::from_secs(5);
const RUN: Duration = Duration::from_secs(20);
/// A line longer than this is not a message, and is not read further.
const LINE: u64 = 1 << 20;

pub fn main(args: &[String]) -> std::io::Result<()> {
    let [dir] = args else {
        eprintln!("usage: nerd --plugin-trial <folder>");
        std::process::exit(2);
    };
    let (ok, report) = run(Path::new(dir), RUN);
    print!("{report}");
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Whether it connected and kept running, and what happened, in words.
pub fn run(dir: &Path, time: Duration) -> (bool, String) {
    let c = crate::agents::check_folder(dir, false);
    let mut out = crate::install::report(&c);
    let Some(mut m) = c.manifest.filter(|_| c.problems.is_empty()) else { return (false, out) };
    if m.exec.is_none() {
        out += "Nothing to run: it only describes an agent.\n";
        return (true, out);
    }
    let tmp = std::env::temp_dir().join(format!("nerd-trial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (ok, said) = trial(&mut m, &tmp, time);
    let _ = std::fs::remove_dir_all(&tmp);
    out += "\nTrial, with no network and no write paths:\n";
    out += &said;
    (ok, out)
}

/// The end of the plugin's log, read outside its sandbox: never through a link, never a pipe that would block,
/// never more than the last 16 KB. The plugin chose what sits at that name.
fn log_tail(log: &Path) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    const MOST: u64 = 16 * 1024;
    // The folder too may be a link the plugin put there: it is opened without following, and the log inside it.
    let (Some(dir), Some(name)) = (log.parent(), log.file_name().and_then(|n| std::ffi::CString::new(n.as_encoded_bytes()).ok())) else { return String::new() };
    let Ok(dir) = std::fs::OpenOptions::new().read(true).custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW).open(dir) else { return String::new() };
    // SAFETY: a valid directory descriptor and a NUL-terminated name; the result is checked before use.
    let fd = unsafe { nix::libc::openat(std::os::fd::AsRawFd::as_raw_fd(&dir), name.as_ptr(), nix::libc::O_RDONLY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK) };
    if fd < 0 {
        return String::new();
    }
    // SAFETY: fd was just opened and is owned by nothing else.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let Ok(meta) = f.metadata() else { return String::new() };
    if !meta.is_file() {
        return String::new();
    }
    let _ = f.seek(SeekFrom::Start(meta.len().saturating_sub(MOST)));
    let mut bytes = Vec::new();
    let _ = f.take(MOST).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn trial(m: &mut crate::agents::Manifest, tmp: &Path, time: Duration) -> (bool, String) {
    let mut said = String::new();
    // The real state root stays out of its reach, as it is for an installed plugin.
    let real = std::fs::canonicalize(crate::paths::state_root()).unwrap_or_else(|_| crate::paths::state_root());
    // Its data folder and socket go under the temporary folder, and the profile is built for them.
    unsafe {
        std::env::set_var("NERD_STATE_DIR", tmp.join("state"));
        std::env::set_var("NERD_SOCKET", tmp.join("t.sock"));
    }
    m.permissions = Default::default();
    let data = crate::plugins::data_dir(&m.name);
    let built = std::fs::create_dir_all(&data).map_err(|e| e.to_string()).and_then(|()| crate::plugins::profile(m));
    let Ok(mut profile) = built else { return (false, format!("  no sandbox profile: {}\n", built.unwrap_err())) };
    // The last matching rule wins: its own folders are allowed again after the deny.
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
    let own: Vec<String> = m.dir.iter().map(|d| canon(d)).chain([canon(&data)]).collect();
    let real = real.to_string_lossy().into_owned();
    if std::iter::once(&real).chain(&own).any(|p| p.contains(['"', '\\']) || p.chars().any(char::is_control)) {
        return (false, "  no sandbox profile: a folder's name cannot go into one\n".into());
    }
    profile += &format!("(deny file-read-data (subpath \"{real}\"))\n");
    profile += &format!("(allow file-read-data {})\n", own.iter().map(|p| format!("(subpath \"{p}\")")).collect::<Vec<_>>().join(" "));
    let sock = crate::session::plugin_socket_path();
    let listener = match crate::plugins::bind(&sock) {
        Ok(l) => l,
        Err(e) => return (false, format!("  no socket: {e}\n")),
    };
    let _ = listener.set_nonblocking(true);
    let log = data.join("output.log");
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nerd"));
    let mut cmd = crate::spawn::cmd(exe);
    cmd.arg("--plugin-exec").arg(&m.program).args(m.exec.as_ref().map(|e| e.args.clone()).unwrap_or_default());
    cmd.current_dir(m.dir.as_deref().unwrap_or(Path::new("/")))
        .env("NERD_PLUGIN_PROFILE", &profile)
        .env("NERD_PLUGIN_LOG", &log)
        .env("NERD_SOCKET", &sock)
        .env("NERD_PLUGIN", &m.name)
        .env("NERD_PLUGIN_DATA", &data)
        .env("TMPDIR", &data)
        .stdin(std::process::Stdio::null());
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid is async-signal-safe; the whole group is ended below.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(drop).map_err(std::io::Error::from));
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, format!("  did not start: {e}\n")),
    };
    let start = Instant::now();
    let stream = loop {
        match listener.accept() {
            Ok((s, _)) => break Some(s),
            Err(_) if start.elapsed() < HANDSHAKE && child.try_wait().ok().flatten().is_none() => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let mut ok = false;
    let mut received: Vec<String> = Vec::new();
    match stream {
        None => said += "  it never connected\n",
        Some(s) => {
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let mut reader = BufReader::new(s.try_clone().expect("a socket clones"));
            let mut w = s;
            let first = match next_line(&mut reader, Instant::now() + HANDSHAKE, LINE) {
                Read::Line(l) => l,
                Read::End | Read::Late => String::new(),
            };
            if !first.contains("\"Connect\"") {
                said += "  its first line was not Connect\n";
            } else {
                said += "  connected\n";
                ok = true;
                let mut send = |ev: &Event| {
                    let _ = writeln!(w, "{}", serde_json::to_string(ev).unwrap_or_default());
                };
                send(&Event::Connected { protocol: crate::proto::PROTOCOL });
                let sample = tmp.join("sample");
                for ev in events(m, &sample) {
                    send(&ev);
                }
                let until = Instant::now() + time;
                while child.try_wait().ok().flatten().is_none() {
                    match next_line(&mut reader, until, LINE) {
                        // Its words reach the terminal that runs the trial: no escape sequence goes with them.
                        Read::Line(line) => received.push(serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|v| v.as_object().and_then(|o| o.keys().next().map(|k| crate::render::unspoofed(k).chars().take(40).collect()))).unwrap_or_else(|| "a line that is not a message".into())),
                        Read::End | Read::Late => break,
                    }
                }
            }
        }
    }
    let status = child.try_wait().ok().flatten();
    if status.is_some() {
        ok = false;
    }
    // Its group, and any descendant that left it with a session of its own; one whose parent already exited belongs
    // to launchd and is not found here, which is why the review runs on a machine of its own.
    let root = child.id() as i32;
    if let Some(procs) = crate::detect::procs() {
        for pid in crate::detect::descendants(&procs, root) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(root), nix::sys::signal::Signal::SIGKILL);
    let _ = child.wait();
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for r in &received {
        *counts.entry(r).or_default() += 1;
    }
    said += &match counts.is_empty() {
        true => "  it sent nothing\n".to_string(),
        false => format!("  it sent {}\n", counts.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", ")),
    };
    match status {
        Some(s) => said += &format!("  it exited on its own before the end ({s})\n"),
        None => said += &format!("  it was still running after {} s and was stopped\n", time.as_secs()),
    }
    let text = log_tail(&log);
    let tail: Vec<&str> = text.lines().rev().take(20).collect();
    if !tail.is_empty() {
        said += "  its output:\n";
        for l in tail.into_iter().rev() {
            said += &format!("    {}\n", crate::render::unspoofed(l));
        }
    }
    (ok, said)
}

pub(crate) enum Read {
    Line(String),
    End,
    Late,
}

/// One line, never past `until` and never longer than `LINE`: each pass is one socket read of at most the read
/// timeout, so a plugin that drips bytes without a newline cannot hold the trial.
pub(crate) fn next_line(reader: &mut BufReader<std::os::unix::net::UnixStream>, until: Instant, most: u64) -> Read {
    let mut line: Vec<u8> = Vec::new();
    loop {
        if Instant::now() >= until {
            return Read::Late;
        }
        let (used, done) = match reader.fill_buf() {
            Ok([]) => return Read::End,
            Ok(buf) => match buf.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&buf[..=i]);
                    (i + 1, true)
                }
                None => {
                    line.extend_from_slice(buf);
                    (buf.len(), false)
                }
            },
            Err(_) => (0, false),
        };
        reader.consume(used);
        if done || line.len() as u64 >= most {
            return Read::Line(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

/// One of each event the manifest declares, about a file and a pane that exist only for the trial.
fn events(m: &crate::agents::Manifest, sample: &Path) -> Vec<Event> {
    let has = |e: &str| m.events.iter().any(|x| x == e);
    let mut out = Vec::new();
    if has("pane_opened") {
        out.push(Event::PaneOpened { pane: 1, cwd: sample.to_path_buf() });
    }
    if has("pane_cwd") {
        out.push(Event::PaneCwd { pane: 1, cwd: sample.join("sub") });
    }
    if has("agent_state") {
        out.push(Event::AgentState { pane: 1, agent: "claude".into(), state: "working".into() });
    }
    if has("file_chosen") {
        out.push(Event::FileChosen { path: sample.join("a.txt") });
    }
    if has("command") {
        out.extend(m.commands.iter().map(|c| Event::Command { id: c.id.clone(), pane: None, path: None }));
    }
    if has("preview_request") {
        out.extend(m.previews.iter().flat_map(|p| p.extensions.clone()).enumerate().map(|(i, x)| Event::PreviewRequest { req: i as u64 + 1, path: sample.join(format!("a.{x}")), cols: 60, rows: 20 , mode: None }));
    }
    if has("panel_opened") {
        out.extend(m.panels.iter().map(|p| Event::PanelOpened { id: p.id.clone(), pane: None, path: None }));
    }
    if has("panel_item") {
        out.extend(m.panels.iter().map(|p| Event::PanelItem { panel: p.id.clone(), item: "trial".into() }));
    }
    if has("pane_closed") {
        out.push(Event::PaneClosed { pane: 1 });
    }
    out
}
