//! Thin client: the user's terminal <-> the server's socket. Panes live on the server, so the client can be closed anytime.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use crate::proto::{take_requests, ClientMsg, Requests};
use crate::session::socket_path;

const SPAWN_WAIT: Duration = Duration::from_secs(5);

/// Sets `NERD_INSTANCE` so every state path below, and the daemon we may spawn, resolve to it.
fn select_instance(resume: Option<Option<String>>) -> std::io::Result<String> {
    let name = match resume {
        None => crate::session::new_instance_name(),
        // By id or by name; an unknown one no longer makes an empty desktop.
        Some(Some(n)) => crate::session::resolve(&n).ok_or_else(|| unknown_instance(&n))?,
        Some(None) => crate::session::last_instance()
            .ok_or_else(|| std::io::Error::other("no servers to resume; run nerd to start one"))?,
    };
    // Single-threaded at this point, so the Rust 2024 unsafety of set_var is theoretical.
    unsafe { std::env::set_var("NERD_INSTANCE", &name) };
    Ok(name)
}

pub fn run(resume: Option<Option<String>>, from_window: bool) -> std::io::Result<()> {
    // Each way out before the session still leaves its `left:` line.
    let early = |e: std::io::Error| {
        leave_with(Err(&e));
        e
    };
    // A typo in a name says more than a missing terminal; an old single-instance layout is moved first.
    crate::session::migrate_legacy();
    if let Some(Some(n)) = &resume
        && crate::session::resolve(n).is_none()
    {
        return Err(early(unknown_instance(n)));
    }
    // Before any server starts: one with no window to show runs on alone (an agent's tool shell has no
    // terminal); before an instance is chosen, the line lands in the main log (none where nerd never ran).
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(early(std::io::Error::other("not a terminal: nerd shows its window in one; nothing was started")));
    }
    // A failed refresh leaves the old hook working; it is not a reason to refuse to start.
    let _ = crate::hook::refresh();
    let name = select_instance(resume).map_err(early)?;
    guard();
    let mut stream = connect_or_spawn().map_err(early)?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let (cols, rows) = crossterm::terminal::size().map_err(early)?;

    let tui = Tui::up(crate::config::load_or_init().mouse).map_err(early)?;
    let mut out = std::io::stdout();
    // The key file is rewritten on every start, so an update carries a changed key map into
    // the terminal without anyone running anything. Ghostty is asked to reread only when it
    // actually changed, and only when its config pulls our file in.
    if crate::ghostty::refresh().unwrap_or(false) && crate::ghostty::is_linked() {
        reload_ghostty();
    }
    // tmux/screen swallow Kitty graphics anyway, and turn the query itself into a pane title.
    let mux = in_multiplexer();
    // `tmux` and `ps` are asked with their output captured, so nothing of theirs reaches the screen.
    let terminal = if mux { terminal_app() } else { None };
    // After the alternate screen: a terminal that prints the unknown query leaves it there, not in scrollback.
    let graphics = !mux && query_graphics();
    // After the query: tmux would otherwise leave the query's text as the window name.
    let _ = out.write_all(format!("\x1b]2;nerd {}\x07", crate::render::unspoofed(&crate::session::instance_label(&name))).as_bytes());
    let _ = out.flush();

    let dark = appearance_dark();
    let appearance = appearance_poller(dark);
    let hello = ClientMsg::Hello {
        cwd,
        cols,
        rows,
        mux,
        screen: mux && std::env::var_os("TMUX").is_none(),
        graphics: Some(graphics),
        terminal: terminal.clone(),
        opened_by_nerd: from_window,
        opens_windows: terminal.is_some(),
        dark,
        host: hostname(),
        bye: false,
        connects: true,
        reattach: None,
        machine: None,
    };
    crate::log::info(&format!("started in {}, mux={mux}", term_program()));
    let result = hello
        .write_to(&mut stream)
        .and_then(|()| session(BufReader::new(stream.try_clone()?), stream, Vec::new(), terminal, mux, dark, &appearance, ssh_path(), true));
    drop(tui);
    leave_with(result.as_ref());
    match result? {
        Seen { refused: Some(line), .. } => Err(std::io::Error::other(line)),
        _ => Ok(()),
    }
}

fn term_program() -> String {
    std::env::var("TERM_PROGRAM").unwrap_or_else(|_| "?".into())
}

/// Set by whoever writes the client's one `left:` line.
static LEFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The ssh this client waits on, or 0: a signal that ends the client ends it too, or it would hold a prompt in the terminal.
static SSH: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The `left:` line, unless another thread wrote it first.
fn leave(level: crate::log::Level, why: &str) {
    if !LEFT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        crate::log::log(level, &format!("left: {why}"));
    }
}

/// The `left:` line for how the session ended; after a signal, the process exits with 128 + it.
fn leave_with(result: Result<&Seen, &std::io::Error>) {
    let signal = crate::signals::reason();
    let level = if result.is_err() && signal.is_none() { crate::log::Level::Error } else { crate::log::Level::Info };
    leave(level, &leave_reason(signal, result));
    let sig = crate::signals::caught();
    if sig != 0 {
        std::process::exit(128 + sig);
    }
}

/// Signals, panics and blocking waits each end in one `left:` line; set up once per client, after `NERD_INSTANCE` is set.
fn guard() {
    crate::signals::install();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload_as_str().unwrap_or("unknown");
        let at = info.location().map(|l| format!(" ({l})")).unwrap_or_default();
        let thread = std::thread::current();
        match thread.name() {
            Some("main") => leave(crate::log::Level::Error, &format!("panic: {msg}{at}")),
            name => crate::log::error(&format!("panic in thread {}: {msg}{at}", name.unwrap_or("unnamed"))),
        }
        hook(info);
    }));
    // A wait that never wakes for a signal (ssh's password prompt, Enter after an error) still ends.
    std::thread::spawn(|| {
        let mut since = None;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let sig = crate::signals::caught();
            if sig == 0 || since.get_or_insert_with(Instant::now).elapsed() < Duration::from_secs(1) {
                continue;
            }
            leave(crate::log::Level::Info, &crate::signals::reason().unwrap_or_default());
            let ssh = SSH.swap(0, std::sync::atomic::Ordering::SeqCst);
            if ssh > 0 {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(ssh), nix::sys::signal::Signal::SIGTERM);
            }
            tui_down(true);
            std::process::exit(128 + sig);
        }
    });
}

/// A signal comes first: an error it caused (a read on a hung-up terminal) is not the cause.
fn leave_reason(signal: Option<String>, result: Result<&Seen, &std::io::Error>) -> String {
    if let Some(signal) = signal {
        return signal;
    }
    match result {
        Ok(Seen { refused: Some(line), .. }) => line.clone(),
        Ok(Seen { stopped: Some(why), .. }) => format!("the server stopped ({})", printable(why)),
        Ok(Seen { bye: true, .. }) => "the server ended this session (detach, takeover or disconnect)".into(),
        Ok(_) => "the connection closed (a local client is not told why; the server's line says)".into(),
        Err(e) => format!("error: {e}"),
    }
}

fn hostname() -> Option<String> {
    nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned())
}

/// Raw mode and the alternate screen, undone on every way out: a return, a `?`, a panic.
struct Tui(bool);

/// Set while the TUI is up: undoing it twice would restore the cursor over what was printed between.
static TUI_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Tui {
    fn up(mouse_on: bool) -> std::io::Result<Tui> {
        // Before the first change, so a failure halfway is undone too.
        let tui = Tui(mouse_on);
        TUI_UP.store(true, std::sync::atomic::Ordering::SeqCst);
        // A panic's message must land on the screen the human keeps, not the one torn down after it.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tui_down(mouse_on);
            hook(info);
        }));
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen)?;
        if mouse_on {
            execute!(out, EnableMouseCapture)?;
        }
        let _ = execute!(out, crossterm::event::EnableBracketedPaste);
        Ok(tui)
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        tui_down(self.0);
    }
}

fn tui_down(mouse_on: bool) {
    // Locked before the swap: a frame the screen thread is writing goes out whole first, and none after it.
    let mut out = std::io::stdout().lock();
    if !TUI_UP.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let _ = execute!(out, crossterm::event::DisableBracketedPaste);
    if mouse_on {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = out.write_all(&crate::pointer::osc(crate::pointer::DEFAULT));
    // the terminal gets its own background back
    let _ = out.write_all(b"\x1b]111\x07");
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Stdout for the screen thread: refused once the TUI is down, which a signal does without joining that thread.
struct Screen;

impl Write for Screen {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf).map(|()| buf.len())
    }
    /// Under one lock, so a frame is never cut by `tui_down`.
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        if !TUI_UP.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        out.write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

/// Kitty graphics query followed by DA1: every terminal answers DA1, so its reply ends the wait.
/// Reads fd 0 directly: `std::io::stdin()` would buffer bytes that crossterm then never sees.
fn query_graphics() -> bool {
    use std::io::IsTerminal as _;
    use std::os::fd::{AsFd as _, AsRawFd as _};
    use nix::poll::{PollFd, PollFlags, PollTimeout};

    let stdin = std::io::stdin();
    // crossterm reads /dev/tty then, and poll on a device is what macOS does not promise.
    if !stdin.is_terminal() {
        return false;
    }
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c").and_then(|()| out.flush()).is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut fds, PollTimeout::from(left.as_millis().min(500) as u16)) {
            Ok(n) if n > 0 => {}
            _ => return false,
        }
        match nix::unistd::read(stdin.as_raw_fd(), &mut buf) {
            Ok(n) if n > 0 => got.extend_from_slice(&buf[..n]),
            _ => return false,
        }
        if let Some(ok) = graphics_reply(&got) {
            return ok;
        }
    }
}

/// `None` until the DA1 reply (`ESC [ ? ... c`) has arrived; then whether the Kitty query was answered OK.
fn graphics_reply(b: &[u8]) -> Option<bool> {
    let da = b.windows(3).position(|w| w == b"\x1b[?")?;
    b[da + 3..].iter().find(|c| !(c.is_ascii_digit() || **c == b';')).filter(|c| **c == b'c')?;
    let ok: &[u8] = b"\x1b_Gi=31;OK";
    Some(b[..da].windows(ok.len()).any(|w| w == ok))
}

/// Terminals nerd can start with a command and that show images: moving there is only worth it for those.
const KNOWN_TERMINALS: [&str; 3] = ["Ghostty.app", "kitty.app", "WezTerm.app"];

/// The app bundle showing this tmux session, if nerd could open a window in it.
/// `None` for screen, for a session with more than one client (the window could open
/// where nobody is sitting), and for terminals nerd cannot drive.
fn terminal_app() -> Option<PathBuf> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let session = tmux(&["display", "-p", "-t", &pane, "#{session_name}"])?;
    let clients = tmux(&["list-clients", "-t", session.trim(), "-F", "#{client_pid}"])?;
    if !alone_on_the_session(&clients) {
        return None;
    }
    let pid: u32 = tmux(&["display", "-p", "-t", &pane, "#{client_pid}"])?.trim().parse().ok()?;
    app_bundle(&walk_to_app(pid)?)
}

/// One client, one screen to move: with two, the window could open where nobody is sitting.
fn alone_on_the_session(clients: &str) -> bool {
    clients.lines().filter(|l| !l.trim().is_empty()).count() == 1
}

fn tmux(args: &[&str]) -> Option<String> {
    let out = crate::spawn::cmd("tmux").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// SIGUSR2 is how Ghostty is told to reread its config. Only our own terminal is signalled -
/// the one this client is running inside - never every Ghostty the user has open.
fn reload_ghostty() {
    let Some(exe) = walk_to_app(std::process::id()) else { return };
    if !exe.contains("Ghostty.app/Contents/MacOS/") {
        return;
    }
    let mut pid = std::process::id();
    for _ in 0..10 {
        let Ok(out) = crate::spawn::cmd("ps").args(["-o", "ppid=,comm=", "-p", &pid.to_string()]).output() else { return };
        let line = String::from_utf8_lossy(&out.stdout);
        let Some((parent, comm)) = line.trim().split_once(char::is_whitespace) else { return };
        if comm.contains("Ghostty.app/Contents/MacOS/") {
            // `kill` rather than a crate: one signal is not worth a dependency.
            let _ = crate::spawn::cmd("kill").args(["-USR2", &pid.to_string()]).status();
            return;
        }
        let Ok(next) = parent.trim().parse() else { return };
        pid = next;
    }
}

/// Up the process tree from the tmux client: it is a child of the terminal, through `login`.
fn walk_to_app(from: u32) -> Option<String> {
    let mut pid = from;
    for _ in 0..10 {
        let out = crate::spawn::cmd("ps").args(["-o", "ppid=,comm=", "-p", &pid.to_string()]).output().ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let (parent, comm) = line.trim().split_once(char::is_whitespace)?;
        if comm.contains(".app/Contents/MacOS/") {
            return Some(comm.to_string());
        }
        pid = parent.trim().parse().ok()?;
    }
    None
}

/// `/Applications/Ghostty.app/Contents/MacOS/ghostty` -> `/Applications/Ghostty.app`, if we know it.
fn app_bundle(exe: &str) -> Option<PathBuf> {
    let at = exe.find(".app/")?;
    let bundle = PathBuf::from(&exe[..at + 4]);
    let name = bundle.file_name()?.to_str()?;
    KNOWN_TERMINALS.contains(&name).then_some(bundle)
}

/// Starts nerd in a new window of this terminal, attached to the same server: `open` hands our
/// environment to the new process, so it resolves the same socket even for a test instance.
fn open_window(app: &std::path::Path, args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = crate::spawn::cmd("/usr/bin/open");
    // `open` hands our environment to the new process: without this it would inherit the
    // multiplexer we are leaving and think it is still inside one.
    for k in MUX_VARS {
        cmd.env_remove(k);
    }
    cmd.arg("-na")
        .arg(app)
        .arg("--args")
        .arg("-e")
        .arg(&exe)
        .args(args)
        .arg("--from-window")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = crate::spawn::detached(&mut cmd);
}

/// Everything a multiplexer leaves behind, cleared from a window we open outside it;
/// the broker strips the same set from panes (`src/broker.rs`).
const MUX_VARS: [&str; 4] = ["TMUX", "TMUX_PANE", "STY", "WINDOW"];

/// tmux and screen turn an APC query into a pane title, and carry no graphics to ask about.
/// Only the two that say a multiplexer is running: `WINDOW` is set by other programs too.
fn in_multiplexer() -> bool {
    ["TMUX", "STY"].iter().any(|k| std::env::var_os(k).is_some())
}

const APPEARANCE_POLL: Duration = Duration::from_secs(2);
const UNKNOWN: u8 = 0;
const DARK: u8 = 1;
const LIGHT: u8 = 2;

fn encode(dark: Option<bool>) -> u8 {
    match dark {
        Some(true) => DARK,
        Some(false) => LIGHT,
        None => UNKNOWN,
    }
}

/// macOS keeps `AppleInterfaceStyle` only while the appearance is dark; `None` when `defaults` can't answer.
fn appearance_dark() -> Option<bool> {
    let out = crate::spawn::cmd("/usr/bin/defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "Dark")
}

type Appearance = std::sync::Arc<std::sync::atomic::AtomicU8>;

/// Polled on its own thread, once per client: `defaults` is a process, and the input loop must not wait for it.
/// ceiling: a 2 s poll, not a notification; observing AppleInterfaceThemeChangedNotification needs Objective-C.
fn appearance_poller(dark: Option<bool>) -> Appearance {
    let appearance = Appearance::new(std::sync::atomic::AtomicU8::new(encode(dark)));
    let keep = appearance.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(APPEARANCE_POLL);
        keep.store(encode(appearance_dark()), std::sync::atomic::Ordering::Relaxed);
    });
    appearance
}

/// What came down the stream, for telling a detach from a stop from a loss.
#[derive(Default, Debug)]
struct Seen {
    bytes: bool,
    armed: bool,
    bye: bool,
    stopped: Option<String>,
    /// The server's `refused:` line, sent instead of a first frame.
    refused: Option<String>,
}

/// Copies the stream to `out` until it ends; `first` arrived before the terminal was ready for it.
fn pump(first: Vec<u8>, mut reader: impl Read, out: &mut impl Write, mut ask: impl FnMut(&Requests)) -> Seen {
    let mut seen = Seen::default();
    let mut carry = Vec::new();
    let mut data = first;
    let mut buf = [0u8; 65536];
    loop {
        if !seen.bytes
            && let Some(rest) = data.strip_prefix(b"\x1b[?1049l\r\nrefused: ".as_slice())
        {
            let why = String::from_utf8_lossy(rest);
            seen.refused = Some(format!("refused: {}", why.split("\r\n").next().unwrap_or_default()));
            data.clear();
        }
        if !data.is_empty() {
            seen.bytes = true;
            let mut all = std::mem::take(&mut carry);
            all.extend_from_slice(&data);
            let r = take_requests(&all, &mut carry);
            seen.armed |= r.armed;
            seen.bye |= r.bye;
            if r.stopped.is_some() {
                seen.stopped = r.stopped.clone();
            }
            if out.write_all(&r.out).and_then(|()| out.flush()).is_err() {
                return seen;
            }
            ask(&r);
        }
        data = match reader.read(&mut buf) {
            Ok(0) | Err(_) => return seen,
            Ok(n) => buf[..n].to_vec(),
        };
    }
}

#[allow(clippy::too_many_arguments)]
fn session(
    reader: impl Read + Send + 'static,
    mut writer: impl Write,
    first: Vec<u8>,
    terminal: Option<PathBuf>,
    mux: bool,
    dark: Option<bool>,
    appearance: &Appearance,
    ssh: PathBuf,
    connects: bool,
) -> std::io::Result<Seen> {
    let instance = crate::session::instance_name();
    // Answers from ssh threads go out through the one writer the input loop owns.
    let (tx, rx) = std::sync::mpsc::channel::<ClientMsg>();
    let fetched: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>> = Default::default();
    let pump = std::thread::Builder::new().name("screen".into()).spawn(move || {
        let mut out = Screen;
        pump(first, reader, &mut out, |r| {
            if r.reload {
                reload_ghostty();
            }
            for _ in 0..r.open {
                if let Some(app) = &terminal {
                    open_window(app, &["--resume", &instance]);
                }
            }
            let (list, connect) = connect_asks(r, connects);
            for name in list {
                list_for_server(&ssh, name, &terminal, mux, &fetched, &tx);
            }
            for (name, id) in connect {
                open_remote_window(name, id.as_deref(), &terminal, mux, &fetched);
            }
        })
    })?;

    let mut sent = encode(dark);

    'input: loop {
        if pump.is_finished() {
            break;
        }
        // The pump is not joined: it reads until the server closes a socket this client still holds.
        if crate::signals::caught() != 0 {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        while let Ok(msg) = rx.try_recv() {
            if msg.write_to(&mut writer).is_err() {
                break 'input;
            }
        }
        let now = appearance.load(std::sync::atomic::Ordering::Relaxed);
        if now != sent && now != UNKNOWN {
            sent = now;
            if ClientMsg::Appearance(now == DARK).write_to(&mut writer).is_err() {
                break;
            }
        }
        if !crossterm::event::poll(Duration::from_millis(50))? {
            continue;
        }
        let ev = crossterm::event::read()?;
        if ClientMsg::Event(ev).write_to(&mut writer).is_err() {
            break;
        }
    }
    drop(writer);
    pump.join().map_err(|p| {
        let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
        let why = format!("panic in the screen thread: {msg}");
        leave(crate::log::Level::Error, &why);
        std::io::Error::other(why)
    })
}

/// A remote client serves no connect requests: that server would reach this Mac's other machines.
fn connect_asks(r: &Requests, connects: bool) -> (&[String], &[(String, Option<String>)]) {
    if connects { (&r.list, &r.connect) } else { (&[], &[]) }
}

/// The window app this client can open a terminal in: its tmux's, or the one it runs in.
fn window_app(terminal: &Option<PathBuf>, mux: bool) -> Option<PathBuf> {
    terminal.clone().or_else(|| if mux { None } else { app_bundle(&walk_to_app(std::process::id())?) })
}

type Fetched = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>;

/// Under the server's 20 s wait for the answer, so a hung list call is named rather than just unanswered.
const MENU_LIST_LIMIT: Duration = Duration::from_secs(15);

/// Connect to...: lists a machine from this client's config over BatchMode ssh, so no prompt reaches raw mode.
fn list_for_server(ssh: &Path, name: &str, terminal: &Option<PathBuf>, mux: bool, fetched: &Fetched, tx: &std::sync::mpsc::Sender<ClientMsg>) {
    let Ok(entry) = usable_remote(&crate::config::load_or_init().remote_entries(), name) else { return };
    let (ssh, terminal, fetched, tx) = (ssh.to_path_buf(), terminal.clone(), fetched.clone(), tx.clone());
    std::thread::spawn(move || {
        let (result, ssh_failed) = match list_remote(&ssh, &entry, Some(MENU_LIST_LIMIT)) {
            Ok(list) => {
                let ids = list.iter().map(|i| i.id.clone()).collect();
                fetched.lock().unwrap_or_else(|e| e.into_inner()).insert(entry.name.clone(), ids);
                (Ok(list), false)
            }
            Err((ssh_failed, e)) => (Err(e), ssh_failed),
        };
        let window = window_app(&terminal, mux).is_some();
        let _ = tx.send(ClientMsg::Remote { name: entry.name, result, window, ssh_failed });
    });
}

/// Only a machine in this client's config, and only an instance this client listed itself.
fn open_remote_window(name: &str, id: Option<&str>, terminal: &Option<PathBuf>, mux: bool, fetched: &Fetched) {
    if usable_remote(&crate::config::load_or_init().remote_entries(), name).is_err() {
        return;
    }
    if let Some(id) = id
        && !fetched.lock().unwrap_or_else(|e| e.into_inner()).get(name).is_some_and(|ids| ids.iter().any(|i| i == id))
    {
        return;
    }
    if let Some(app) = window_app(terminal, mux) {
        let mut args = vec!["--remote", name];
        args.extend(id);
        open_window(&app, &args);
    }
}

/// The only ssh nerd runs. A debug build takes a stand-in for the live check in a nested tmux.
fn ssh_path() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("NERD_TEST_SSH") {
        return p.into();
    }
    PathBuf::from("/usr/bin/ssh")
}

/// A non-interactive ssh command gets a bare PATH; these are where nerd is installed.
const REMOTE_PATH: &str = r#"PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH""#;

/// A machine by name from config.json, usable or with the reason it is not.
fn usable_remote(remotes: &[crate::config::RemoteEntry], name: &str) -> Result<crate::config::RemoteEntry, String> {
    let Some(entry) = remotes.iter().find(|r| r.name == name) else {
        let known: Vec<String> = remotes.iter().map(|r| printable(&r.name)).collect();
        let known = if known.is_empty() { "none; add remotes to config.json".to_string() } else { known.join(", ") };
        return Err(format!("no machine {} in config.json (known: {known})", printable(name)));
    };
    entry.check().map_err(|e| format!("machine {name} in config.json cannot be used: {e}"))?;
    Ok(entry.clone())
}

/// Text from another machine or a file, safe to print: one line, no control characters.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

fn last_line(stderr: &str) -> String {
    stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).map(printable).unwrap_or_default()
}

/// The instances on a machine, through `nerd --list --json` there; the error says whether ssh itself failed.
/// With a limit (the menu) ssh runs in BatchMode and is killed when it runs out; without one (a terminal) a human waits.
fn list_remote(ssh: &Path, entry: &crate::config::RemoteEntry, limit: Option<Duration>) -> Result<Vec<crate::proto::RemoteInstance>, (bool, String)> {
    use std::process::Stdio;
    let mut cmd = crate::spawn::cmd(ssh);
    cmd.args(["-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none"]);
    if limit.is_some() {
        cmd.args(["-n", "-o", "BatchMode=yes"]);
    }
    cmd.arg("--").arg(&entry.host).arg(format!("{REMOTE_PATH}; nerd --list --json"));
    crate::log::info(&format!("remote: ssh to {} to list its servers", logged_host(&entry.host)));
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| (false, format!("cannot start {}: {e}", ssh.display())))?;
    if limit.is_none() {
        SSH.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    }
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err((false, "ssh started without its pipes".into()));
    };
    let (out, err) = (collect(out), collect(err));
    let deadline = limit.map(|l| Instant::now() + l);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if deadline.is_none_or(|d| Instant::now() < d) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if limit.is_none() {
        SSH.store(0, std::sync::atomic::Ordering::SeqCst);
    }
    let until = Instant::now() + Duration::from_millis(500);
    let (stdout, stderr) = (collected(out, until), collected(err, until));
    let Some(status) = status else {
        return Err((false, format!("ssh to {} did not answer in {} s", entry.name, limit.unwrap_or_default().as_secs())));
    };
    match status.code() {
        Some(0) => parse_listing(&stdout, &entry.name).map_err(|e| (false, e)),
        code => Err((code == Some(255), ssh_failure(&entry.name, code, &String::from_utf8_lossy(&stderr)))),
    }
}

/// Errors table rows 2, 3 and 7: what ssh's exit and its last word mean.
fn ssh_failure(name: &str, code: Option<i32>, stderr: &str) -> String {
    let last = last_line(stderr);
    match code {
        Some(127) => format!("{name} has no nerd (looked in PATH, ~/.local/bin, /opt/homebrew/bin, /usr/local/bin)"),
        Some(255) if last.contains("timed out") => format!("ssh to {name} timed out"),
        Some(255) if last.is_empty() => format!("ssh to {name} failed"),
        Some(255) => format!("ssh to {name} failed: {last}"),
        Some(c) if last.is_empty() => format!("ssh to {name} ended with exit {c}"),
        Some(c) => format!("ssh to {name} ended with exit {c}: {last}"),
        None => format!("ssh to {name} was killed"),
    }
}

/// Only instances whose id and label are safe to put in argv, a title or a dialog survive.
fn parse_listing(stdout: &[u8], name: &str) -> Result<Vec<crate::proto::RemoteInstance>, String> {
    let text = String::from_utf8_lossy(stdout);
    let Ok(listing) = serde_json::from_str::<crate::proto::Listing>(text.trim()) else {
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).map(printable).unwrap_or_default();
        return Err(if first.is_empty() {
            format!("nerd on {name} is too old for --remote; update it there")
        } else {
            format!("nerd on {name} is older than this one, or its shell startup prints text: {first}")
        });
    };
    match listing.relay.cmp(&crate::proto::RELAY_VERSION) {
        std::cmp::Ordering::Less => Err(format!("nerd on {name} is too old for --remote; update nerd on {name}")),
        std::cmp::Ordering::Greater => Err(format!("nerd on {name} is newer than this one; update nerd here")),
        std::cmp::Ordering::Equal => Ok(listing
            .instances
            .into_iter()
            .filter(|i| crate::session::valid_id(&i.id) && crate::session::valid_label(&i.label))
            .collect()),
    }
}

/// The instances as commands, one per line, for a message that has to offer a choice.
fn as_commands(name: &str, list: &[crate::proto::RemoteInstance]) -> String {
    list.iter()
        .map(|i| {
            let state = if i.running { "running" } else { "stopped" };
            let label = if i.label == i.id { String::new() } else { format!("{}, ", i.label) };
            format!("\n  nerd --remote {name} {}   ({label}{state})", i.id)
        })
        .collect()
}

/// The instance asked for by id or label; without one, the only one running.
fn pick_instance<'a>(list: &'a [crate::proto::RemoteInstance], want: Option<&str>, name: &str) -> Result<&'a crate::proto::RemoteInstance, String> {
    let chosen = match want {
        Some(w) => list
            .iter()
            .find(|i| i.id == w)
            .or_else(|| list.iter().find(|i| i.label == w))
            .ok_or_else(|| format!("{name} has no server {}{}", printable(w), as_commands(name, list)))?,
        None => {
            let running: Vec<&crate::proto::RemoteInstance> = list.iter().filter(|i| i.running).collect();
            match running.as_slice() {
                [one] => *one,
                [] if list.is_empty() => return Err(format!("{name} has no servers; start one there with nerd")),
                [] => return Err(format!("no server runs on {name}; start one there with nerd --resume <id>{}", as_commands(name, list))),
                _ => return Err(format!("several servers run on {name}; pick one:{}", as_commands(name, &running.into_iter().cloned().collect::<Vec<_>>()))),
            }
        }
    };
    if !chosen.running {
        return Err(stopped_there(name, &chosen.id));
    }
    Ok(chosen)
}

fn stopped_there(name: &str, id: &str) -> String {
    format!("server {id} on {name} is stopped; start it there with nerd --resume {id}")
}

/// How a remote session ended.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// A detach or a takeover: nothing to say.
    Quiet,
    /// Armed, no `BYE`, ssh did not end cleanly: the panes may still run there.
    Lost,
    End(String),
}

/// What ended a remote session, as the human should hear it.
fn remote_outcome(name: &str, id: &str, seen: &Seen, exit: Option<i32>, stderr: &str, list: &[crate::proto::RemoteInstance]) -> Outcome {
    if let Some(line) = &seen.refused {
        return Outcome::End(line.clone());
    }
    if seen.bye {
        return Outcome::Quiet;
    }
    if let Some(why) = &seen.stopped {
        return Outcome::End(format!("the server on {name} stopped ({})", printable(why)));
    }
    if seen.armed {
        return match exit {
            Some(0) => Outcome::End(format!("the server on {name} ended unexpectedly")),
            _ => Outcome::Lost,
        };
    }
    if seen.bytes {
        return Outcome::End(format!("the session on {name} ended"));
    }
    Outcome::End(match exit {
        Some(3) => stopped_there(name, id),
        Some(4) => format!("{name} has no server {id}{}", as_commands(name, list)),
        Some(5) => format!("nerd on {name} refused the server name {id}"),
        Some(0) => format!("the session on {name} ended"),
        code => ssh_failure(name, code, stderr),
    })
}

/// What Close prints after a loss.
fn lost_message(name: &str, id: &str) -> String {
    format!("the connection to {name} was lost. The panes keep running there. Reconnect with nerd --remote {name} {id}")
}

/// A pipe read to its end on its own thread, and what came so far.
type Collected = (std::thread::JoinHandle<()>, std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

fn collect(mut pipe: impl Read + Send + 'static) -> Collected {
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = got.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = pipe.read(&mut buf) {
            keep.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&buf[..n]);
        }
    });
    (reader, got)
}

/// What came, waiting for the pipe's end no later than `until`: a grandchild of ssh may hold it open.
fn collected((reader, got): Collected, until: Instant) -> Vec<u8> {
    while !reader.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    got.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// ssh running `nerd --relay <id>` there, its stdio the session's stream.
struct Relay {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    stderr: Collected,
}

/// The host as the log keeps it: without the user name before an `@`.
fn logged_host(host: &str) -> String {
    crate::render::unspoofed(host.rsplit_once('@').map_or(host, |(_, h)| h))
}

fn spawn_relay(ssh: &Path, host: &str, id: &str) -> std::io::Result<Relay> {
    use std::process::Stdio;
    crate::log::info(&format!("remote: ssh relay to {}, server {}", logged_host(host), crate::render::unspoofed(id)));
    let mut child = crate::spawn::cmd(ssh)
        .args(["-T", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"])
        .args(["-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ClearAllForwardings=yes"])
        .arg("--")
        .arg(host)
        .arg(format!("{REMOTE_PATH}; nerd --relay {id}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| std::io::Error::other(format!("cannot start {}: {e}", ssh.display())))?;
    SSH.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let (Some(stdin), Some(stdout), Some(err)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        return Err(std::io::Error::other("ssh started without its pipes"));
    };
    Ok(Relay { child, stdin, stdout, stderr: collect(err) })
}

/// ssh's exit code and what it said; ssh gets a few seconds to go after its stdin closed.
fn finish(mut child: std::process::Child, stderr: Collected) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                break child.wait().ok();
            }
        }
    };
    SSH.store(0, std::sync::atomic::Ordering::SeqCst);
    let said = collected(stderr, Instant::now() + Duration::from_millis(500));
    (status.and_then(|s| s.code()), String::from_utf8_lossy(&said).into_owned())
}

/// `nerd --remote <name> [instance]`; from a window nerd opened, an error waits for Enter.
pub fn remote(name: Option<String>, want: Option<String>, from_window: bool) -> std::io::Result<()> {
    crate::log::info(&format!("started for {} in {}, mux={}", printable(name.as_deref().unwrap_or("?")), term_program(), in_multiplexer()));
    guard();
    let result = remote_session(name, want, from_window);
    let ended = Seen { bye: true, ..Seen::default() };
    leave_with(result.as_ref().map(|()| &ended));
    if let Err(e) = &result
        && from_window
    {
        eprintln!("nerd: {e}\n\nPress Enter to close this window.");
        let _ = std::io::stdin().read_line(&mut String::new());
        std::process::exit(1);
    }
    result
}

fn remote_session(name: Option<String>, want: Option<String>, from_window: bool) -> std::io::Result<()> {
    let Some(name) = name else {
        return Err(std::io::Error::other("usage: nerd --remote <machine> [server]; machines are the remotes in config.json"));
    };
    let cfg = crate::config::load_or_init();
    let entry = usable_remote(&cfg.remote_entries(), &name).map_err(std::io::Error::other)?;
    let ssh = ssh_path();
    let mut list = list_remote(&ssh, &entry, None).map_err(|(_, e)| std::io::Error::other(e))?;
    let mut inst = pick_instance(&list, want.as_deref(), &name).map_err(std::io::Error::other)?.clone();

    let mux = in_multiplexer();
    // Asked now, while no ssh prompt can be on screen; the answer needs raw mode.
    let graphics = !mux && Tui::up(false).is_ok_and(|_tui| query_graphics());
    let appearance = appearance_poller(appearance_dark());
    // Set from the first loss on: when the link last dropped, and why the last reconnect failed.
    let mut lost: Option<(Instant, Option<String>)> = None;
    loop {
        if let Some((since, failed)) = &lost {
            if !reconnect_screen(&name, *since, failed.as_deref(), &cfg, &appearance)? {
                return Err(std::io::Error::other(lost_message(&name, &inst.id)));
            }
            eprintln!("nerd: reconnecting to {name}…");
            let again = list_remote(&ssh, &entry, None)
                .map_err(|(_, e)| e)
                .and_then(|l| pick_instance(&l, Some(&inst.id), &name).cloned().map(|i| (l, i)));
            match again {
                Ok((l, i)) => (list, inst) = (l, i),
                Err(e) => {
                    lost = Some((*since, Some(e)));
                    continue;
                }
            }
        }
        let (seen, exit, said) = attach_remote(&ssh, &entry, &name, &inst, &cfg, mux, graphics, &appearance, from_window)?;
        match remote_outcome(&name, &inst.id, &seen, exit, &said, &list) {
            Outcome::Quiet => return Ok(()),
            // The count starts at ssh's exit; ServerAlive took up to 45 s to notice before that.
            Outcome::Lost => lost = Some((Instant::now(), None)),
            Outcome::End(msg) if !seen.bytes && let Some((since, _)) = lost => lost = Some((since, Some(msg))),
            Outcome::End(msg) => return Err(std::io::Error::other(msg)),
        }
    }
}

/// One relay: Hello, the cooked-mode wait for the server's first bytes, then the session; ssh's end read.
#[allow(clippy::too_many_arguments)]
fn attach_remote(
    ssh: &Path,
    entry: &crate::config::RemoteEntry,
    name: &str,
    inst: &crate::proto::RemoteInstance,
    cfg: &crate::config::Config,
    mux: bool,
    graphics: bool,
    appearance: &Appearance,
    from_window: bool,
) -> std::io::Result<(Seen, Option<i32>, String)> {
    let (cols, rows) = crossterm::terminal::size()?;
    let dark = appearance_dark();
    let hello = ClientMsg::Hello {
        // Used only by an instance with no spaces; a path missing there starts the shell in that Mac's home.
        cwd: crate::config::home_dir(),
        cols,
        rows,
        mux,
        screen: false,
        graphics: Some(graphics),
        terminal: None,
        opened_by_nerd: from_window,
        opens_windows: false,
        dark,
        host: hostname(),
        bye: true,
        connects: false,
        reattach: Some(format!("nerd --remote {name} {}", inst.id)),
        machine: Some(name.to_string()),
    };
    let Relay { child, mut stdin, mut stdout, stderr } = spawn_relay(ssh, &entry.host, &inst.id)?;
    let _ = hello.write_to(&mut stdin);
    // Cooked mode until the server speaks: ssh may still be asking for a password or a host key.
    let mut first = vec![0u8; 65536];
    let n = stdout.read(&mut first).unwrap_or(0);
    first.truncate(n);
    let seen = if n == 0 {
        drop(stdin);
        Seen::default()
    } else {
        let tui = Tui::up(cfg.mouse)?;
        let _ = write!(std::io::stdout(), "\x1b]2;nerd {}\x07", crate::render::unspoofed(&format!("{} @ {name}", inst.label)));
        // The window may have changed size while ssh was asking.
        if let Ok((c, r)) = crossterm::terminal::size() {
            let _ = ClientMsg::Event(Event::Resize(c, r)).write_to(&mut stdin);
        }
        let result = session(stdout, stdin, first, None, mux, dark, appearance, ssh.to_path_buf(), false);
        drop(tui);
        result?
    };
    let (exit, said) = finish(child, stderr);
    Ok((seen, exit, said))
}

/// Where the reconnect screen starts, from the top-left corner.
const RECONNECT_TOP: u16 = 1;
const RECONNECT_LEFT: u16 = 2;
/// Worded and spaced like a dialog's buttons.
const RECONNECT: &str = " [Enter] reconnect ";
const CLOSE: &str = " [Esc] close ";

/// The reconnect screen's lines, then its Reconnect and Close buttons; the lines are cut so the buttons fit in `rows`.
fn reconnect_layout(name: &str, secs: u64, failed: Option<&str>, rows: u16) -> (Vec<String>, ratatui::layout::Rect, ratatui::layout::Rect) {
    use ratatui::layout::Rect;
    let mut lines = vec![format!("nerd: the link to {name} dropped {secs} s ago. The panes keep running there.")];
    if let Some(e) = failed {
        lines.push(String::new());
        lines.extend(e.lines().map(printable));
    }
    let room = usize::from(rows.saturating_sub(4)).max(2);
    if lines.len() > room {
        lines.truncate(room - 1);
        lines.push("\u{2026}".into());
    }
    let row = RECONNECT_TOP + lines.len() as u16 + 1;
    let width = |s: &str| s.chars().count() as u16;
    let reconnect = Rect::new(RECONNECT_LEFT, row, width(RECONNECT), 1);
    let close = Rect::new(reconnect.right() + 1, row, width(CLOSE), 1);
    (lines, reconnect, close)
}

/// ratatui's colour as crossterm writes it; the theme's colours are all RGB.
fn crossterm_color(c: ratatui::style::Color) -> crossterm::style::Color {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
        _ => crossterm::style::Color::Reset,
    }
}

/// Waits for Reconnect (click or Enter) or Close (click, Esc or Ctrl-C) on the alternate screen; true is Reconnect.
fn reconnect_screen(name: &str, since: Instant, failed: Option<&str>, cfg: &crate::config::Config, appearance: &Appearance) -> std::io::Result<bool> {
    // The buttons answer the mouse whatever config.json says about the panes.
    let tui = Tui::up(true)?;
    let mut out = std::io::stdout();
    let chosen = reconnect_wait(&mut out, name, since, failed, cfg, appearance);
    let _ = execute!(out, crossterm::cursor::Show);
    drop(tui);
    // Motion reports still queued would otherwise reach ssh's password prompt.
    let _ = nix::sys::termios::tcflush(std::io::stdin(), nix::sys::termios::FlushArg::TCIFLUSH);
    chosen
}

fn reconnect_wait(out: &mut std::io::Stdout, name: &str, since: Instant, failed: Option<&str>, cfg: &crate::config::Config, appearance: &Appearance) -> std::io::Result<bool> {
    use crossterm::cursor::{Hide, MoveTo};
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
    use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
    use crossterm::terminal::{Clear, ClearType};
    let mut shown = None;
    loop {
        let secs = since.elapsed().as_secs();
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let (lines, reconnect, close) = reconnect_layout(name, secs, failed, rows);
        if shown != Some(secs) {
            shown = Some(secs);
            let dark = match appearance.load(std::sync::atomic::Ordering::Relaxed) {
                UNKNOWN => None,
                v => Some(v == DARK),
            };
            let (theme, _) = crate::render::pick_theme(cfg, dark);
            let (ground, text) = (crossterm_color(theme.ground), crossterm_color(theme.text));
            // The terminal's own background takes nerd's ground, as the server does; tui_down gives it back.
            if let ratatui::style::Color::Rgb(r, g, b) = theme.ground {
                write!(out, "\x1b]11;#{r:02x}{g:02x}{b:02x}\x07")?;
            }
            let width = usize::from(cols.saturating_sub(RECONNECT_LEFT));
            crossterm::queue!(out, Hide, SetBackgroundColor(ground), Clear(ClearType::All), SetForegroundColor(text))?;
            for (i, line) in lines.iter().enumerate() {
                let line: String = line.chars().take(width).collect();
                crossterm::queue!(out, MoveTo(RECONNECT_LEFT, RECONNECT_TOP + i as u16), Print(line))?;
            }
            // As a dialog draws them: the one Enter picks in the accent bar, the other plain.
            let key = crossterm_color(theme.border_focus);
            crossterm::queue!(
                out,
                MoveTo(reconnect.x, reconnect.y),
                SetForegroundColor(ground),
                SetBackgroundColor(key),
                SetAttribute(Attribute::Bold),
                Print(RECONNECT),
                SetAttribute(Attribute::NormalIntensity),
                SetBackgroundColor(ground),
                MoveTo(close.x, close.y),
                SetForegroundColor(text),
                Print(CLOSE),
            )?;
            out.flush()?;
        }
        if crate::signals::caught() != 0 {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if !crossterm::event::poll(Duration::from_millis(200))? {
            continue;
        }
        match crossterm::event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc => return Ok(false),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(false),
                _ => {}
            },
            // On release: a press would leave its release to reach ssh's prompt in cooked mode.
            Event::Mouse(m) if m.kind == MouseEventKind::Up(MouseButton::Left) => {
                let at = ratatui::layout::Position::new(m.column, m.row);
                if reconnect.contains(at) {
                    return Ok(true);
                }
                if close.contains(at) {
                    return Ok(false);
                }
            }
            Event::Resize(..) => shown = None,
            _ => {}
        }
    }
}

/// `nerd --relay <id>`, the far end of `--remote`: never starts a server; exits 3 not running, 4 unknown, 5 bad name.
pub fn relay(id: Option<String>) -> ! {
    let code = match relay_target(id.as_deref().unwrap_or("")) {
        Err((code, msg)) => {
            eprintln!("nerd: {msg}");
            code
        }
        Ok(id) => {
            // SAFETY: single-threaded here, before socket_path() reads it.
            unsafe { std::env::set_var("NERD_INSTANCE", &id) };
            guard();
            crate::log::info("started as a relay over ssh");
            match crate::transport::connect_own(&socket_path()) {
                Ok(s) => {
                    leave(crate::log::Level::Info, &copy_both_ways(s));
                    0
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("nerd: {e}");
                    leave(crate::log::Level::Error, &format!("error: {e}"));
                    3
                }
                Err(_) => {
                    eprintln!("nerd: server {id} is not running");
                    leave(crate::log::Level::Info, "the server is not running");
                    3
                }
            }
        }
    };
    std::process::exit(code)
}

fn relay_target(id: &str) -> Result<String, (i32, String)> {
    if !crate::session::valid_id(id) {
        return Err((5, "bad server name".into()));
    }
    crate::session::resolve(id).filter(|r| !r.is_empty()).ok_or_else(|| (4, format!("no server {id}")))
}

/// Until either end: the server's closing returns the relay's `left:` reason, stdin's end exits the relay.
fn copy_both_ways(sock: UnixStream) -> String {
    if let Ok(mut up) = sock.try_clone() {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut up);
            // Exit, not shutdown(Write): macOS can lose the server's wakeup on a half-close, and it would never see the client go.
            leave(crate::log::Level::Info, "ssh's input ended (the client left or the link dropped)");
            std::process::exit(0);
        });
    }
    let mut sock = sock;
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 65536];
    loop {
        match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = out.write_all(&buf[..n]).and_then(|()| out.flush()) {
                    return format!("ssh stopped taking the output ({e})");
                }
            }
            Err(e) => return format!("socket error: {e}"),
        }
    }
    "the server closed the connection".into()
}

fn connect_or_spawn() -> std::io::Result<UnixStream> {
    let path = socket_path();
    match crate::transport::connect_own(&path) {
        Ok(s) => return Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(e),
        Err(_) => {}
    }
    // Don't remove a dead socket here: the daemon does it before bind, and doing it twice
    // would let two simultaneous launches race and spawn two servers.
    let exe = std::env::current_exe()?;
    use std::os::unix::process::CommandExt as _;
    // New session, no controlling terminal, or the daemon dies with this window
    // and anything it spawns could reopen /dev/tty into it.
    let mut cmd = crate::spawn::cmd(exe);
    cmd.arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
    }
    cmd.spawn()?;

    let start = Instant::now();
    while start.elapsed() < SPAWN_WAIT {
        match crate::transport::connect_own(&path) {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(e),
            Err(_) => {}
        }
        // The server listens within a few ms; a slow start is waited for more patiently.
        std::thread::sleep(Duration::from_millis(if start.elapsed() < Duration::from_millis(100) { 3 } else { 50 }));
    }
    Err(std::io::Error::other(format!(
        "server did not start within {} s (socket {})",
        SPAWN_WAIT.as_secs(),
        path.display()
    )))
}

/// The error for a name nobody answers to, with the names that would work.
fn unknown_instance(query: &str) -> std::io::Error {
    let known: Vec<String> = crate::session::instances().iter().map(|id| crate::session::instance_label(id)).collect();
    let known = if known.is_empty() { "none; run nerd to start one".to_string() } else { known.join(", ") };
    std::io::Error::other(format!("no server {query}; known: {known}"))
}

/// Names an instance, or clears its name. Only touches files: no server is needed.
pub fn rename(instance: Option<String>, name: Option<String>) -> std::io::Result<()> {
    crate::session::migrate_legacy();
    let Some(query) = instance else {
        return Err(std::io::Error::other("usage: nerd --rename <server> [name]; without a name the name is cleared"));
    };
    let id = crate::session::resolve(&query).ok_or_else(|| unknown_instance(&query))?;
    let name = name.unwrap_or_default();
    crate::session::set_name(&id, &name).map_err(std::io::Error::other)?;
    if name.is_empty() {
        println!("{id}: name cleared");
    } else {
        println!("{id} is now {name}: nerd --resume {name}");
    }
    Ok(())
}

/// Panes close; the snapshot stays, so `--resume` brings the desktop back.
pub fn stop(name: Option<String>) -> std::io::Result<()> {
    crate::session::migrate_legacy();
    let names = match name {
        Some(n) => vec![crate::session::resolve(&n).ok_or_else(|| unknown_instance(&n))?],
        None => {
            every_instance("--stop")?;
            let mut v = crate::session::instances();
            if legacy_alive() {
                v.push(String::new());
            }
            v
        }
    };
    if names.is_empty() {
        println!("no servers");
        return Ok(());
    }
    for n in names {
        let label = crate::session::instance_label(&n);
        unsafe { std::env::set_var("NERD_INSTANCE", &n) };
        let path = socket_path();
        let n = label;
        let mut s = match crate::transport::connect_own(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                println!("{n}: {e}");
                continue;
            }
            Err(_) => {
                println!("{n}: not running");
                continue;
            }
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
        ClientMsg::Shutdown.write_to(&mut s)?;
        if let Some(line) = refusal(&s) {
            return Err(std::io::Error::other(line));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if path.exists() {
            println!("{n}: did not respond within 5 s");
        } else {
            let again = if n == "legacy" { "nerd".to_string() } else { format!("nerd --resume {n}") };
            println!("{n}: stopped, resume with {again}");
        }
    }
    Ok(())
}

/// Ends every connection to one server, or to each running one; the servers and panes stay.
pub fn disconnect(name: Option<String>) -> std::io::Result<()> {
    crate::session::migrate_legacy();
    let names = match name {
        Some(n) => vec![crate::session::resolve(&n).ok_or_else(|| unknown_instance(&n))?],
        None => {
            every_instance("--disconnect")?;
            let mut v = crate::session::instances();
            if legacy_alive() {
                v.push(String::new());
            }
            v
        }
    };
    if names.is_empty() {
        println!("no servers");
        return Ok(());
    }
    let mut any = false;
    for n in names {
        unsafe { std::env::set_var("NERD_INSTANCE", &n) };
        let label = crate::session::instance_label(&n);
        let s = match crate::transport::connect_own(&socket_path()) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                println!("{label}: {e}");
                continue;
            }
            Err(_) => {
                println!("{label}: not running");
                continue;
            }
        };
        match ask_to_disconnect(s) {
            Ok((ended, whole)) => {
                any = true;
                for line in &ended {
                    println!("{label}: {line}");
                }
                if ended.is_empty() || !whole {
                    println!("{label}: disconnected, but the server did not say whom it ended");
                }
            }
            Err(Some(line)) => return Err(std::io::Error::other(line)),
            Err(None) => println!(
                "{label}: this server is older than this nerd, so it could not disconnect anyone; update it from its menu, or stop it with nerd --stop {label}"
            ),
        }
    }
    if any {
        for line in crate::log::HYGIENE {
            println!("{line}");
        }
    }
    Ok(())
}

/// The server's lines after its `ok` and whether they reached EOF; its `refused:` line, or `None` when no `ok` came: an older build closes or stays silent.
fn ask_to_disconnect(mut s: UnixStream) -> Result<(Vec<String>, bool), Option<String>> {
    use std::io::BufRead as _;
    // Before the request: macOS refuses the option once the server has answered and closed.
    s.set_read_timeout(Some(Duration::from_secs(3))).map_err(|_| None)?;
    ClientMsg::DisconnectAll.write_to(&mut s).map_err(|_| None)?;
    let mut r = BufReader::new(s);
    let mut ok = String::new();
    r.read_line(&mut ok).map_err(|_| None)?;
    if ok.starts_with("refused: ") {
        return Err(Some(ok.trim_end().to_string()));
    }
    if ok != "ok\n" {
        return Err(None);
    }
    // A server from before the non-blocking farewell waits out a 3 s write timeout for each of the farewell and BYE to a stalled client.
    let _ = r.get_ref().set_read_timeout(Some(Duration::from_secs(10)));
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) => return Ok((lines, true)),
            Ok(_) if line.ends_with('\n') => lines.push(line.trim_end().to_string()),
            _ => return Ok((lines, false)),
        }
    }
}

/// The server's `refused:` line; an accepted request closes the socket at once, and the read timeout set before the request bounds the wait.
fn refusal(s: &UnixStream) -> Option<String> {
    use std::io::BufRead as _;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    line.starts_with("refused: ").then(|| line.trim_end().to_string())
}

/// The no-name forms act on every instance: from inside a pane they need a name, or `NERD_SOCKET`/`NERD_STATE_DIR` as the target.
fn every_instance(flag: &str) -> std::io::Result<()> {
    let targeted = std::env::var_os("NERD_SOCKET").is_some() || std::env::var_os("NERD_STATE_DIR").is_some();
    if targeted {
        return Ok(());
    }
    let Some(procs) = crate::detect::procs() else { return Ok(()) };
    if !crate::detect::under_broker(&procs, std::process::id() as i32) || crate::msock::pane_rights() {
        return Ok(());
    }
    crate::log::warn(&format!("refused nerd {flag} with no name, inside a pane"));
    let known: Vec<String> = crate::session::instances().iter().map(|id| crate::session::instance_label(id)).collect();
    Err(std::io::Error::other(format!(
        "refused: nerd {flag} with no name from inside a pane acts on every server; name one: nerd {flag} <server> (known: {})",
        if known.is_empty() { "none".into() } else { known.join(", ") }
    )))
}

fn legacy_alive() -> bool {
    unsafe { std::env::set_var("NERD_INSTANCE", "") };
    UnixStream::connect(socket_path()).is_ok()
}

/// Panes and screens survive the swap.
pub fn update(name: Option<String>) -> std::io::Result<()> {
    crate::session::migrate_legacy();
    let names = match name {
        Some(n) => vec![crate::session::resolve(&n).ok_or_else(|| unknown_instance(&n))?],
        None => {
            every_instance("--update")?;
            crate::session::instances()
        }
    };
    for n in names {
        unsafe { std::env::set_var("NERD_INSTANCE", &n) };
        let label = crate::session::instance_label(&n);
        match crate::transport::connect_own(&socket_path()) {
            Ok(mut s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
                ClientMsg::Update.write_to(&mut s)?;
                if let Some(line) = refusal(&s) {
                    return Err(std::io::Error::other(line));
                }
                println!("{label}: updating in place");
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => println!("{label}: {e}"),
            Err(_) => println!("{label}: not running"),
        }
    }
    Ok(())
}

/// `--json`: what `--remote` on another Mac reads; the legacy instance has no id to relay to.
pub fn status(json: bool) -> std::io::Result<()> {
    crate::session::migrate_legacy();
    if json {
        let instances = crate::session::instances()
            .into_iter()
            .map(|id| {
                // SAFETY: single-threaded; server_alive() and load() read it right after.
                unsafe { std::env::set_var("NERD_INSTANCE", &id) };
                crate::proto::RemoteInstance {
                    label: crate::session::instance_label(&id),
                    running: server_alive(),
                    spaces: crate::session::load().map_or(0, |s| s.spaces.len()),
                    id,
                }
            })
            .collect();
        let listing = crate::proto::Listing { relay: crate::proto::RELAY_VERSION, instances };
        println!("{}", serde_json::to_string(&listing).map_err(std::io::Error::other)?);
        return Ok(());
    }
    let mut names = crate::session::instances();
    if legacy_alive() {
        names.push(String::new());
    }
    if names.is_empty() {
        println!("no servers; run nerd to start one");
        return Ok(());
    }
    // "work (1)" when a name hides the id; the column fits the longest.
    let shown = |id: &str| match crate::session::instance_label(id) {
        label if label == id || id.is_empty() => label,
        label => format!("{label} ({id})"),
    };
    let width = names.iter().map(|n| shown(n).chars().count()).max().unwrap_or(4).max(4);
    for n in names {
        unsafe { std::env::set_var("NERD_INSTANCE", &n) };
        let n = shown(&n);
        let spaces: Vec<String> = crate::session::load()
            .map(|s| s.spaces.iter().map(|sp| listed_folder(&sp.cwd)).collect())
            .unwrap_or_default();
        println!(
            "{n:<width$} {:<11} sandbox: {:<40} {}",
            if server_alive() { "running" } else { "stopped" },
            crate::log::sandbox_status().unwrap_or_else(|| "unknown".into()),
            spaces.join(", ")
        );
    }
    Ok(())
}

/// A folder name reaches the terminal: controls become escapes, bidi and invisible characters go.
fn listed_folder(p: &std::path::Path) -> String {
    crate::render::unspoofed(&crate::log::escape(&p.display().to_string()))
}

pub fn server_alive() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

#[allow(dead_code)]
fn _event_is_serializable(e: Event) -> String {
    serde_json::to_string(&e).unwrap_or_default()
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_log_keeps_an_ssh_host_without_its_user() {
        assert_eq!(super::logged_host("me@studio.local"), "studio.local");
        assert_eq!(super::logged_host("studio"), "studio");
        assert_eq!(super::logged_host("a@b@host"), "host");
    }
    use super::graphics_reply;

    #[test]
    fn a_client_says_why_it_left() {
        use super::{leave_reason, Seen};
        assert_eq!(leave_reason(None, Ok(&Seen { stopped: Some("SIGTERM".into()), bye: true, ..Seen::default() })), "the server stopped (SIGTERM)");
        assert_eq!(leave_reason(None, Ok(&Seen { bye: true, ..Seen::default() })), "the server ended this session (detach, takeover or disconnect)");
        assert_eq!(leave_reason(None, Ok(&Seen::default())), "the connection closed (a local client is not told why; the server's line says)");
        let boom = std::io::Error::other("boom");
        assert_eq!(leave_reason(None, Err(&boom)), "error: boom");
        let hup = "SIGHUP from the kernel (the terminal hung up)";
        assert_eq!(leave_reason(Some(hup.into()), Err(&boom)), hup, "the hangup caused the error");
    }

    #[test]
    fn the_screen_thread_writes_nothing_once_the_tui_is_down() {
        use std::io::Write as _;
        assert!(!super::TUI_UP.load(std::sync::atomic::Ordering::SeqCst), "no test brings the TUI up");
        assert_eq!(super::Screen.write_all(b"\x1b[Hframe").unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn the_graphics_reply_is_read_up_to_the_da1_answer() {
        let ok = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22;52c";
        assert_eq!(graphics_reply(ok), Some(true));
        assert_eq!(graphics_reply(b"\x1b[?1;2c"), Some(false), "DA1 only: no image support");
        assert_eq!(graphics_reply(b"\x1b_Gi=31;ENOTSUPPORTED:no\x1b\\\x1b[?62c"), Some(false));
        for cut in 0..ok.len() {
            assert_eq!(graphics_reply(&ok[..cut]), None, "split at {cut}: wait for more");
        }
        assert_eq!(graphics_reply(b"\x1b[?62;22"), None, "DA1 not finished");
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::{alone_on_the_session, app_bundle};

    #[test]
    fn the_offer_needs_the_session_to_itself() {
        assert!(alone_on_the_session("52347\n"));
        assert!(!alone_on_the_session("52347\n55347\n"), "two clients share this session");
        assert!(!alone_on_the_session(""), "a detached session has no screen to move");
    }

    #[test]
    fn only_terminals_nerd_can_drive_come_back() {
        assert_eq!(
            app_bundle("/Applications/Ghostty.app/Contents/MacOS/ghostty").unwrap().to_str().unwrap(),
            "/Applications/Ghostty.app"
        );
        assert!(app_bundle("/Users/x/Applications/kitty.app/Contents/MacOS/kitty").is_some());
        assert!(app_bundle("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal").is_none(), "Terminal.app ignores -e");
        assert!(app_bundle("/usr/bin/login").is_none());
        assert!(app_bundle("/Applications/Xcode.app/Contents/MacOS/Xcode").is_none(), "not a terminal");
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::proto::{list_remote_marker, open_remote_marker, stopped_marker, RemoteInstance, ARMED, BYE};

    fn inst(id: &str, label: &str, running: bool) -> RemoteInstance {
        RemoteInstance { id: id.into(), label: label.into(), running, spaces: 1 }
    }

    #[test]
    fn a_machine_comes_from_config_or_says_why_not() {
        let e = |n: &str, h: &str| crate::config::RemoteEntry { name: n.into(), host: h.into() };
        let remotes = vec![e("studio", "me@studio"), e("bad", "-oProxyCommand=x")];
        assert_eq!(usable_remote(&remotes, "studio").unwrap().host, "me@studio");
        let err = usable_remote(&remotes, "mini").unwrap_err();
        assert_eq!(err, "no machine mini in config.json (known: studio, bad)");
        assert!(usable_remote(&remotes, "bad").unwrap_err().contains("cannot start with '-'"));
        assert!(usable_remote(&[], "x").unwrap_err().contains("known: none"));
    }

    #[test]
    fn the_instance_is_the_one_asked_for_or_the_only_one_running() {
        let list = vec![inst("1", "main", true), inst("2", "2", false), inst("3", "side", true)];
        assert_eq!(pick_instance(&list, Some("1"), "studio").unwrap().id, "1");
        assert_eq!(pick_instance(&list, Some("side"), "studio").unwrap().id, "3", "by label");
        assert_eq!(pick_instance(&list, Some("2"), "studio").unwrap_err(), stopped_there("studio", "2"));
        let none = pick_instance(&list, Some("9"), "studio").unwrap_err();
        assert!(none.starts_with("studio has no server 9\n  nerd --remote studio 1"), "{none}");
        let several = pick_instance(&list, None, "studio").unwrap_err();
        assert!(several.contains("nerd --remote studio 1") && several.contains("nerd --remote studio 3") && !several.contains("studio 2"), "{several}");
        assert_eq!(pick_instance(&list[..2], None, "studio").unwrap().id, "1");
        assert!(pick_instance(&list[1..2], None, "studio").unwrap_err().starts_with("no server runs on studio"));
    }

    #[test]
    fn hostile_output_never_becomes_an_id_or_a_label() {
        let json = br#"{"relay":1,"instances":[{"id":"1","label":"main","running":true,"spaces":2},
            {"id":"../x","label":"a","running":true},{"id":"-oX","label":"a","running":true},
            {"id":"2","label":"a\u001b]2;x","running":false}]}"#;
        assert_eq!(parse_listing(json, "studio").unwrap(), vec![RemoteInstance { id: "1".into(), label: "main".into(), running: true, spaces: 2 }]);
        let old = parse_listing(b"\x1b[1mLast login\x1b[0m\nnerd 0.5\n", "studio").unwrap_err();
        assert_eq!(old, "nerd on studio is older than this one, or its shell startup prints text: [1mLast login[0m");
        assert_eq!(parse_listing(b"", "studio").unwrap_err(), "nerd on studio is too old for --remote; update it there");
        assert!(parse_listing(br#"{"relay":2,"instances":[]}"#, "studio").unwrap_err().ends_with("update nerd here"));
        assert!(parse_listing(br#"{"relay":0,"instances":[]}"#, "studio").unwrap_err().ends_with("update nerd on studio"));
    }

    #[test]
    fn every_end_of_a_session_has_its_words() {
        let list = vec![inst("1", "1", true)];
        let out = |seen: Seen, exit: Option<i32>, err: &str| remote_outcome("studio", "1", &seen, exit, err, &list);
        let said = |seen: Seen, exit: Option<i32>, err: &str| match out(seen, exit, err) {
            Outcome::End(msg) => msg,
            o => panic!("words expected, got {o:?}"),
        };
        let armed = || Seen { bytes: true, armed: true, ..Seen::default() };
        assert_eq!(out(Seen { bye: true, ..armed() }, Some(0), ""), Outcome::Quiet, "a detach or a takeover says nothing");
        assert_eq!(said(Seen { stopped: Some("SIGTERM".into()), ..armed() }, Some(0), ""), "the server on studio stopped (SIGTERM)");
        assert_eq!(out(armed(), Some(255), ""), Outcome::Lost);
        assert_eq!(out(armed(), None, ""), Outcome::Lost, "ssh killed by a signal is a loss too");
        assert!(lost_message("studio", "1").starts_with("the connection to studio was lost. The panes keep running there."));
        assert_eq!(said(armed(), Some(0), ""), "the server on studio ended unexpectedly");
        assert_eq!(said(Seen { bytes: true, ..Seen::default() }, Some(0), ""), "the session on studio ended");
        assert_eq!(said(Seen::default(), Some(3), ""), stopped_there("studio", "1"));
        assert!(said(Seen::default(), Some(4), "").starts_with("studio has no server 1"));
        assert_eq!(said(Seen::default(), Some(255), "x\nssh: connect to host s port 22: Operation timed out\n"), "ssh to studio timed out");
        assert_eq!(said(Seen::default(), Some(255), "Permission denied (publickey).\n"), "ssh to studio failed: Permission denied (publickey).");
        assert!(said(Seen::default(), Some(127), "").starts_with("studio has no nerd"));
    }

    #[test]
    fn the_reconnect_screen_counts_and_its_buttons_are_where_they_are_drawn() {
        use ratatui::layout::Position;
        let (lines, reconnect, close) = reconnect_layout("studio", 12, None, 24);
        assert_eq!(lines, ["nerd: the link to studio dropped 12 s ago. The panes keep running there."]);
        assert_eq!((reconnect.y, reconnect.width, close.width), (3, RECONNECT.len() as u16, CLOSE.len() as u16));
        assert_eq!(reconnect.right() + 1, close.x, "one cell between them, as in a dialog");
        for x in reconnect.x..reconnect.right() {
            assert!(reconnect.contains(Position::new(x, 3)) && !close.contains(Position::new(x, 3)));
        }
        assert!(!reconnect.contains(Position::new(reconnect.right(), 3)) && !close.contains(Position::new(reconnect.right(), 3)), "the gap is neither");
        assert!(!reconnect.contains(Position::new(reconnect.x, 2)), "the row above is not a button");

        // A failure's own words sit above the buttons, which move down under them.
        let (lines, again, _) = reconnect_layout("studio", 70, Some("ssh to studio failed\n  second"), 24);
        assert_eq!(lines[0], "nerd: the link to studio dropped 70 s ago. The panes keep running there.");
        assert_eq!(&lines[1..], ["", "ssh to studio failed", "  second"]);
        assert_eq!(again.y, RECONNECT_TOP + 4 + 1);

        // A long failure is cut, so the buttons stay on screen.
        let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
        for rows in [6, 12, 24] {
            let (lines, reconnect, close) = reconnect_layout("studio", 1, Some(&long), rows);
            assert_eq!(lines.last().map(String::as_str), Some("\u{2026}"), "{rows} rows: the cut is shown");
            assert!(reconnect.y < rows && close.y == reconnect.y, "{rows} rows: buttons at {} are on screen", reconnect.y);
        }
    }

    /// This test binary hard-linked as `sshd-session` once per build: macOS checks each new path it launches, up to 10 s under load.
    fn stand_in() -> &'static Path {
        static LINK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        LINK.get_or_init(|| {
            use std::os::unix::fs::MetadataExt as _;
            let me = std::env::current_exe().unwrap();
            let dir = std::env::temp_dir().join("nerd-remote-stand-in");
            let link = dir.join("sshd-session");
            let ino = |p: &Path| std::fs::metadata(p).map(|m| (m.dev(), m.ino())).ok();
            if ino(&link) != ino(&me) {
                std::fs::create_dir_all(&dir).unwrap();
                // Built under its own name, then renamed over: a copy onto a link another build shares would write into its binary.
                let tmp = dir.join(format!("sshd-session.{}", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                std::fs::hard_link(&me, &tmp).or_else(|_| std::fs::copy(&me, &tmp).map(|_| ())).unwrap();
                std::fs::rename(&tmp, &link).unwrap();
            }
            // Pays the launch check here, once, rather than inside a test's deadline.
            let _ = std::process::Command::new(&link).arg("--list").output();
            link
        })
    }

    fn processes_in(dir: &Path) -> String {
        let out = std::process::Command::new("pgrep").arg("-fl").arg(format!("{}/", dir.display())).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A test's own directory: the fake ssh links to it, and it keeps the mode, the list and the argv log.
    struct Fake {
        dir: PathBuf,
    }

    impl Fake {
        fn new(tag: &str, mode: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("nerd-remote-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("state/instances/1")).unwrap();
            let nerd = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("nerd");
            assert!(nerd.exists(), "the relay is the real binary: {} is built by cargo test (tests/remote.rs)", nerd.display());
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ssh");
            std::os::unix::fs::symlink(fixture, dir.join("ssh")).unwrap();
            std::os::unix::fs::symlink(stand_in(), dir.join("sshd-session")).unwrap();
            std::fs::write(dir.join("stand-in"), "").unwrap();
            std::os::unix::fs::symlink(nerd, dir.join("nerd")).unwrap();
            std::fs::write(dir.join("mode"), mode).unwrap();
            Fake { dir }
        }

        fn ssh(&self) -> PathBuf {
            self.dir.join("ssh")
        }

        fn entry(&self) -> crate::config::RemoteEntry {
            crate::config::RemoteEntry { name: "studio".into(), host: "fake-host".into() }
        }

        /// Each call's argv, in order.
        fn calls(&self) -> Vec<Vec<String>> {
            let log = std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default();
            log.split("=====\n").filter(|c| !c.is_empty()).map(|c| c.lines().map(String::from).collect()).collect()
        }

        /// Processes still running from this test's directory: the fake, its sshd-session and the relay.
        fn leftovers(&self) -> String {
            processes_in(&self.dir)
        }

        /// A stand-in server on the relay's socket: returns the peer's own name, `peer_program`'s, and the Hello.
        fn serve(&self, script: impl FnOnce(&mut UnixStream) + Send + 'static) -> std::thread::JoinHandle<(Option<String>, Option<String>, ClientMsg)> {
            let l = std::os::unix::net::UnixListener::bind(self.dir.join("state/nerd.sock")).unwrap();
            l.set_nonblocking(true).unwrap();
            let dir = self.dir.clone();
            std::thread::spawn(move || {
                // The relay's own end, written by the fake's sshd after it; a pgrep for its processes failed this test under load.
                let deadline = Instant::now() + Duration::from_secs(300);
                let mut s = loop {
                    if let Ok((s, _)) = l.accept() {
                        break s;
                    }
                    if dir.join("ended").exists() {
                        match l.accept() {
                            Ok((s, _)) => break s,
                            Err(e) => panic!("the relay ended without connecting: {e}"),
                        }
                    }
                    assert!(Instant::now() < deadline, "the relay never connected in 300 s");
                    std::thread::sleep(Duration::from_millis(20));
                };
                s.set_nonblocking(false).unwrap();
                let pid = nix::sys::socket::getsockopt(&s, nix::sys::socket::sockopt::LocalPeerPid).unwrap();
                let (own, program) = (crate::transport::proc_name(pid), crate::transport::peer_program(&s));
                let hello = ClientMsg::read_from(&mut BufReader::new(s.try_clone().unwrap())).unwrap().unwrap();
                script(&mut s);
                (own, program, hello)
            })
        }

        /// The client's side, with the terminal left out: Hello in, the stream pumped, ssh's end read.
        fn run(&self, id: &str) -> (Seen, Vec<u8>, Option<i32>, String) {
            let Relay { child, mut stdin, stdout, stderr } = spawn_relay(&self.ssh(), "fake-host", id).unwrap();
            // As the real client: a relay that fails at once may close the pipe before Hello.
            let _ = hello().write_to(&mut stdin);
            let mut screen = Vec::new();
            let seen = pump(Vec::new(), stdout, &mut screen, |_| {});
            drop(stdin);
            let (exit, said) = finish(child, stderr);
            (seen, screen, exit, said)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn hello() -> ClientMsg {
        ClientMsg::Hello {
            cwd: PathBuf::new(),
            cols: 80,
            rows: 24,
            mux: false,
            screen: false,
            graphics: Some(false),
            terminal: None,
            opened_by_nerd: false,
            opens_windows: false,
            dark: None,
            host: None,
            bye: true,
            connects: false,
            reattach: Some("nerd --remote studio 1".into()),
            machine: Some("studio".into()),
        }
    }

    fn outcome(seen: &Seen, exit: Option<i32>, said: &str) -> Outcome {
        remote_outcome("studio", "1", seen, exit, said, &[inst("1", "1", true)])
    }

    #[test]
    fn a_list_call_fails_in_words_and_passes_nothing_of_ours_to_the_host() {
        let fake = Fake::new("list", "json");
        std::fs::write(
            fake.dir.join("list.json"),
            r#"{"relay":1,"instances":[{"id":"1","label":"main","running":true,"spaces":2},{"id":"../x","label":"x","running":true}]}"#,
        )
        .unwrap();
        let list = list_remote(&fake.ssh(), &fake.entry(), None).unwrap();
        assert_eq!(list.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["1"]);
        list_remote(&fake.ssh(), &fake.entry(), Some(Duration::from_secs(5))).unwrap();
        let cmd = format!("{REMOTE_PATH}; nerd --list --json");
        let base = ["-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none"];
        let calls = fake.calls();
        assert_eq!(calls[0], [&base[..], &["--", "fake-host", &cmd]].concat());
        assert_eq!(calls[1], [&base[..], &["-n", "-o", "BatchMode=yes", "--", "fake-host", &cmd]].concat(), "the menu's call cannot prompt");

        for (mode, want) in [
            ("refused", "ssh to studio failed: ssh: connect to host fake-host port 22: Connection refused"),
            ("timeout", "ssh to studio timed out"),
            ("nonerd", "studio has no nerd (looked in PATH, ~/.local/bin, /opt/homebrew/bin, /usr/local/bin)"),
            ("garbage", "nerd on studio is older than this one, or its shell startup prints text: Last login: yesterday"),
        ] {
            std::fs::write(fake.dir.join("mode"), mode).unwrap();
            let (ssh_failed, said) = list_remote(&fake.ssh(), &fake.entry(), None).unwrap_err();
            assert_eq!(said, want, "{mode}");
            assert_eq!(ssh_failed, matches!(mode, "refused" | "timeout"), "{mode}: only ssh's own exit offers a terminal");
        }
    }

    #[test]
    fn a_menu_list_call_that_hangs_is_killed_and_named() {
        let fake = Fake::new("hang", "hang");
        let started = Instant::now();
        let (ssh_failed, said) = list_remote(&fake.ssh(), &fake.entry(), Some(Duration::from_secs(1))).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3), "returned in {:?}", started.elapsed());
        assert_eq!(said, "ssh to studio did not answer in 1 s");
        assert!(!ssh_failed, "no terminal to offer: ssh did not refuse, it went quiet");
        // under load the kill can land before the fake writes its pid; then it died sooner still
        if let Ok(pid) = std::fs::read_to_string(fake.dir.join("hang.pid")) {
            let pid: i32 = pid.trim().parse().unwrap();
            assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(), "the hung ssh {pid} is gone");
        }
    }

    #[test]
    fn a_detach_ends_quietly_and_the_server_sees_ssh() {
        let fake = Fake::new("bye", "relay");
        let server = fake.serve(|s| {
            s.write_all(ARMED).unwrap();
            s.write_all(b"frame").unwrap();
            s.write_all(BYE).unwrap();
        });
        let (seen, screen, exit, said) = fake.run("1");
        let (own, program, hello) = server.join().unwrap();
        assert_eq!(outcome(&seen, exit, &said), Outcome::Quiet, "{seen:?} {exit:?} {said}");
        assert_eq!(screen, b"frame", "markers never reach the screen");
        assert_eq!(own.as_deref(), Some("nerd"), "the relay itself is nerd");
        assert_eq!(program.as_deref(), Some("sshd-session"), "its parent says over ssh, found by the walk");
        let ClientMsg::Hello { bye, connects, terminal, opens_windows, reattach, .. } = hello else { panic!("hello expected") };
        assert_eq!((bye, connects, terminal, opens_windows), (true, false, None, false));
        assert_eq!(reattach.as_deref(), Some("nerd --remote studio 1"));
        let cmd = format!("{REMOTE_PATH}; nerd --relay 1");
        let relay = [
            "-T", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ControlMaster=no",
            "-o", "ControlPath=none", "-o", "ClearAllForwardings=yes", "--", "fake-host", &cmd,
        ];
        assert_eq!(fake.calls()[0], relay);
        assert_eq!(fake.leftovers(), "", "nothing of the session is left running");
        let log = std::fs::read_to_string(fake.dir.join("state/daemon.log")).unwrap();
        assert!(log.contains("INFO  client[") && log.contains("] started as a relay over ssh\n"), "{log}");
        assert_eq!(log.matches("] left: ").count(), 1, "{log}");
    }

    /// The fake ssh starts this binary as `sshd-session`; here it runs the relay as its child.
    #[test]
    fn sshd_stand_in() {
        let Some(relay) = std::env::var_os("NERD_FAKE_SSHD") else { return };
        use std::os::fd::FromRawFd as _;
        // SAFETY: the fake ssh opened 3 and 4 for this process: the session's stdin and stdout.
        let (input, output) = unsafe { (std::fs::File::from_raw_fd(3), std::fs::File::from_raw_fd(4)) };
        let id = std::env::var("NERD_FAKE_ID").unwrap_or_default();
        let status = std::process::Command::new(relay).arg("--relay").arg(id).stdin(input).stdout(output).status();
        if let Some(ended) = std::env::var_os("NERD_FAKE_ENDED") {
            std::fs::write(ended, "").unwrap();
        }
        std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(255));
    }

    #[test]
    fn a_click_crosses_the_relay() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let fake = Fake::new("click", "relay");
        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 31, row: 0, modifiers: KeyModifiers::NONE };
        let server = fake.serve(move |s| {
            s.write_all(ARMED).unwrap();
            let got = ClientMsg::read_from(&mut BufReader::new(s.try_clone().unwrap())).unwrap();
            assert!(matches!(got, Some(ClientMsg::Event(Event::Mouse(m))) if m == click), "{got:?}");
            s.write_all(BYE).unwrap();
        });
        let Relay { child, mut stdin, mut stdout, stderr } = spawn_relay(&fake.ssh(), "fake-host", "1").unwrap();
        hello().write_to(&mut stdin).unwrap();
        let mut first = vec![0u8; 64];
        let n = stdout.read(&mut first).unwrap();
        first.truncate(n);
        // What the input loop writes for every event it reads, a click included.
        ClientMsg::Event(Event::Mouse(click)).write_to(&mut stdin).unwrap();
        let seen = pump(first, stdout, &mut Vec::new(), |_| {});
        drop(stdin);
        let (exit, said) = finish(child, stderr);
        server.join().unwrap();
        assert_eq!(outcome(&seen, exit, &said), Outcome::Quiet, "{seen:?} {exit:?} {said}");
    }

    #[test]
    fn a_remote_client_serves_no_connect_requests() {
        let mut data = list_remote_marker("studio");
        data.extend_from_slice(&open_remote_marker("studio", Some("1")));
        let r = take_requests(&data, &mut Vec::new());
        let (list, connect) = connect_asks(&r, true);
        assert_eq!((list.len(), connect.len()), (1, 1));
        let (list, connect) = connect_asks(&r, false);
        assert!(list.is_empty() && connect.is_empty(), "a server on another Mac cannot make this one ssh or open windows");
    }

    #[test]
    fn a_stop_a_crash_and_an_old_server_are_told_apart() {
        type Script = fn(&mut UnixStream);
        let cases: [(&str, Script, &str); 3] = [
            ("stopped", |s| s.write_all(&[ARMED, &stopped_marker("SIGTERM")].concat()).unwrap(), "the server on studio stopped (SIGTERM)"),
            ("crashed", |s| s.write_all(ARMED).unwrap(), "the server on studio ended unexpectedly"),
            ("old", |s| s.write_all(b"frame").unwrap(), "the session on studio ended"),
        ];
        for (tag, script, want) in cases {
            let fake = Fake::new(tag, "relay");
            let server = fake.serve(script);
            let (seen, _, exit, said) = fake.run("1");
            server.join().unwrap();
            assert_eq!(exit, Some(0), "{tag}: the relay ends cleanly on the socket's end");
            assert_eq!(outcome(&seen, exit, &said), Outcome::End(want.into()), "{tag}");
        }
    }

    #[test]
    fn a_link_that_dies_after_the_arming_is_a_loss() {
        let fake = Fake::new("lost", "lost");
        let server = fake.serve(|s| {
            s.write_all(ARMED).unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let (seen, _, exit, said) = fake.run("1");
        assert_eq!(exit, Some(255));
        assert_eq!(outcome(&seen, exit, &said), Outcome::Lost, "the client offers to reconnect instead of exiting");
        server.join().unwrap();
    }

    #[test]
    fn a_relay_that_fails_before_any_byte_says_why() {
        let fake = Fake::new("early", "relay");
        let (seen, _, exit, said) = fake.run("1");
        assert_eq!((exit, outcome(&seen, exit, &said)), (Some(3), Outcome::End(stopped_there("studio", "1"))), "nothing listens: stopped");
        let (seen, _, exit, said) = fake.run("9");
        assert_eq!(exit, Some(4));
        let msg = remote_outcome("studio", "9", &seen, exit, &said, &[inst("1", "1", true)]);
        assert_eq!(msg, Outcome::End("studio has no server 9\n  nerd --remote studio 1   (running)".into()));
        assert!(!fake.dir.join("state/nerd.sock").exists(), "the relay starts no server");
        let log = std::fs::read_to_string(fake.dir.join("state/daemon.log")).unwrap();
        assert!(log.contains("] left: the server is not running\n"), "{log}");
    }

    #[test]
    fn a_client_that_dies_takes_its_ssh_with_it() {
        let fake = Fake::new("died", "relay");
        // Like the real server: it closes once the client's side ends.
        let server = fake.serve(|s| {
            s.write_all(ARMED).unwrap();
            let _ = std::io::copy(s, &mut std::io::sink());
        });
        let Relay { mut child, mut stdin, mut stdout, stderr: _ } = spawn_relay(&fake.ssh(), "fake-host", "1").unwrap();
        hello().write_to(&mut stdin).unwrap();
        let mut buf = [0u8; 64];
        assert!(stdout.read(&mut buf).unwrap() > 0);
        // What a SIGHUP, a SIGKILL or a panic leaves: both pipes closed, nobody waiting.
        drop((stdin, stdout));
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(child.try_wait().unwrap().is_some(), "ssh is gone");
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fake.leftovers().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fake.leftovers(), "");
    }

    #[test]
    fn the_relay_closes_its_socket_when_ssh_input_ends_and_waits_for_nothing() {
        let fake = Fake::new("input-ended", "relay");
        let (eof, saw_eof) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        // A server that reads to the end, then keeps its side open: the relay must not wait on it.
        let server = fake.serve(move |s| {
            s.write_all(ARMED).unwrap();
            let _ = std::io::copy(s, &mut std::io::sink());
            eof.send(()).unwrap();
            let _ = held.recv_timeout(Duration::from_secs(30));
        });
        let Relay { mut child, mut stdin, mut stdout, stderr: _ } = spawn_relay(&fake.ssh(), "fake-host", "1").unwrap();
        hello().write_to(&mut stdin).unwrap();
        let mut buf = [0u8; 64];
        assert!(stdout.read(&mut buf).unwrap() > 0);
        drop(stdin);
        saw_eof.recv_timeout(Duration::from_secs(10)).expect("the server saw the relay's end");
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let ended = child.try_wait().unwrap();
        release.send(()).unwrap();
        server.join().unwrap();
        assert!(ended.is_some(), "ssh ended while the server still held its side open");
        let log = std::fs::read_to_string(fake.dir.join("state/daemon.log")).unwrap();
        assert!(log.contains("] left: ssh's input ended (the client left or the link dropped)\n"), "{log}");
    }

    #[test]
    fn a_refusal_is_read_back_and_an_accepted_request_is_not_one() {
        use std::io::Write as _;
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        theirs.write_all(b"refused: stop from inside a pane\n").unwrap();
        assert_eq!(super::refusal(&ours).as_deref(), Some("refused: stop from inside a pane"));
        drop(theirs);
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        drop(theirs);
        assert_eq!(super::refusal(&ours), None, "EOF: the server took it");

        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut r = BufReader::new(theirs.try_clone().unwrap());
            let _ = ClientMsg::read_from(&mut r);
            let _ = (&theirs).write_all(b"refused: disconnect from inside a pane\n");
        });
        assert_eq!(super::ask_to_disconnect(ours), Err(Some("refused: disconnect from inside a pane".to_string())));
    }

    #[test]
    fn a_refused_attach_is_kept_off_the_screen_for_the_caller_to_say() {
        let mut screen = Vec::new();
        let seen = super::pump(b"\x1b[?1049l\r\nrefused: attach from inside a pane\r\n\x1b[?1049h".to_vec(), std::io::empty(), &mut screen, |_| {});
        assert_eq!(seen.refused.as_deref(), Some("refused: attach from inside a pane"));
        assert!(screen.is_empty() && !seen.bytes);
        let seen = super::pump(b"frame refused: nothing".to_vec(), std::io::empty(), &mut Vec::new(), |_| {});
        assert!(seen.refused.is_none());
    }
}
