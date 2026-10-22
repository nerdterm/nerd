use super::*;

impl App {
    /// Menu > Connect to another computer…: the machines in config.json, or why there is nothing to pick.
    pub(super) fn connect_overlay(&mut self) -> Overlay {
        use crate::menu::{Confirm, Picker, PickerItem};
        let info = |body: &str| Overlay::Confirm(Confirm::new("Connect to another computer", body).info(), Pending::Dismiss);
        if !self.caps.connects {
            return info(if self.caps.bye {
                "This window shows nerd on another Mac, which lists machines of its own. Connect from a nerd window on this Mac."
            } else {
                "This window's nerd is older than the server. Close it and open nerd again to connect."
            });
        }
        let remotes = self.cfg.remote_entries();
        if remotes.is_empty() {
            return info(NO_REMOTES);
        }
        let items = remotes
            .iter()
            .enumerate()
            .map(|(id, r)| PickerItem {
                label: printable(&r.name),
                detail: match r.check() {
                    Ok(()) => r.host.clone(),
                    Err(e) => format!("not usable: {e}"),
                },
                id,
            })
            .collect();
        Overlay::Picker(Picker::new("Connect to another computer", items), Pick::Remote(remotes))
    }

    /// The client's answer to `list-remote`. A late one, or one for another machine, opens nothing.
    pub fn remote_listed(&mut self, name: String, result: Result<Vec<crate::proto::RemoteInstance>, String>, window: bool, ssh_failed: bool) {
        if self.remote_ask.as_ref().is_none_or(|(n, _)| *n != name) {
            crate::log::warn(&format!("connect: dropped an answer about {} nobody waits for", printable(&name)));
            return;
        }
        self.remote_ask = None;
        self.toast = None;
        self.dirty = true;
        let list = match result {
            Ok(list) => list,
            Err(e) => {
                let e = printable(&e);
                crate::log::error(&format!("connect: {name} did not list: {e}"));
                // Only ssh itself failing can be a prompt BatchMode refused; the rest are answers.
                if !ssh_failed {
                    self.notice = Some(e);
                    return;
                }
                let (body, button, what) = if window {
                    (
                        format!("ssh could not list the servers without asking: {e}. A terminal of its own can ask for a password or a host key."),
                        " [Enter] open a terminal for it ",
                        Pending::OpenRemote(name.clone()),
                    )
                } else {
                    (
                        format!("ssh could not list the servers without asking: {e}. Run `nerd --remote {name}` in a terminal on this Mac; the button copies it."),
                        " [Enter] copy the command ",
                        Pending::CopyCommand(format!("nerd --remote {name}")),
                    )
                };
                self.overlay = Overlay::Confirm(crate::menu::Confirm::new(format!("Can't reach {name}"), body).labels(button, " [Esc] close "), what);
                return;
            }
        };
        let list: Vec<_> = list.into_iter().filter(|i| crate::session::valid_id(&i.id) && crate::session::valid_label(&i.label)).collect();
        crate::log::info(&format!("connect: {name} lists {} servers", list.len()));
        if list.is_empty() {
            self.notice = Some(format!("{name} has no servers; start one there with nerd"));
            return;
        }
        let items = list
            .iter()
            .enumerate()
            .map(|(id, i)| crate::menu::PickerItem {
                label: if i.label == i.id { i.id.clone() } else { format!("{} ({})", i.label, i.id) },
                detail: if i.running { format!("running, {}", plural_spaces(i.spaces)) } else { "stopped".into() },
                id,
            })
            .collect();
        self.overlay = Overlay::Picker(crate::menu::Picker::new(format!("Servers on {name}"), items), Pick::RemoteInstance { name, list, window });
    }

    /// Opens server `id` of this Mac for this session: in its place, or in a terminal window of its own.
    /// `shown`: what the listing said; anything but "no session shows it" asks before taking it over.
    // Servers… calls it; until then only the tests do.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn open_server(&mut self, id: &str, shown: Option<bool>, new_window: bool) -> Overlay {
        use crate::menu::Confirm;
        let label = printable(&crate::session::instance_label(id));
        let cmd = format!("nerd --resume {label}");
        let copy = |body: String| Overlay::Confirm(Confirm::new("Open a server", body).labels(" [Enter] copy the command ", " [Esc] close "), Pending::CopyCommand(cmd.clone()));
        if self.client_inside {
            self.notice = Some(super::mail::INSIDE_REFUSED.into());
            return Overlay::None;
        }
        if !crate::session::valid_id(id) || id == crate::session::instance_name() {
            self.notice = Some(format!("server {label} is the one this session shows"));
            return Overlay::None;
        }
        if !self.caps.connects {
            return Overlay::Confirm(Confirm::new("Open a server", "This session shows nerd on another Mac. Open its servers from a terminal there.").info(), Pending::Dismiss);
        }
        if !self.caps.switches {
            return copy(format!("This session cannot move to another server: its nerd is older than the server, or it was started for a test server. Run `{cmd}` in a terminal; the button copies it."));
        }
        if new_window && !self.caps.opens_windows {
            return copy(format!("nerd cannot open a terminal window from here. Run `{cmd}` in a terminal on this Mac; the button copies it."));
        }
        if shown != Some(false) {
            let ask = Confirm::new("Open a server", format!("Server {label} is open in another session. Take it over?")).does("take it over");
            return Overlay::Confirm(ask, Pending::OpenServer(id.to_string(), new_window));
        }
        self.send_open_server(id, false, new_window);
        Overlay::None
    }

    /// The marker for an open the person settled; `over` takes the server from the session that shows it.
    pub(super) fn send_open_server(&mut self, id: &str, over: bool, new_window: bool) {
        let label = printable(&crate::session::instance_label(id));
        let (marker, how) = if new_window { (crate::proto::open_server_marker(id, over), "in a new window") } else { (crate::proto::switch_marker(id, over), "in this session") };
        crate::log::info(&format!("servers: asked client {} to open server {id} {how}{}", self.client, if over { ", taking it over" } else { "" }));
        self.to_client.extend(marker);
        self.toast = Some(format!("opening server {label} {how}"));
    }

    /// The client opens the window; one that cannot gets the command to run instead.
    pub(super) fn open_remote(&mut self, name: &str, id: Option<&str>, window: bool) -> Overlay {
        let cmd = format!("nerd --remote {name}{}", id.map(|i| format!(" {i}")).unwrap_or_default());
        if !window {
            let body = format!("nerd cannot open a terminal window from here. Run `{cmd}` in a terminal on this Mac; the button copies it.");
            let dialog = crate::menu::Confirm::new(format!("Can't reach {name}"), body).labels(" [Enter] copy the command ", " [Esc] close ");
            return Overlay::Confirm(dialog, Pending::CopyCommand(cmd));
        }
        crate::log::info(&format!("connect: asked client {} to open {cmd}", self.client));
        self.to_client.extend(crate::proto::open_remote_marker(name, id));
        self.toast = Some(format!("opening a window: {cmd}"));
        Overlay::None
    }

    /// No answer to Connect to another computer… by now: the client is busy or gone.
    pub fn tick_remote(&mut self) -> bool {
        let Some((name, until)) = &self.remote_ask else { return false };
        if Instant::now() < *until {
            return false;
        }
        self.notice = Some(format!("no answer from {name} in {} s", REMOTE_WAIT.as_secs()));
        self.remote_ask = None;
        true
    }

    /// What the client runs inside: tmux or screen.
    pub(super) fn mux_name(&self) -> &'static str {
        if self.caps.screen { "screen" } else { "tmux" }
    }

    /// The offer to open a window outside tmux, and what became of it.
    pub fn offer_outside_tmux(&mut self, client: u64) {
        if !self.caps.mux || self.offered == Some(client) || self.overlay.is_open() {
            return;
        }
        crate::log::info(&format!(
            "inside a multiplexer: setting {:?}, terminal {:?}, client opens windows: {}",
            self.cfg.tmux,
            self.caps.terminal.as_ref().map(|t| t.display().to_string()),
            self.caps.opens_windows
        ));
        self.offered = Some(client);
        match self.cfg.tmux {
            crate::config::TmuxOffer::Stay => {}
            // A client nerd opened itself never opens another: that is what bounds the chain.
            crate::config::TmuxOffer::NewWindow if !self.caps.opened_by_nerd && self.can_open_a_window() => {
                self.open_outside_tmux();
            }
            _ => {
                self.overlay = self.tmux_offer();
                self.dirty = true;
            }
        }
    }

    /// A terminal nerd can start, and someone able to start it: the client when it does that
    /// itself, otherwise this server, which can only be found from the default state root.
    fn can_open_a_window(&self) -> bool {
        self.caps.terminal.is_some() && (self.caps.opens_windows || self.default_state_root)
    }

    /// Says why it opened and what the answer changes; the third button also stops the asking.
    pub fn tmux_offer(&self) -> Overlay {
        use crate::menu::Confirm;
        let here = self.mux_name();
        let (body, yes, what) = match self.caps.terminal.as_ref().filter(|_| self.can_open_a_window()).and_then(|t| t.file_stem()).and_then(|s| s.to_str()) {
            Some(name) => (
                format!(
                    "Image previews here are drawn in coloured blocks: {here} carries no graphics.                      A new {name} window on this same server shows them in full quality, with the same                      panes and agents. This {here} session and its windows stay as they are."
                ),
                format!(" [Enter] open {name} "),
                Pending::LeaveTmux,
            ),
            None => (
                format!(
                    "Image previews here are drawn in coloured blocks: {here} carries no graphics.                      For full quality run `{}` in a terminal of your own; the button copies that command.",
                    self.resume_command()
                ),
                " [Enter] copy the command ".to_string(),
                Pending::CopyResumeCommand,
            ),
        };
        let dialog = Confirm::new(format!("nerd is inside {here}"), body)
            .labels(yes, " [Esc] stay here ")
            // Opens by itself, so a stray Enter must be the harmless answer.
            .other(" [d] never ask ")
            .select(1);
        Overlay::Confirm(dialog, what)
    }

    /// What the human would type to reach this server from a terminal of their own.
    pub fn resume_command(&self) -> String {
        if let Some(r) = &self.caps.reattach {
            return r.clone();
        }
        match crate::session::instance_name() {
            // The legacy instance has no name, and `--resume` with none means the last used.
            name if name.is_empty() => "nerd".to_string(),
            name => format!("nerd --resume {name}"),
        }
    }

    /// Starts the terminal app with a nerd client for this instance. `open` exits 0 even when
    /// nothing starts, so success is a client attaching, which `tick_opening` waits for.
    pub fn open_outside_tmux(&mut self) {
        let Some(app) = self.caps.terminal.clone().filter(|p| !p.as_os_str().is_empty()) else {
            self.notice = Some(format!("no terminal to open; run {} yourself", self.resume_command()));
            return;
        };
        // The client is a fresh process in the user's session: launch services answer it even
        // when they no longer answer a server that has been running for days.
        if self.caps.opens_windows {
            self.to_client.extend_from_slice(crate::proto::OPEN_WINDOW);
            crate::log::info(&format!("open outside tmux: asked the client for a {} window", app.display()));
            self.opening = Some(Opening { pty: None, until: Instant::now() + OPEN_WAIT, out: Vec::new(), app });
            self.toast = Some(format!("opening a window outside {}…", self.mux_name()));
            self.dirty = true;
            return;
        }
        // An older client cannot, so the server opens it, and only reaches a client that resolves
        // the same socket from the login environment.
        if !self.default_state_root {
            self.notice = Some(format!("this server runs outside the default state directory; run {} with the same environment", self.resume_command()));
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            self.notice = Some(format!("cannot find nerd's own path; run {} yourself", self.resume_command()));
            return;
        };
        let argv = open_argv(&app, &exe, &crate::session::instance_name());
        let id = self.alloc_id();
        crate::log::info(&format!("open outside tmux: {}", argv.join(" ")));
        match crate::pty::Pty::spawn(id, &argv, &crate::paths::home().unwrap_or_else(std::env::temp_dir), 1, 1, self.bell.clone(), &crate::policy::Isolation::default()) {
            Ok(pty) => {
                self.opening = Some(Opening { pty: Some(pty), until: Instant::now() + OPEN_WAIT, out: Vec::new(), app });
                self.toast = Some(format!("opening a window outside {}…", self.mux_name()));
            }
            Err(e) => self.open_failed(&e.to_string()),
        }
        self.dirty = true;
    }

    pub(super) fn open_failed(&mut self, e: &str) {
        crate::log::error(&format!("open outside tmux failed: {e}"));
        self.notice = Some(format!("could not start the terminal: {e}; run {} yourself", self.resume_command()));
        self.opening = None;
    }

    /// The window never appeared: say so with the command, and log whatever `open` printed.
    pub fn tick_opening(&mut self) -> bool {
        let Some(op) = &self.opening else {
            return false;
        };
        if Instant::now() < op.until {
            return false;
        }
        let printed = String::from_utf8_lossy(&op.out).trim().to_string();
        crate::log::error(&format!("open outside tmux: {} started no client; open said {printed:?}", op.app.display()));
        self.notice = Some(format!("no window appeared; run {} in your terminal", self.resume_command()));
        self.opening = None;
        true
    }
}

/// `open` starts a second instance of the app (-n) and hands the rest to it (-a, --args).
pub(super) fn open_argv(app: &std::path::Path, exe: &std::path::Path, instance: &str) -> Vec<String> {
    ["/usr/bin/open", "-na", &app.to_string_lossy(), "--args", "-e", &exe.to_string_lossy(), "--resume", instance, "--from-window"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// How long a window has to appear before nerd says it did not.
const OPEN_WAIT: Duration = Duration::from_secs(5);

/// How long Connect to another computer… waits for the client's ssh to list a machine.
pub(super) const REMOTE_WAIT: Duration = Duration::from_secs(20);

const NO_REMOTES: &str = "No machines yet. Add them to config.json (nerd --config prints its path) as \
    \"remotes\": [{\"name\": \"studio\", \"host\": \"me@studio.local\"}], with ssh keys set up for the host. \
    Update running nerd servers first: an older one drops the key when it saves settings. \
    In a remote session, links, pbcopy and opening files act on that Mac.";

/// Text from a file or another machine, safe to draw: control characters would move the cursor.
pub(super) fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// The `open` process behind an offer accepted; dropped when a client attaches or the wait ends.
pub struct Opening {
    /// The bundle it was told to open, for the log when nothing appears.
    pub(super) app: std::path::PathBuf,
    /// Dropped when a client attaches or the wait ends, which kills `open`, nothing else.
    /// None only in tests.
    pub(super) pty: Option<crate::pty::Pty>,
    pub(super) until: Instant,
    pub(super) out: Vec<u8>,
}
