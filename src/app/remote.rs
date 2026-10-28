use super::*;

impl App {
    /// Menu > Servers…: the window opens at once and fills in when the servers have answered.
    pub(super) fn servers_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let own = crate::session::instance_name();
        self.servers.ask(crate::server_list::Job::List(own.clone()));
        Overlay::Servers(crate::server_list::ServerList::new(own, self.cfg.remote_entries(), self.caps.connects, self.caps.bye).caps(self.caps.switches, self.caps.opens_windows))
    }

    /// The open Servers window, or the one waiting under a dialog it opened.
    pub(super) fn servers_window(&mut self) -> Option<&mut crate::server_list::ServerList> {
        match &mut self.overlay {
            Overlay::Servers(w) => Some(w),
            _ => self.servers_back.as_mut(),
        }
    }

    /// The worker's answers: the window's rows, or what a job said, after which the window asks again.
    pub(super) fn take_servers(&mut self) -> bool {
        use crate::server_list::Found;
        let found: Vec<Found> = self.servers.take().collect();
        let changed = !found.is_empty();
        for f in found {
            match f {
                Found::Listed(list, strays) => {
                    if let Some(w) = self.servers_window() {
                        w.set(list, strays);
                    }
                }
                Found::Done(said) => {
                    for line in said {
                        match line {
                            Ok(l) => self.toast = Some(printable(&l)),
                            Err(e) => self.notice = Some(printable(&e)),
                        }
                    }
                    if self.servers_window().is_some() {
                        self.servers.ask(crate::server_list::Job::List(crate::session::instance_name()));
                    }
                }
            }
        }
        self.dirty |= changed;
        changed
    }

    /// `next` in the window's place, the window kept for when it is answered; nothing to show leaves the window.
    fn away(&mut self, w: crate::server_list::ServerList, next: Overlay) -> Overlay {
        if next.is_open() {
            self.servers_back = Some(w);
            next
        } else {
            Overlay::Servers(w)
        }
    }

    pub(super) fn servers_outcome(&mut self, w: crate::server_list::ServerList, out: crate::server_list::Out) -> Overlay {
        use crate::menu::{Confirm, Prompt};
        use crate::server_list::Out;
        let own = crate::session::instance_name();
        let changes = !matches!(out, Out::None | Out::Close | Out::List(_) | Out::Open(..) | Out::OpenRemote(..) | Out::EditConfig);
        if changes && self.client_inside {
            self.notice = Some(super::mail::INSIDE_REFUSED.into());
            return Overlay::Servers(w);
        }
        let name = |s: &crate::servers::Server| printable(&s.label);
        match out {
            Out::None => Overlay::Servers(w),
            Out::Close => Overlay::None,
            // A takeover may have put another client on screen since the window opened.
            Out::List(_) if !self.caps.connects => Overlay::Servers(w),
            Out::List(r) => {
                crate::log::info(&format!("connect: asked client {} to list {}", self.client, r.name));
                self.to_client.extend(crate::proto::list_remote_marker(&r.name));
                self.remote_ask = Some((r.name.clone(), Instant::now() + REMOTE_WAIT));
                self.toast = Some(format!("asking {} for its servers…", r.name));
                Overlay::Servers(w)
            }
            // Once the marker is out the window is done; a question or a refusal comes back to it.
            Out::Open(s, new_window) => {
                let sent = self.to_client.len();
                let next = self.open_server(&s.id, s.shown, new_window);
                if self.to_client.len() > sent { Overlay::None } else { self.away(w, next) }
            }
            Out::OpenRemote(m, i, _) if !i.running => {
                self.notice = Some(format!("server {0} on {m} is stopped; start it there with nerd --resume {0}", printable(&i.id)));
                Overlay::Servers(w)
            }
            Out::OpenRemote(m, i, window) => self.open_remote(&m, Some(&i.id), window),
            Out::EditConfig => {
                self.edit_config();
                Overlay::Servers(w)
            }
            Out::New => self.new_server(w),
            Out::Rename(s) => {
                let prefill = if s.label == s.id { String::new() } else { s.label.clone() };
                let title = if s.id == own { "Rename this server" } else { "Rename server" };
                self.away(w, Overlay::Prompt(Prompt::new(title, prefill).does("rename"), Ask::RenameServer(s.id)))
            }
            Out::Update(s) if s.id == own => {
                let next = self.update_overlay();
                self.away(w, next)
            }
            Out::Stop(s) if s.id == own => {
                let next = self.stop_overlay();
                self.away(w, next)
            }
            Out::Update(s) => {
                let ask = Confirm::new("Update server", format!("Server {} swaps itself for the installed build in place; its panes and agents keep running.", name(&s))).does("update");
                self.away(w, Overlay::Confirm(ask, Pending::ServerUpdate(s.id)))
            }
            Out::Stop(s) => {
                let body = format!("Server {0} stops: its panes close, and what runs in them is killed. Start it again with nerd --resume {0}; its spaces and the panes' screens are kept.", name(&s));
                let ask = Confirm::new("Stop server", body).does("stop the server").danger(true).about(self.on_machine(String::new()));
                self.away(w, Overlay::Confirm(ask, Pending::ServerStop(s.id)))
            }
            Out::StopStray(st) => {
                let body = format!("The nerd server with pid {} ({}) gets SIGTERM: its panes close, and what runs in them is killed.", st.pid, printable(&st.note));
                let ask = Confirm::new("Stop server", body).does("stop the server").danger(true).about(self.on_machine(String::new()));
                self.away(w, Overlay::Confirm(ask, Pending::StrayStop(st.pid, st.exe)))
            }
            // A folder whose server never started holds nothing: it goes without a question.
            Out::Remove(s) if s.reserved_only => {
                self.servers.ask(crate::server_list::Job::Remove(vec![s.id], own));
                Overlay::Servers(w)
            }
            Out::Remove(s) => self.remove_overlay(w, vec![s]),
            Out::RemoveAll(list) if list.is_empty() => {
                self.notice = Some("no stopped servers".into());
                Overlay::Servers(w)
            }
            Out::RemoveAll(list) => self.remove_overlay(w, list),
        }
    }

    /// Remove… and Remove all stopped… say what goes and what stays before anything does.
    fn remove_overlay(&mut self, w: crate::server_list::ServerList, list: Vec<crate::servers::Server>) -> Overlay {
        let (title, what) = match list.as_slice() {
            [s] => ("Remove server", format!("Server {}", printable(&s.label))),
            _ => ("Remove all stopped", format!("These {} stopped servers", list.len())),
        };
        let body = format!("{what} and {} go for good; {}.", crate::servers::GOES, crate::servers::STAYS);
        let about = list.iter().map(|s| if s.label == s.id { printable(&s.id) } else { format!("{} ({})", printable(&s.label), s.id) }).collect::<Vec<_>>().join(", ");
        let ask = crate::menu::Confirm::new(title, body).does("remove").danger(true).about(self.on_machine(about));
        self.away(w, Overlay::Confirm(ask, Pending::ServerRemove(list.into_iter().map(|s| s.id).collect())))
    }

    /// New server…: a new folder, marked as untouched, opened in this session's place as any other server is.
    fn new_server(&mut self, w: crate::server_list::ServerList) -> Overlay {
        if !self.caps.connects || !self.caps.switches {
            let (body, cmd) = stays_here(!crate::client::can_switch(), "nerd --new");
            let ask = crate::menu::Confirm::new("New server", body).labels(" [Enter] copy the command ", " [Esc] close ");
            return self.away(w, Overlay::Confirm(ask, Pending::CopyCommand(cmd)));
        }
        let id = crate::session::new_instance_name();
        let _ = crate::servers::mark_untouched(&id);
        crate::log::info(&format!("servers: new server {id} reserved for client {}", self.client));
        self.send_open_server(&id, false, false);
        Overlay::None
    }

    /// The client's answer to `list-remote`, for the Servers window. A late one, or one for another machine, changes nothing.
    pub fn remote_listed(&mut self, name: String, result: Result<Vec<crate::proto::RemoteInstance>, String>, window: bool, ssh_failed: bool) {
        use crate::server_list::Machine;
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
                if let Some(w) = self.servers_window() {
                    w.listed(&name, Machine::Failed(e.clone()));
                }
                // Only a prompt ssh refused is answered by a terminal; the rest show in the machine's section.
                if !ssh_failed {
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
                if let Overlay::Servers(w) = std::mem::replace(&mut self.overlay, Overlay::None) {
                    self.servers_back = Some(w);
                }
                self.overlay = Overlay::Confirm(crate::menu::Confirm::new(format!("Can't reach {name}"), body).labels(button, " [Esc] close "), what);
                return;
            }
        };
        let list: Vec<_> = list.into_iter().filter(|i| crate::session::valid_id(&i.id) && crate::session::valid_label(&i.label)).collect();
        crate::log::info(&format!("connect: {name} lists {} servers", list.len()));
        if let Some(w) = self.servers_window() {
            w.listed(&name, Machine::Listed(list, window));
        }
    }

    /// Opens server `id` of this Mac for this session: in its place, or in a terminal window of its own.
    /// `shown`: what the listing said; anything but "no session shows it" asks before taking it over.
    pub fn open_server(&mut self, id: &str, shown: Option<bool>, new_window: bool) -> Overlay {
        use crate::menu::Confirm;
        let label = printable(&crate::session::instance_label(id));
        let cmd = format!("nerd --resume {label}");
        let copy_this = |body: String, cmd: String| Overlay::Confirm(Confirm::new("Open a server", body).labels(" [Enter] copy the command ", " [Esc] close "), Pending::CopyCommand(cmd));
        let copy = |body: String| copy_this(body, cmd.clone());
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
            let (body, cmd) = stays_here(!crate::client::can_switch(), &cmd);
            return copy_this(body, cmd);
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

    /// No answer from the client about a machine by now: it is busy or gone.
    pub fn tick_remote(&mut self) -> bool {
        let Some((name, until)) = self.remote_ask.clone() else { return false };
        if Instant::now() < until {
            return false;
        }
        let said = format!("no answer from {name} in {} s", REMOTE_WAIT.as_secs());
        if let Some(w) = self.servers_window() {
            w.listed(&name, crate::server_list::Machine::Failed(said.clone()));
        }
        self.notice = Some(said);
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

/// How long Servers… waits for the client's ssh to list a machine.
pub(super) const REMOTE_WAIT: Duration = Duration::from_secs(20);

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

/// Why a session that sent no `switches` stays on this server, and the command to copy instead of `cmd`.
fn stays_here(test: bool, cmd: &str) -> (String, String) {
    if test {
        let body = format!("This server was started for a test (NERD_SOCKET or NERD_STATE_DIR is set), so a session cannot move from it to another server. Run `{cmd}` in a terminal without those variables; the button copies it.");
        return (body, cmd.into());
    }
    let body = "This window was opened before nerd was last updated, so it cannot move to another server. Detach and run `nerd` again in this terminal to open it with the new build; the button copies it.";
    (body.into(), "nerd".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_refused_move_says_whether_the_server_is_a_test_or_the_window_predates_an_update() {
        let (body, cmd) = super::stays_here(true, "nerd --resume 2");
        assert!(body.starts_with("This server was started for a test (NERD_SOCKET or NERD_STATE_DIR is set)") && body.contains("`nerd --resume 2`"), "{body}");
        assert_eq!(cmd, "nerd --resume 2");
        let (body, cmd) = super::stays_here(false, "nerd --resume 2");
        assert!(body.starts_with("This window was opened before nerd was last updated") && body.contains("run `nerd` again"), "{body}");
        assert_eq!(cmd, "nerd", "the old window copies the plain command that reopens it on the new build");
    }
}
