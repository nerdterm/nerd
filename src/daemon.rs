//! Server: owns the panes and lives until stopped; ratatui draws to a socket instead of stdout, since the daemon has no tty.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use crossterm::event::Event;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::{TerminalOptions, Viewport};

use crate::app::App;
use crate::log::{debug, error, info, log, warn, Level, HYGIENE};
use crate::session::socket_path;
use crate::transport::{accept_loop, peer_program, read_client, Inbound, Inbox, Peer, Sink, Waiting};

/// No screen is bigger than this; the ratatui buffer is cols x rows cells.
const MAX_DIM: u16 = 1000;

fn shut_down(app: &mut App, sink: &Sink, path: &std::path::Path, reason: &str) {
    // Only the kind of stop: an error or a panic message is no business of the other Mac.
    if app.caps.bye {
        sink.say(&crate::proto::stopped_marker(reason.split(':').next().unwrap_or(reason)));
    }
    app.plugins.stop_all();
    app.dump_screens();
    save_now(app);
    sink.drain(Duration::from_secs(1));
    let level = if reason.starts_with("error:") || reason.starts_with("panic:") { Level::Error } else { Level::Info };
    log(level, &format!("stopped by {reason}"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(crate::session::plugin_socket_path());
    let _ = std::fs::remove_file(crate::session::msock_path());
    crate::isolation::remove_shared();
}

/// Waits for every child that already finished, without blocking on a live one (the adopted broker).
fn reap_finished_children() -> usize {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    let mut n = 0;
    while let Ok(status) = waitpid(nix::unistd::Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
        if status == WaitStatus::StillAlive {
            break;
        }
        n += 1;
    }
    n
}

/// How often the ticks run: tick cadence is what it was when the loop waited 16 ms a turn.
const TICK: Duration = Duration::from_millis(16);
/// The least time between two frames.
const FRAME_GAP: Duration = Duration::from_millis(8);

/// Size from the client: crossterm would fall back to `tput`, which fails for a TERM without terminfo (xterm-kitty).
struct ClientBackend {
    inner: CrosstermBackend<Sink>,
    size: Size,
}

impl Backend for ClientBackend {
    type Error = std::io::Error;
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        Err(std::io::Error::other("the server has no tty to ask for the cursor"))
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<Size> {
        Ok(self.size)
    }
    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        Ok(WindowSize { columns_rows: self.size, pixels: Size::default() })
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

type Tui = ratatui::Terminal<ClientBackend>;

/// ratatui clears the client's screen here (ESC[2J), and a terminal that draws images deletes them
/// with it, so the images are sent again rather than trusted to still be there.
fn resize(tui: &mut Tui, app: &mut App, cols: u16, rows: u16) -> std::io::Result<()> {
    tui.backend_mut().size = Size::new(cols, rows);
    app.images_lost();
    tui.resize(Rect::new(0, 0, cols, rows))
}

/// Room for files: macOS starts a process at 256, which a few dozen panes and plugin connections use up.
fn raise_file_limit() {
    let mut lim = nix::libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: get/setrlimit write and read the one struct passed.
    unsafe {
        if nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < 10240 {
            lim.rlim_cur = lim.rlim_max.min(10240);
            nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &lim);
        }
    }
}

pub fn run() -> std::io::Result<()> {
    let mut started = false;
    serve(&mut started).inspect_err(|e| {
        // The pid tells a second server's line from the running one's.
        if !started {
            error(&format!("server pid {} did not start: {e}", std::process::id()));
        }
    })
}

fn serve(started: &mut bool) -> std::io::Result<()> {
    raise_file_limit();
    let path = socket_path();
    // Only our own state directory: `NERD_SOCKET` can point anywhere, and
    // chmod-ing someone else's folder to 0700 is not our business.
    let state = crate::session::state_dir();
    std::fs::create_dir_all(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    // An update in place keeps the pid, and with it the zombies the old build left; reaped before anything
    // of this build is started, so no status of its own is taken. Children still running at the exec lose
    // their waiter and stay zombies until the next update: a sound or two.
    let reaped = reap_finished_children();
    if let Some(dir) = path.parent().filter(|d| *d != state) {
        std::fs::create_dir_all(dir)?;
    }
    // Set on an in-place update: the previous build exec'd us with its descriptors
    // still open, described by the handover file.
    let handover: Option<crate::session::Handover> = std::env::var_os("NERD_HANDOVER")
        .and_then(|_| std::fs::read(crate::session::handover_path()).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());

    crate::policy::init();
    let plugin_listener;
    let msock_listener;
    let listener = match &handover {
        Some(h) => {
            plugin_listener = crate::plugins::bind(&crate::session::plugin_socket_path());
            msock_listener = crate::msock::bind(&crate::session::msock_path());
            let l = unsafe { UnixListener::from_raw_fd(h.listener_fd) };
            nix::fcntl::fcntl(h.listener_fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
            l
        }
        None => {
            if UnixStream::connect(&path).is_ok() {
                return Err(std::io::Error::other("server already running"));
            }
            // The plugin socket first: whoever sees the main socket may already look for it.
            plugin_listener = crate::plugins::bind(&crate::session::plugin_socket_path());
            msock_listener = crate::msock::bind(&crate::session::msock_path());
            let _ = std::fs::remove_file(&path);
            let l = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            l
        }
    };
    let listener_fd = listener.as_raw_fd();

    let (tx, rx) = channel();
    let (bell, door) = crate::bell::new();
    let inbox = Inbox { tx, bell: bell.clone() };
    let (exit_tx, exits) = channel();
    let (note_tx, notes) = channel();
    let (spawn_tx, spawns) = channel();
    // Ids continue past the client carried over an in-place update: reusing its id would make
    // the next client's own Detach, sent when it replaces that client, shut the new one down.
    let carried = handover.as_ref().and_then(|h| h.client).map(|c| c.0).unwrap_or(0);
    let waiting = Waiting::default();
    accept_loop(listener, inbox.clone(), carried, waiting.clone());
    match plugin_listener {
        Ok(l) => crate::plugins::listen(l, inbox.clone()),
        Err(e) => error(&format!("plugin socket failed: {e}, plugins cannot connect")),
    }
    match msock_listener {
        Ok(l) => crate::msock::start(l, inbox.clone()),
        Err(e) => error(&format!("message socket failed: {e}; agents cannot send")),
    }
    if !crate::policy::current().accounts.is_empty()
        && let Err(e) = crate::isolation::ensure_shared()
    {
        error(&format!("shared message socket: {e}"));
    }
    // Which client owns the screen, so a stale Detach from a replaced client is ignored.
    let mut current: u64 = 0;

    match crate::config::migrate_prefix() {
        Ok(true) => info("prefix: a stored off became Control + B, the new default; Settings turns it off"),
        Ok(false) => {}
        Err(e) => warn(&format!("prefix: the one-time switch to Control + B failed: {e}")),
    }
    let (cfg, broken) = crate::config::load_checked();
    if broken {
        warn("config.json failed to parse, using defaults");
    }

    // Broker before sandbox: Seatbelt is inherited by children, so a helper
    // started after it would leave agents in the panes with no network.
    match &handover {
        Some(h) => {
            let beside = note_tx.clone();
            if let Err(e) = crate::broker::adopt(h.broker_fd, h.broker_pid, h.broker_protocol, exit_tx, note_tx, spawn_tx, bell.clone()) {
                error(&format!("broker socket did not carry over: {e}, panes will not launch"));
            } else if h.broker_protocol < 1 {
                info("plugins: the broker is from an older build; starting one of this build beside it");
                if let Err(e) = crate::broker::start_beside(beside, bell.clone()) {
                    warn(&format!("plugins off: the broker for plugins did not start: {e}; {}", crate::plugins::OLD_BROKER));
                }
            }
            // Seatbelt survives exec, no need to sandbox_init again.
            info("sandbox: inherited from the previous build");
        }
        None => {
            if let Err(e) = crate::broker::start(exit_tx, note_tx, spawn_tx, bell.clone()) {
                error(&format!("broker failed to start: {e}, panes will not launch"));
            }
            let sandbox = enter_sandbox(cfg.sandbox);
            log(if sandbox.starts_with("did NOT") { Level::Warn } else { Level::Info }, &format!("sandbox: {sandbox}"));
            record_sandbox(&sandbox);
        }
    }

    let mut app = App::with_bell(cfg, bell.clone());
    app.waiting = waiting.clone();
    app.plugins.autostart = true;
    if broken {
        app.mark_cfg_broken();
    }
    // Before restore consumes the screen files, so a SIGTERM in that window still dumps them.
    crate::signals::install();
    // Done at startup, not on shutdown: the Time Machine exclusion tmutil sets on
    // this directory can take ~11s, which shutdown can't afford to wait on.
    if app.cfg.restore_screens
        && let Err(e) = crate::session::ensure_screens_dir()
    {
        error(&format!("screens: directory failed: {e}"));
    }
    match &handover {
        Some(h) => {
            if !app.adopt(h) {
                warn("handover: no pane came over");
            }
            app.updated_changes = (!h.from.is_empty()).then(|| crate::changes::since(&h.from));
            crate::session::clear_handover();
        }
        None => {
            if !app.restore() && crate::session::state_path().exists() {
                // Snapshot exists but didn't restore, most likely unparsable; say so
                // or the user just sees an empty desktop with no explanation.
                error("session snapshot did not restore: file exists but is empty or unparsable");
            }
        }
    }
    app.mail = crate::app::MailState::open(&crate::mail::path());

    let sink = Sink::new(bell.clone());
    let server = crate::session::instance_name();
    let mut untouched = crate::servers::untouched(&server);
    // Fixed viewport: the daemon has no tty, so Fullscreen's terminal::size() call would fail. The client supplies the size instead.
    let mut tui = ratatui::Terminal::with_options(
        ClientBackend { inner: CrosstermBackend::new(sink.clone()), size: Size::new(80, 24) },
        TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)) },
    )?;
    // NERD_TRACE=1: log client events and suspicious pane output, for tracking down terminal replies leaking into panes.
    let trace = std::env::var_os("NERD_TRACE").is_some();
    app.trace = trace;
    info(&format!("server started, id {}", std::env::var("NERD_INSTANCE").unwrap_or_else(|_| "-".into())));
    *started = true;
    if reaped > 0 {
        info(&format!("reaped {reaped} finished child processes left from before"));
    }
    crate::battery::start();
    // The build that runs owns the key map and the colours, so it rewrites the applied presets' files; an update
    // in place carries its clients over, and a client start is too late for a map that changed under them.
    // ceiling: at start only; a theme changed later reaches the terminal at the next start.
    let keys_changed = match crate::presets::refresh(&app.cfg, || crate::client::appearance_dark().unwrap_or(true)) {
        Ok(changed) => {
            if changed {
                info("presets: Ghostty's file rewritten for this build");
            }
            changed
        }
        Err(e) => {
            warn(&format!("presets: rewriting nerd's files failed: {e}"));
            false
        }
    };

    let mut first = false;
    if let Some((id, fd, cols, rows, mux)) = handover.as_ref().and_then(|h| h.client) {
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        let peer = Peer::of(&stream);
        app.client_inside = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerPid)
            .is_ok_and(|pid| crate::transport::inside(pid, crate::broker::pid(), crate::transport::parent_pid));
        if let Ok(reader) = stream.try_clone() {
            let inbox = inbox.clone();
            std::thread::spawn(move || read_client(id, reader, inbox, true, Waiting::default(), crate::transport::allow_from_panes));
        }
        current = id;
        app.client = id;
        app.caps = crate::proto::ClientCaps { mux, ..handover.as_ref().map(|h| h.caps.clone()).unwrap_or_default() };
        app.apply_theme();
        app.graphics = crate::proto::graphics_or_guess(app.caps.graphics);
        sink.set(Some(stream));
        // This build knows the markers too, so the carried client learns it again.
        if app.caps.bye {
            sink.say(crate::proto::ARMED);
        }
        lose_on_error(resize(&mut tui, &mut app, cols, rows), &mut app, &sink, id);
        app.screen = Rect::new(0, 0, cols, rows);
        app.toast_with_more(format!("Updated to {}", crate::changes::short_version()), crate::app::More::Updated);
        warn_sandbox(&mut app);
        if keys_changed {
            app.to_client.extend_from_slice(crate::proto::RELOAD_KEYS);
        }
        first = true;
        let panes: usize = app.spaces.iter().flat_map(|s| s.tabs.iter()).map(|t| t.panes.len()).sum();
        info(&format!("handover: client {id} carried over, {panes} panes, graphics={}, {peer}", app.graphics));
    }
    drop(handover);
    let exe = std::env::current_exe()?;
    let mut exe_id = file_id(&exe);
    let mut exe_checked = Instant::now();
    let mut probe = Probe::default();
    let mut ticked = Instant::now() - TICK;
    let mut drawn = Instant::now() - FRAME_GAP;
    // Loop state, not per turn: a frame held back by the gate, and a change not yet in a snapshot.
    let mut owed = false;
    let mut unsaved = true;
    let mut wait = Duration::ZERO;
    // NERD_TRACE: frames per second, to check the gate holds under a flood.
    let (mut frames, mut frames_at) = (0u32, Instant::now());
    // Test-only: forces a panic on the first tick to exercise the panic exit path.
    let panic_test = std::env::var_os("NERD_PANIC_TEST").is_some();
    // Wall clock, not Instant: Instant stops while the Mac sleeps.
    let (mut had_window, mut alone_since) = (sink.attached(), None::<u64>);
    // The window found dead at the next attach, whose detach line is still to come.
    let mut late = None::<u64>;

    // A closure so catch_unwind below can still dump/snapshot/log on panic;
    // it only borrows `app`, which stays reachable after.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::io::Result<String> {
    loop {
        door.wait(wait);
        if panic_test {
            panic!("NERD_PANIC_TEST");
        }
        let sig = crate::signals::caught();
        if sig != 0 {
            return Ok(format!("{}: {}", crate::signals::name(sig), crate::signals::sender()));
        }
        let mut redraw = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Inbound::Attach { id, stream, cwd, cols, rows, host, program, caps, inside, if_free } => {
                    // Here, not on the reader's thread: two `if_free` attaches cannot both find the server free.
                    if if_free && sink.attached() && !sink.hung_up() {
                        info(&format!("client {id} refused: it asked for a free server and client {current} shows this one, {}", Peer::of(&stream)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let _ = (&stream).write_all(crate::transport::farewell_text(crate::proto::TAKEN).as_bytes());
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    app.client_inside = inside;
                    // A window that died just before this attach is not taken over: its own detach line follows.
                    if sink.hung_up() {
                        crate::clients::close(&mut app.clients, current, crate::clients::End::Closed, crate::clients::now());
                        late = Some(current);
                    }
                    crate::clients::attach(
                        &mut app.clients,
                        crate::clients::ClientRecord { id, since: crate::clients::now(), host, program, ended: None },
                    );
                    // Kept, not dropped: the farewell names the way back of the client leaving.
                    let old = std::mem::replace(&mut app.caps, caps);
                    // a new terminal: its pointer is its own until the mouse moves
                    app.pointer.clear();
                    app.apply_theme();
                    app.graphics = crate::proto::graphics_or_guess(app.caps.graphics);
                    // A client attached: whatever the offer started, it worked.
                    app.opening = None;
                    // mux decides whether nerd offers to open a window elsewhere, so the log has to say it
                    let after = alone_since.take().map(|t| format!(", first window after {}", gap(crate::clients::now().saturating_sub(t)))).unwrap_or_default();
                    info(&format!(
                        "client {id} attached: {}x{} from {}, graphics={}, mux={}, bye={}, connects={}, {}{after}",
                        cols,
                        rows,
                        cwd.display(),
                        app.graphics,
                        app.caps.mux,
                        app.caps.bye,
                        app.caps.connects,
                        Peer::of(&stream),
                    ));
                    if app.spaces.is_empty() {
                        app.open_space(cwd);
                    }
                    // A session from another Mac does not make this the server a bare `nerd` here comes back to,
                    // and a server one has seen is never discarded.
                    if app.caps.bye {
                        keep(&mut untouched, &server, "a session from another Mac attached");
                    } else if let Err(e) = crate::servers::mark_attached(&server) {
                        warn(&format!("attached stamp not written: {e}"));
                    }
                    // Clamp: 65535x65535 cells would be a four-gigabyte buffer.
                    let (cols, rows) = (cols.clamp(1, MAX_DIM), rows.clamp(1, MAX_DIM));
                    if sink.attached() && late != Some(current) {
                        info(&format!("client {current} taken over by client {id}"));
                    }
                    current = id;
                    app.client = id;
                    if sink.attached() {
                        sink.farewell(&farewell(old.reattach.as_deref(), &crate::session::instance_name()));
                        if old.bye {
                            sink.say(crate::proto::BYE);
                        }
                    }
                    sink.set(Some(stream));
                    if app.caps.bye {
                        sink.say(crate::proto::ARMED);
                    }
                    // Not tui.clear(): it queries the cursor position, and the daemon has no
                    // tty to answer, so it would hang forever. resize() clears the buffer instead.
                    lose_on_error(resize(&mut tui, &mut app, cols, rows), &mut app, &sink, id);
                    app.screen = Rect::new(0, 0, cols, rows);
                    app.offer_outside_tmux(id);
                    app.offer_release_check();
                    warn_sandbox(&mut app);
                    // This build's key map is already on disk; the client is the one that can
                    // have the terminal reread it.
                    if keys_changed {
                        app.to_client.extend_from_slice(crate::proto::RELOAD_KEYS);
                    }
                    redraw = true;
                }
                Inbound::Detach(id, how) if late == Some(id) => {
                    info(&format!("client {id} detached: {how}"));
                    late = None;
                }
                Inbound::Event(id, _) | Inbound::Detach(id, _) | Inbound::Appearance(id, _) | Inbound::CellHeight(id, _) | Inbound::Remote(id, ..) if id != current => {}
                Inbound::Remote(_, name, result, window, ssh_failed) => {
                    app.remote_listed(name, result, window, ssh_failed);
                    redraw = true;
                }
                Inbound::Appearance(_, dark) => {
                    app.caps.dark = Some(dark);
                    app.apply_theme();
                    redraw = true;
                }
                Inbound::CellHeight(_, px) => {
                    app.caps.cell_px = px;
                    redraw = true;
                }
                Inbound::Event(_, Event::Resize(c, r)) => {
                    let (c, r) = (c.clamp(1, MAX_DIM), r.clamp(1, MAX_DIM));
                    lose_on_error(resize(&mut tui, &mut app, c, r), &mut app, &sink, current);
                    app.screen = Rect::new(0, 0, c, r);
                    redraw = true;
                }
                Inbound::Event(_, Event::Key(k)) => {
                    if trace {
                        debug(&trace_line(&Event::Key(k)));
                    }
                    app.key(k);
                    redraw = true;
                }
                Inbound::Event(_, Event::Mouse(m)) => {
                    if trace && matches!(m.kind, crossterm::event::MouseEventKind::Down(_) | crossterm::event::MouseEventKind::Up(_)) {
                        debug(&trace_line(&Event::Mouse(m)));
                    }
                    let screen = app.screen;
                    app.mouse(m, screen);
                    redraw = true;
                }
                Inbound::Event(_, Event::Paste(s)) => {
                    if trace {
                        debug(&trace_line(&Event::Paste(s.clone())));
                    }
                    app.paste(&s);
                    redraw = true;
                }
                Inbound::Event(_, _) => {}
                Inbound::Detach(client, how) => {
                    info(&format!("client {client} detached: {how}"));
                    // A client already lost to an error has no screen left to leave.
                    let left = sink.attached() && !how.starts_with(crate::transport::READ_ERROR);
                    crate::clients::close(&mut app.clients, client, crate::clients::End::Closed, crate::clients::now());
                    if app.caps.bye {
                        sink.say(crate::proto::BYE);
                    }
                    sink.set(None);
                    if left && discardable(&mut untouched, &app, &server) {
                        return Ok(DISCARD.into());
                    }
                }
                Inbound::Shutdown => return Ok("--stop".into()),
                Inbound::Update => app.upgrade = true,
                Inbound::PluginUp { name, id, stream, events } => app.plugins.up(name, id, stream, events, bell.clone()),
                Inbound::PluginSaid { name, id, pane, state } => {
                    if app.plugins.current(&name, id) {
                        app.plugin_report(&name, pane, &state);
                    } else {
                        crate::log::warn_limited("plugin report dropped", &format!("plugin {name}: report for pane {pane} dropped: not from its current connection"));
                    }
                }
                Inbound::PluginSlots { name, id, msgs } => {
                    if app.plugins.current(&name, id) {
                        for m in msgs {
                            app.plugins.slot(&name, m);
                        }
                        redraw = true;
                    }
                }
                Inbound::PluginDown { name, id } => {
                    app.plugins.down(&name, id);
                    redraw = true;
                }
                Inbound::MailHello { pid, uid, started, accepted, reply } => {
                    let found = app.mail_hello(pid, started, accepted);
                    if let Err(why) = &found {
                        crate::log::warn_limited("message socket refused", &format!("refused a message connection from pid {pid}, uid {uid}: {why}"));
                    }
                    let _ = reply.send(found);
                }
                Inbound::Mail { pane, uid, req, reply } => redraw |= app.mail_request(pane, uid, req, reply),
                Inbound::DisconnectAll { id, mut stream } => {
                    let by = if peer_program(&stream).is_some_and(|p| p.starts_with("sshd")) { "a connection over ssh" } else { "a terminal on this Mac" };
                    // `ok` first: an older build closes the socket instead, and the caller tells the two apart.
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                    let _ = stream.write_all(b"ok\n");
                    info(&format!("client {id}: disconnect everyone, from {by}"));
                    let ended = disconnect_everyone(&mut app, &sink, current, &waiting, Some(by));
                    let text = if ended.is_empty() { "nobody was connected\n".to_string() } else { ended.join("\n") + "\n" };
                    let _ = stream.write_all(text.as_bytes());
                    redraw = true;
                }
                Inbound::Probe { mut stream } => {
                    let answer = crate::proto::ProbeAnswer { shown: sink.attached() && !sink.hung_up(), version: crate::version_line(), spaces: app.spaces.len() };
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    if let Ok(line) = serde_json::to_string(&answer) {
                        let _ = stream.write_all(format!("{line}\n").as_bytes());
                    }
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        let (pumped, more) = app.pump_pty(crate::app::PUMP_BUDGET);
        redraw |= pumped;
        redraw |= app.take_results();
        while let Ok(s) = spawns.try_recv() {
            app.spawned(s);
        }
        while let Ok(id) = exits.try_recv() {
            app.exited(id);
        }
        while let Ok(n) = notes.try_recv() {
            app.exec_note(n);
            redraw = true;
        }
        redraw |= app.settle_exits();
        if ticked.elapsed() >= TICK {
            ticked = Instant::now();
            // A newer binary at our own path: say so once, the swap itself is the human's call.
            if exe_checked.elapsed() >= Duration::from_secs(2) {
                exe_checked = Instant::now();
                let now = file_id(&exe);
                if now.is_some() && now != exe_id {
                    exe_id = now;
                    probe.start(&exe, &bell, PROBE_LIMIT);
                }
            }
            if let Some((info, changes)) = probe.take(&exe, &bell, PROBE_LIMIT) {
                app.update_available = true;
                app.update_info = info;
                app.update_changes = changes;
                let skipped = std::fs::read_to_string(crate::paths::state_root().join("update-skipped")).unwrap_or_default();
                app.update_skipped = skipped.trim() == app.update_info;
                if !app.update_skipped {
                    // A standing notice would otherwise hide this card until a key.
                    app.notice = None;
                    app.toast_with_more("Update available".into(), crate::app::More::Update);
                    redraw = true;
                }
            }
            redraw |= app.tick_states();
            redraw |= app.autoscroll_selection();
            redraw |= app.tick_search();
            app.tick_git();
            redraw |= app.tick_clock();
            redraw |= app.tick_battery();
            redraw |= app.tick_opening();
            redraw |= app.tick_spawns();
            redraw |= app.tick_remote();
            app.tick_cfg();
            app.plugins.tick();
            app.tick_release();
            redraw |= app.tick_mail();
        }

        // Before the update: a client handed over would outlive a disconnect asked in the same frame.
        if std::mem::take(&mut app.disconnect_all) {
            info("disconnect everyone, from the menu");
            disconnect_everyone(&mut app, &sink, current, &waiting, None);
            redraw = true;
        }

        if app.stop_server {
            info(&format!("stop asked from the menu of client {current}"));
            return Ok("menu".into());
        }

        // On success, handover_exec never returns: the process image is the new build.
        if std::mem::take(&mut app.upgrade) {
            // A pane with no master yet cannot cross over: its answer is waited for, or it fails.
            while let Some(until) = app.spawn_wait() {
                match spawns.recv_timeout(until.saturating_duration_since(Instant::now())) {
                    Ok(s) => {
                        app.spawned(s);
                    }
                    Err(_) => {
                        std::thread::sleep(until.saturating_duration_since(Instant::now()));
                        app.tick_spawns();
                    }
                }
            }
            // A pane whose process already left must not cross over as if it were alive.
            while let Ok(id) = exits.try_recv() {
                app.exited(id);
            }
            app.settle_exits();
            save_now(&mut app);
            app.plugins.stop_all();
            let e = handover_exec(&mut app, &sink, listener_fd, current);
            error(&format!("update failed: {e}"));
            app.notice = Some(format!("update failed: {e}"));
            app.update_available = false;
            exe_id = file_id(&exe);
            redraw = true;
        }

        // `quit` here means detach, not die: panes must outlive the window.
        if let Some(why) = app.quit.take() {
            info(why);
            let left = sink.attached();
            crate::clients::close(&mut app.clients, current, crate::clients::End::Detached, crate::clients::now());
            if app.caps.bye {
                sink.say(crate::proto::BYE);
            }
            sink.set(None);
            if left && discardable(&mut untouched, &app, &server) {
                return Ok(DISCARD.into());
            }
        }

        let changed = redraw || std::mem::take(&mut app.dirty) || std::mem::take(&mut first);
        if untouched && changed {
            keep_if_touched(&mut untouched, &app, &server);
        }
        save_turn(&mut app, &mut unsaved, changed);

        // OSC 52 goes out between frames as-is; it doesn't move the cursor, so ratatui's buffer doesn't need to know.
        let out = std::mem::take(&mut app.to_client);
        if !out.is_empty() && sink.attached() {
            let _ = sink.clone().write_all(&out).and_then(|_| sink.clone().flush());
        }

        owed = (owed || changed) && sink.attached();
        // A client behind gets no new frame until it has taken the last one; the writer rings at zero.
        // sync_sizes and sync_tree wait with the draw, on purpose: drawing anyway would grow the backlog.
        let behind = sink.backlog() > 0;
        if owed && !behind && drawn.elapsed() >= FRAME_GAP {
            owed = false;
            drawn = Instant::now();
            match draw_turn(&mut tui, &mut app) {
                Ok(fx) => {
                    // After the frame, in the same turn: a Kitty image placed before its frame would be drawn over.
                    let mut out = fx.screen;
                    if let Some(text) = fx.copy {
                        out.extend(crate::select::copy(&text));
                    }
                    if !out.is_empty() && sink.attached() {
                        let _ = sink.clone().write_all(&out).and_then(|_| sink.clone().flush());
                    }
                    if fx.toast.is_some() {
                        app.toast = fx.toast;
                        owed = true;
                    }
                }
                Err(e) => {
                    warn(&format!("frame did not go out: {e}"));
                    crate::clients::close(&mut app.clients, current, crate::clients::End::Lost, crate::clients::now());
                    sink.set(None);
                }
            }
            frames += 1;
            if trace && frames_at.elapsed() >= Duration::from_secs(1) {
                debug(&format!("trace frames: {frames} in {} ms", frames_at.elapsed().as_millis()));
                (frames, frames_at) = (0, Instant::now());
            }
        }
        if had_window && !sink.attached() {
            let panes: usize = app.spaces.iter().flat_map(|s| s.tabs.iter()).map(|t| t.panes.len()).sum();
            info(&format!("no window now; {panes} panes keep running"));
            alone_since = Some(crate::clients::now());
        }
        had_window = sink.attached();
        wait = if more {
            Duration::ZERO
        } else {
            let tick = TICK.saturating_sub(ticked.elapsed());
            if owed && !behind { tick.min(FRAME_GAP.saturating_sub(drawn.elapsed())) } else { tick }
        };
    }
    }));

    match outcome {
        Ok(Ok(reason)) if reason == DISCARD && discard(&mut app, &server) => Ok(()),
        Ok(Ok(reason)) => {
            shut_down(&mut app, &sink, &path, &reason);
            Ok(())
        }
        Ok(Err(e)) => {
            shut_down(&mut app, &sink, &path, &format!("error: {e}"));
            Err(e)
        }
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            shut_down(&mut app, &sink, &path, &format!("panic: {msg}"));
            // abort() over a plain exit: under the LaunchAgent, launchd's
            // KeepAlive.Crashed only restarts on a signal death, not exit 101.
            std::process::abort();
        }
    }
}

/// The loop's reason when the last session left a server nothing was done in.
const DISCARD: &str = "discard: nothing was done in it and its last session left";

/// Clears the mark for good; `why` goes to the log.
fn keep(untouched: &mut bool, id: &str, why: &str) {
    if std::mem::take(untouched) {
        crate::servers::clear_untouched(id);
        info(&format!("server {id} is kept from now on: {why}"));
    }
}

fn keep_if_touched(untouched: &mut bool, app: &App, id: &str) {
    let agent = app.spaces.iter().flat_map(|s| &s.tabs).flat_map(|t| t.panes.values()).any(|p| p.agent.is_some());
    if crate::servers::touched(&app.snapshot(), agent) {
        keep(untouched, id, "something was done in it");
    }
}

/// Whether the server may go now that its last session left on purpose: still untouched, a folder of its own,
/// no window on its way, and no process below a pane's shell. Anything not known keeps it.
fn discardable(untouched: &mut bool, app: &App, id: &str) -> bool {
    keep_if_touched(untouched, app, id);
    if *untouched && !crate::servers::untouched(id) {
        keep(untouched, id, "it was named, or its mark was removed");
    }
    if !*untouched {
        return false;
    }
    let kept = |why: &str| {
        info(&format!("server {id} not discarded: {why}"));
        false
    };
    if std::env::var_os("NERD_STATE_DIR").is_some() || std::env::var_os("NERD_SOCKET").is_some() {
        return kept("it was started at NERD_STATE_DIR or NERD_SOCKET");
    }
    if app.opening.is_some() {
        return kept("a window it opened is still on its way");
    }
    let Some(procs) = crate::detect::procs() else { return kept("the process table could not be read") };
    for p in app.spaces.iter().flat_map(|s| &s.tabs).flat_map(|t| t.panes.values()) {
        match p.pty.shell_pid() {
            None => return kept("a pane is still starting"),
            Some(shell) if !crate::detect::descendants(&procs, shell as i32).is_empty() => return kept("a program runs in a pane"),
            Some(_) => {}
        }
    }
    true
}

/// Ends a server nothing was done in and leaves nothing of it: no screen dump, no snapshot, no folder.
/// False when the saver could still write, which would make the folder again: the server then stops as usual.
fn discard(app: &mut App, id: &str) -> bool {
    if !app.saver.stop() {
        error("discard: the session saver did not stop within 5 s; stopping as usual");
        return false;
    }
    if let Some(b) = crate::broker::handle() {
        for p in app.spaces.iter().flat_map(|s| &s.tabs).flat_map(|t| t.panes.keys()) {
            b.kill(*p);
        }
    }
    app.plugins.stop_all();
    crate::isolation::remove_shared();
    let dir = crate::session::instances_dir().join(id);
    let gone = crate::session::instances_dir().join(format!(".gone-{id}"));
    let _ = std::fs::remove_dir_all(&gone);
    // Renamed first: a server listing it meanwhile sees no server, and the log below lands in the root.
    match std::fs::rename(&dir, &gone) {
        Ok(()) => {
            info(&format!("server {id} discarded: nothing was done in it and its last session left"));
            if let Err(e) = std::fs::remove_dir_all(&gone) {
                warn(&format!("server {id}: {} not removed: {e}", gone.display()));
            }
        }
        Err(e) => error(&format!("server {id} not discarded: its folder did not move: {e}")),
    }
    true
}

/// `7h 4m`, `2d 3h`, `4m`, `12s`: how long the server ran with no window.
fn gap(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// Sizes the panes to the frame's areas, then draws it; what it returns goes out right after the frame.
fn draw_turn<B: ratatui::backend::Backend>(tui: &mut ratatui::Terminal<B>, app: &mut App) -> Result<crate::ui::Effects, B::Error> {
    app.sync_tree();
    app.sync_sizes(crate::ui::areas(app, app.screen).panes);
    let mut fx = crate::ui::Effects::default();
    tui.draw(|f| fx = crate::ui::frame(f, app))?;
    Ok(fx)
}

/// The snapshot is a deep copy of everything: built only when something changed, and once an interval.
fn save_turn(app: &mut App, unsaved: &mut bool, changed: bool) {
    *unsaved |= changed;
    if *unsaved && app.saver.due() {
        *unsaved = false;
        app.saver.request(app.snapshot());
    }
}

/// The snapshot on disk before the process goes: exec and exit wait for it.
fn save_now(app: &mut App) {
    app.saver.request(app.snapshot());
    if !app.saver.flush() {
        error("session save did not answer within 5 s");
    }
}

/// A client gone mid-write ends that client, never the server: a dropped ssh link does this at any time.
fn lose_on_error(r: std::io::Result<()>, app: &mut App, sink: &Sink, id: u64) {
    if let Err(e) = r {
        warn(&format!("client {id} lost: {e}"));
        crate::clients::close(&mut app.clients, id, crate::clients::End::Lost, crate::clients::now());
        sink.set(None);
    }
}

/// Ends the screen client and every unattached socket, keeping the server; `by` is who asked, `None` for the menu.
fn disconnect_everyone(app: &mut App, sink: &Sink, current: u64, waiting: &Waiting, by: Option<&str>) -> Vec<String> {
    let mut ended = Vec::new();
    if sink.attached() {
        let back = way_back(app.caps.reattach.as_deref(), &crate::session::instance_name());
        sink.farewell(&match by {
            Some(by) => format!("nerd: this session was disconnected from {by}. Reattach with `{back}`."),
            None => format!("nerd: everyone was disconnected. Come back with `{back}`.\r\n{}", HYGIENE.join("\r\n")),
        });
        if app.caps.bye {
            sink.say(crate::proto::BYE);
        }
        crate::clients::close(&mut app.clients, current, crate::clients::End::Disconnected, crate::clients::now());
        sink.set(None);
        let own = nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned());
        if let Some(r) = app.clients.iter().find(|r| r.id == current) {
            ended.push(crate::clients::summary(r, own.as_deref()));
        }
    }
    for (_, s) in waiting.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        let how = if peer_program(&s).is_some_and(|p| p.starts_with("sshd")) { "over ssh" } else { "on this Mac" };
        let _ = s.shutdown(std::net::Shutdown::Both);
        ended.push(format!("ended: a connection {how} that had not attached"));
    }
    for line in &ended {
        info(line);
    }
    ended
}

/// What the human types after a takeover to get this screen back.
fn farewell(reattach: Option<&str>, instance: &str) -> String {
    format!("nerd: another session took over; this one is done. Reattach with `{}`.", way_back(reattach, instance))
}

fn way_back(reattach: Option<&str>, instance: &str) -> String {
    // Plain `nerd` opens a new instance, so the way back names this one.
    match reattach {
        Some(r) => r.to_string(),
        None if instance.is_empty() => "nerd".to_string(),
        None => format!("nerd --resume {instance}"),
    }
}

/// How long a new build may take to say what it is.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// A new build at our path, asked off the loop what it is and what changed since this one.
#[derive(Default)]
struct Probe {
    answer: Option<std::sync::mpsc::Receiver<(String, Vec<Vec<String>>)>>,
    /// The file changed during the probe (codesign does): the answer would be the older file's.
    again: bool,
}

impl Probe {
    fn start(&mut self, exe: &std::path::Path, bell: &crate::bell::Bell, limit: Duration) {
        if self.answer.is_some() {
            self.again = true;
            return;
        }
        let (tx, rx) = channel();
        let (exe, bell) = (exe.to_path_buf(), bell.clone());
        std::thread::spawn(move || {
            let until = Instant::now() + limit;
            let ask = |args: &[&str]| {
                let mut cmd = crate::spawn::cmd(&exe);
                cmd.args(args);
                crate::git::run_within(cmd, until.saturating_duration_since(Instant::now())).ok()
            };
            let info = ask(&["--version"]).unwrap_or_else(|| "unknown build".into());
            let changes = ask(&["--changes-since", &crate::version_line()]).unwrap_or_default().lines().map(|l| l.split('\t').map(String::from).collect()).collect();
            let _ = tx.send((info, changes));
            bell.ring();
        });
        self.answer = Some(rx);
    }

    /// The answer about the file as it is now, once there is one.
    fn take(&mut self, exe: &std::path::Path, bell: &crate::bell::Bell, limit: Duration) -> Option<(String, Vec<Vec<String>>)> {
        let got = match self.answer.as_ref()?.try_recv() {
            Ok(got) => got,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => ("unknown build".into(), Vec::new()),
        };
        self.answer = None;
        if std::mem::take(&mut self.again) {
            self.start(exe, bell, limit);
            return None;
        }
        Some(got)
    }
}

/// A reinstall at the same path changes the inode, which is how an update is detected.
fn file_id(p: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

/// Returns only on failure: a successful exec replaces this process, threads and all.
fn handover_exec(app: &mut App, sink: &Sink, listener_fd: i32, current: u64) -> std::io::Error {
    let mut inherited = Vec::new();
    let mut go = || -> std::io::Result<std::io::Error> {
        let Some(broker) = crate::broker::handle() else {
            return Err(std::io::Error::other("no broker to hand over"));
        };
        let panes = app.handover_panes()?;
        let broker_fd = broker.export_fd()?;
        nix::fcntl::fcntl(listener_fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()))?;
        let client = sink.export_fd().map(|fd| (current, fd, app.screen.width, app.screen.height, app.caps.mux));
        inherited.extend(panes.iter().map(|p| p.fd).chain([broker_fd, listener_fd]).chain(client.map(|c| c.1)));
        let h = crate::session::Handover {
            snapshot: app.snapshot(),
            panes,
            broker_fd,
            broker_pid: crate::broker::pid(),
            broker_protocol: crate::broker::protocol(),
            listener_fd,
            client,
            caps: app.caps.clone(),
            clients: app.clients.clone(),
            next_id: app.next_id,
            from: crate::version_line(),
        };
        crate::session::write_handover(&h)?;
        info(&format!("handover: {} panes, exec {}", h.panes.len(), std::env::current_exe()?.display()));
        // The writer keeps going, so a failed exec keeps the client; one still behind may get a sequence cut, which ARMED and the full paint repair.
        sink.drain(Duration::from_secs(1));
        use std::os::unix::process::CommandExt as _;
        Ok(std::process::Command::new(std::env::current_exe()?)
            .arg("--daemon")
            .env("NERD_HANDOVER", "1")
            .exec())
    };
    let (Ok(e) | Err(e)) = go();
    // The exec failed: later children (git, `nerd --version`) must not inherit what was opened for the new build.
    for fd in inherited {
        let _ = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC));
    }
    e
}

/// A sandbox failure doesn't stop startup: a multiplexer without network beats no multiplexer.
fn enter_sandbox(want: bool) -> String {
    if !want {
        return "disabled in config".into();
    }
    match crate::sandbox::apply() {
        Ok(()) => "enabled, network denied to the daemon".into(),
        Err(e) => format!("did NOT enable ({e}), running without it"),
    }
}

/// Key codes and modifiers, but never what was typed or pasted: the log outlives the trace.
fn trace_line(ev: &Event) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    match ev {
        Event::Key(k) if matches!(k.code, KeyCode::Char(_)) && !k.modifiers.contains(KeyModifiers::CONTROL) => format!("trace key char {:?}", k.modifiers),
        Event::Key(k) => format!("trace key {:?} {:?}", k.code, k.modifiers),
        Event::Paste(s) => format!("trace paste {} chars", s.chars().count()),
        Event::Mouse(m) => format!("trace mouse {:?} {:?}", m.kind, m.modifiers),
        other => format!("trace {other:?}"),
    }
}

/// A stale file left in place would outrank this start's line in the log.
fn record_sandbox(status: &str) {
    if let Err(e) = crate::paths::write_private(&crate::log::sandbox_path(), "tmp", status.as_bytes()) {
        error(&format!("sandbox status file failed: {e}"));
        let _ = std::fs::remove_file(crate::log::sandbox_path());
    }
}

/// From the status file, so a client carried over an in-place update is told too.
fn warn_sandbox(app: &mut App) {
    if let Some(n) = crate::log::sandbox_status().as_deref().and_then(sandbox_notice) {
        app.notice = Some(n);
    }
}

/// Only a failure: a sandbox turned off in the config is the human's own choice.
fn sandbox_notice(status: &str) -> Option<String> {
    let reason = status.strip_prefix("did NOT enable (")?.strip_suffix("), running without it")?;
    Some(format!("network sandbox is off: {reason}"))
}

#[cfg(test)]
mod tests {
    /// Polls the probe as the loop does, for up to `for_`.
    fn answer(p: &mut super::Probe, exe: &std::path::Path, limit: std::time::Duration, for_: std::time::Duration) -> Option<(String, Vec<Vec<String>>)> {
        let (bell, _door) = crate::bell::new();
        let until = std::time::Instant::now() + for_;
        while std::time::Instant::now() < until {
            if let Some(a) = p.take(exe, &bell, limit) {
                return Some(a);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let exe = dir.join("nerd");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    #[test]
    fn a_new_build_that_hangs_holds_up_neither_the_loop_nor_the_card() {
        let dir = std::env::temp_dir().join(format!("nerd-probe-hang-{}", std::process::id()));
        let exe = script(&dir, "exec sleep 30");
        let (bell, _door) = crate::bell::new();
        let mut p = super::Probe::default();
        let t = std::time::Instant::now();
        p.start(&exe, &bell, std::time::Duration::from_millis(300));
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "started in {:?}", t.elapsed());
        let got = answer(&mut p, &exe, std::time::Duration::from_millis(300), std::time::Duration::from_secs(5));
        assert_eq!(got, Some(("unknown build".into(), Vec::new())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_change_during_the_probe_is_asked_again() {
        let dir = std::env::temp_dir().join(format!("nerd-probe-again-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("v"), "1").unwrap();
        let d = dir.display();
        let exe = script(&dir, &format!("v=$(cat {d}/v); touch {d}/read-$v; sleep 0.3\n[ \"$1\" = --version ] && echo \"nerd $v\" || printf 'Added\\tbuild %s\\n' $v"));
        let (bell, _door) = crate::bell::new();
        let limit = std::time::Duration::from_secs(5);
        let mut p = super::Probe::default();
        p.start(&exe, &bell, limit);
        while !dir.join("read-1").exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // codesign after install: the same path changes again while the first build answers.
        std::fs::write(dir.join("v"), "2").unwrap();
        p.start(&exe, &bell, limit);
        let got = answer(&mut p, &exe, limit, std::time::Duration::from_secs(8));
        assert_eq!(got, Some(("nerd 2".into(), vec![vec!["Added".into(), "build 2".into()]])));
        assert_eq!(answer(&mut p, &exe, limit, std::time::Duration::from_millis(500)), None, "one answer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trace_never_holds_what_was_typed_or_pasted() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(super::trace_line(&Event::Paste("secret".into())), "trace paste 6 chars");
        let line = super::trace_line(&Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SHIFT)));
        assert_eq!(line, "trace key char KeyModifiers(SHIFT)");
        assert_eq!(super::trace_line(&Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))), "trace key Char('c') KeyModifiers(CONTROL)");
        assert_eq!(super::trace_line(&Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))), "trace key F(2) KeyModifiers(0x0)");
    }

    #[test]
    fn a_trace_names_a_mouse_press_without_its_place() {
        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let down = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 7, row: 3, modifiers: KeyModifiers::NONE };
        assert_eq!(super::trace_line(&Event::Mouse(down)), "trace mouse Down(Left) KeyModifiers(0x0)");
    }

    #[test]
    fn a_status_file_that_cannot_be_written_leaves_no_stale_one() {
        crate::session::tests::with_temp_state_dir("sandbox-stale", || {
            let path = crate::log::sandbox_path();
            std::fs::write(&path, "enabled, network denied to the daemon").unwrap();
            // A directory where the temporary file goes makes the write fail.
            std::fs::create_dir(path.with_extension("tmp")).unwrap();
            crate::log::warn("sandbox: did NOT enable (x), running without it");
            super::record_sandbox("did NOT enable (x), running without it");
            assert!(!path.exists());
            assert_eq!(crate::log::sandbox_status().as_deref(), Some("did NOT enable (x), running without it"));
        });
    }

    #[test]
    fn only_a_sandbox_that_failed_is_noticed() {
        assert_eq!(super::sandbox_notice("did NOT enable (denied), running without it").as_deref(), Some("network sandbox is off: denied"));
        assert_eq!(super::sandbox_notice("disabled in config"), None);
        assert_eq!(super::sandbox_notice("enabled, network denied to the daemon"), None);
    }

    #[test]
    fn the_logo_goes_out_in_the_turn_that_drew_it() {
        let mut app = crate::app::App::new(crate::config::Config::default());
        (app.graphics, app.caps.mux, app.sidebar) = (true, false, true);
        app.screen = ratatui::layout::Rect::new(0, 0, 120, 30);
        let mut tui = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        let fx = super::draw_turn(&mut tui, &mut app).unwrap();
        assert!(String::from_utf8_lossy(&fx.screen).contains("a=T,f=100,i=2,c=9,r=1,"), "the logo's PNG is in this turn's output");
        assert!(app.to_client.is_empty(), "nothing is left for the next turn");
    }

    #[test]
    fn every_socket_not_attached_is_ended_and_leaves_the_registry() {
        use std::io::Read as _;
        let path = std::env::temp_dir().join(format!("nerd-waiting-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (tx, _rx) = std::sync::mpsc::channel();
        let waiting = super::Waiting::default();
        let inbox = super::Inbox { tx, bell: crate::bell::new().0 };
        super::accept_loop(std::os::unix::net::UnixListener::bind(&path).unwrap(), inbox, 0, waiting.clone());
        let mut socks: Vec<_> = (0..3).map(|_| std::os::unix::net::UnixStream::connect(&path).unwrap()).collect();
        // One sends half a Hello and stops: still not attached.
        std::io::Write::write_all(&mut socks[1], b"{\"Hello\":").unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while waiting.lock().unwrap().len() < 3 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(waiting.lock().unwrap().len(), 3);

        let mut app = crate::app::App::new(crate::config::Config::default());
        let ended = super::disconnect_everyone(&mut app, &super::Sink::new(crate::bell::new().0), 0, &waiting, Some("a terminal on this Mac"));
        assert_eq!(ended, vec!["ended: a connection on this Mac that had not attached"; 3]);
        for s in &mut socks {
            // macOS refuses the option on a socket already shut down; the read then ends at once.
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            assert_eq!(s.read(&mut [0u8; 16]).unwrap(), 0, "EOF");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(waiting.lock().unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_the_menu_the_screen_client_hears_the_way_back_and_the_ssh_lines() {
        use crate::clients::{close, now, ClientRecord, End};
        use std::io::Read as _;
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let sink = super::Sink::new(crate::bell::new().0);
        sink.set(Some(ours));
        let mut app = crate::app::App::new(crate::config::Config::default());
        app.client = 7;
        app.caps = crate::proto::ClientCaps { bye: true, reattach: Some("nerd --remote studio 1".into()), ..Default::default() };
        crate::clients::attach(&mut app.clients, ClientRecord { id: 7, since: now(), host: None, program: Some("nerd".into()), ended: None });
        let ended = super::disconnect_everyone(&mut app, &sink, 7, &super::Waiting::default(), None);
        assert_eq!(ended.len(), 1);
        assert!(ended[0].starts_with("ended: ") && ended[0].contains(" disconnected at "), "{ended:?}");
        assert!(!sink.attached());

        let mut got = Vec::new();
        theirs.read_to_end(&mut got).unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains("nerd: everyone was disconnected. Come back with `nerd --remote studio 1`.\r\n"), "{got}");
        for line in super::HYGIENE {
            assert!(got.contains(line), "{got}");
        }
        assert!(got.ends_with(std::str::from_utf8(crate::proto::BYE).unwrap()), "{got:?}");

        // The reader's EOF that follows is a stale Detach: the record keeps its first end.
        close(&mut app.clients, 7, End::Closed, now());
        assert!(matches!(app.clients[0].ended, Some((_, End::Disconnected))));
        assert!(super::disconnect_everyone(&mut app, &sink, 7, &super::Waiting::default(), None).is_empty(), "nobody left");
    }

    #[test]
    fn a_rename_with_nothing_after_it_is_saved_within_the_interval() {
        use std::time::{Duration, Instant};
        crate::session::tests::with_temp_state_dir("rename-saved", || {
            let mut app = crate::app::App::new(crate::config::Config::default());
            app.spaces.push(crate::app::Space { name: None, cwd: "/".into(), tabs: Vec::new(), active_tab: 0, isolation_new: None });
            let mut unsaved = false;
            // Generous bounds: the saver is a thread, and a loaded machine may be slow to run it.
            let within = |what: &str, done: &mut dyn FnMut() -> bool| {
                let t = Instant::now();
                while !done() {
                    assert!(t.elapsed() < Duration::from_secs(2), "{what}");
                    std::thread::sleep(Duration::from_millis(16));
                }
            };
            super::save_turn(&mut app, &mut unsaved, true);
            within("the first snapshot goes out", &mut || crate::session::load().is_some());
            app.spaces[0].name = Some("renamed".into());
            super::save_turn(&mut app, &mut unsaved, true);
            if !app.saver.due() {
                assert!(unsaved, "within the interval the rename waits, and is not forgotten");
            }
            // Every later turn is quiet, as the loop's 16 ms ticks are.
            within("the rename was not saved", &mut || {
                super::save_turn(&mut app, &mut unsaved, false);
                crate::session::load().and_then(|s| s.spaces[0].name.clone()).is_some()
            });
        });
    }

    #[test]
    fn the_time_without_a_window_reads_in_two_units_at_most() {
        assert_eq!(super::gap(12), "12s");
        assert_eq!(super::gap(4 * 60 + 59), "4m");
        assert_eq!(super::gap(7 * 3600 + 4 * 60 + 30), "7h 4m");
        assert_eq!(super::gap(2 * 86400 + 3 * 3600 + 59 * 60), "2d 3h");
    }

    #[test]
    fn the_farewell_names_the_way_back_of_the_client_leaving() {
        assert!(super::farewell(Some("nerd --remote studio 1"), "1").contains("`nerd --remote studio 1`"));
        assert!(super::farewell(None, "2").contains("`nerd --resume 2`"));
        assert!(super::farewell(None, "").contains("`nerd`"));
    }
}
