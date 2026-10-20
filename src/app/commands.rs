use super::*;

impl App {
    /// A subagent's run opens in its agent's pane, which takes the focus, as a click on its row gives it.
    pub(super) fn open_run(&mut self, pane: PaneId, id: String) {
        if let Some((si, ti)) = self.locate(pane) {
            self.go_to(si, ti, pane);
            self.run = Some(crate::run::Run::new(pane, id));
            self.view.run = Default::default();
            self.run_keys = true;
            self.run_zoom = false;
        }
        self.dirty = true;
    }

    /// The pane menu's Subagents...: the run itself when there is one, a list to pick from when more.
    pub(super) fn subagents_overlay(&mut self, id: PaneId) -> Overlay {
        let subs = self.pane(id).map(|p| p.activity.subs.clone()).unwrap_or_default();
        match subs.as_slice() {
            [] => Overlay::None,
            [s] => {
                self.open_run(id, s.id.clone());
                Overlay::None
            }
            _ => {
                let now = crate::hook::now();
                let items = subs
                    .iter()
                    .enumerate()
                    .map(|(n, s)| {
                        let state = match s.state(now) {
                            crate::hook::SubState::Running => "running",
                            crate::hook::SubState::Done => "done",
                            crate::hook::SubState::Failed => "failed",
                            crate::hook::SubState::Unknown => "no report",
                        };
                        crate::menu::PickerItem { label: format!("{} \u{b7} {}", s.kind, s.task), detail: state.into(), id: n }
                    })
                    .collect();
                Overlay::Picker(crate::menu::Picker::new("Subagents", items), Pick::Run(id, subs.into_iter().map(|s| s.id).collect()))
            }
        }
    }

    /// The next view the preview's plugin offers of this file; remembered for that plugin, so the next file opens in it.
    pub(super) fn next_view(&mut self) {
        let Some((by, next)) = self.preview_views().and_then(|(by, modes, mode)| {
            let at = modes.iter().position(|(id, _)| *id == mode).map_or(0, |i| i + 1) % modes.len();
            Some((by, modes.get(at)?.0.clone()))
        }) else {
            return;
        };
        self.preview_modes.insert(by, next);
        if let Some(w) = self.preview_asked.as_ref().map(|a| a.w.clone()) {
            self.ask_view(w);
        }
    }

    /// "Next view: Table" when the preview offers views, else just "Next view".
    pub(super) fn next_view_label(&self) -> String {
        match self.preview_views() {
            Some((_, modes, mode)) => {
                let at = modes.iter().position(|(id, _)| *id == mode).map_or(0, |i| i + 1) % modes.len();
                format!("Next view: {}", modes[at].1)
            }
            None => "Next view".into(),
        }
    }

    /// The plugin showing the preview, the views it offers (two or more) and the one shown.
    pub(super) fn preview_views(&self) -> Option<Views> {
        let asked = self.preview_asked.as_ref().filter(|a| a.by.0 && a.shown)?;
        match &self.tree.as_ref()?.shown_preview()?.kind {
            crate::preview::Kind::Styled { modes, mode, .. } if modes.len() > 1 => Some((asked.by.1.clone(), modes.clone(), mode.clone())),
            _ => None,
        }
    }

    /// The preview's wrap is a setting, so a key, the menu and the settings screen all move one value.
    pub(super) fn toggle_wrap(&mut self) {
        self.cfg.preview_wrap = !self.cfg.preview_wrap;
        self.config_changed();
    }

    /// The sidebar's state is a setting: the next start opens the way it was left.
    pub(super) fn toggle_sidebar(&mut self) {
        // On, but collapsed to its rail for lack of room: turning it off would only be remembered for a wider window.
        let a = crate::ui::areas(self, self.screen);
        if self.sidebar && a.sidebar.is_none() && a.rail.is_some() {
            self.toast = Some("the window is too narrow for the sidebar".into());
            self.dirty = true;
            return;
        }
        self.sidebar = !self.sidebar;
        self.cfg.sidebar_visible = self.sidebar;
        self.save_cfg_soon();
    }

    pub(super) fn config_changed(&mut self) {
        self.cfg = self.cfg.clone().clamped();
        self.apply_theme();
        self.save_cfg();
        self.dirty = true;
    }

    pub(super) fn picked(&mut self, id: usize, pick: Pick) -> Overlay {
        self.dirty = true;
        match pick {
            Pick::OpenWorktree(paths) => {
                let Some(path) = paths.get(id).cloned() else { return Overlay::None };
                // Already open: switch to it instead of spawning a second copy.
                if let Some(i) = self.spaces.iter().position(|s| s.cwd == path) {
                    self.active = i;
                } else {
                    self.open_space(path);
                }
            }
            // A takeover may have put another client on screen since the picker opened.
            Pick::Remote(_) | Pick::RemoteInstance { .. } if !self.caps.connects => return self.connect_overlay(),
            Pick::Remote(list) => {
                let Some(r) = list.get(id) else { return Overlay::None };
                if let Err(e) = r.check() {
                    self.notice = Some(format!("{} cannot be used: {e}", printable(&r.name)));
                    return Overlay::None;
                }
                crate::log::info(&format!("connect: asked client {} to list {}", self.client, r.name));
                self.to_client.extend(crate::proto::list_remote_marker(&r.name));
                self.remote_ask = Some((r.name.clone(), Instant::now() + REMOTE_WAIT));
                self.toast = Some(format!("asking {} for its servers…", r.name));
            }
            Pick::Run(pane, ids) => {
                if let Some(sub) = ids.get(id) {
                    self.open_run(pane, sub.clone());
                }
            }
            Pick::Report => {
                let Some((_, hours)) = REPORT_PERIODS.get(id) else { return Overlay::None };
                let peers: Vec<String> = self.clients.iter().filter_map(|c| c.host.clone()).collect();
                match crate::report::write(*hours, &self.cfg, &peers) {
                    Ok(path) => {
                        let shown = crate::paths::shown(&path);
                        crate::log::info(&format!("report saved: {hours} hours"));
                        let body = format!("Saved to {shown}. Nothing was sent.");
                        let dialog = crate::menu::Confirm::new("Logs exported", body);
                        // Finder opens on the server's Mac, which a window from another Mac cannot see.
                        if self.caps.bye {
                            return Overlay::Confirm(dialog.info(), Pending::Dismiss);
                        }
                        return Overlay::Confirm(dialog.labels(" [Enter] show in Finder ", " [Esc] close "), Pending::ShowInFinder(path));
                    }
                    Err(e) => self.notice = Some(format!("the report was not saved: {e}")),
                }
            }
            Pick::Topology(preset, mut picked, options, back) => {
                let preset = match preset {
                    None => id,
                    Some(p) => {
                        let Some(o) = options.get(id) else { return Overlay::None };
                        picked.push(o.clone());
                        p
                    }
                };
                return self.topology_next(preset, picked, back);
            }
            Pick::RemoteInstance { name, list, window } => {
                let Some(i) = list.get(id) else { return Overlay::None };
                if !i.running {
                    self.notice = Some(format!("server {0} on {name} is stopped; start it there with nerd --resume {0}", i.id));
                    return Overlay::None;
                }
                return self.open_remote(&name, Some(&i.id), window);
            }
        }
        Overlay::None
    }

    /// Close, asking first (with the pane count) when configured to.
    pub(super) fn confirm_overlay(&mut self, what: Pending) -> Overlay {
        let panes = self.panes_affected(&what);
        let title = match &what {
            Pending::ClosePane => "Close pane",
            Pending::CloseTab(_) => "Close tab",
            Pending::CloseSpace(_) => "Close space",
            Pending::CloseGroup(_) => "Close group",
            Pending::RemoveWorktree(_) => "Delete worktree checkout",
            Pending::DeletePath(_) => "Delete",
            Pending::Update => "Update nerd",
            // Never routed here: these dialogs write their own title and body.
            Pending::Detach => "Detach",
            Pending::DisconnectAll => "Disconnect everyone",
            Pending::LeaveTmux | Pending::CopyResumeCommand => "nerd is inside tmux",
            Pending::OpenRemote(_) | Pending::CopyCommand(_) | Pending::Dismiss => "Connect to another computer",
            Pending::ShowInFinder(_) => "Logs exported",
            Pending::InstallHook(_) => "Install the hook",
            Pending::RemoveHook => "Remove agent hooks",
            Pending::StopServer => "Stop this server",
            Pending::PluginReady(..) => "Install a plugin",
            Pending::RemovePlugin(_) => "Remove plugin",
            Pending::Rename(..) => "Rename pane",
            Pending::Topology(..) => "Set up a topology",
        };
        // The name of the thing goes on its own line, so the sentence carries the consequence
        // and nothing else. A body that is only a count answers a question nobody asked.
        let about = match &what {
            Pending::ClosePane => self
                .tab()
                .and_then(|t| t.panes.get(&t.layout.focused()))
                .map(Pane::label)
                .unwrap_or_default(),
            Pending::CloseTab(i) => {
                self.space().and_then(|sp| sp.tabs.get(*i)).map(|t| t.label(*i)).unwrap_or_default()
            }
            Pending::CloseSpace(i) | Pending::RemoveWorktree(i) => {
                self.spaces.get(*i).map(|s| s.cwd.display().to_string()).unwrap_or_default()
            }
            Pending::CloseGroup(key) => key.display().to_string(),
            Pending::DeletePath(p) => p.display().to_string(),
            _ => String::new(),
        };
        let body = match &what {
            Pending::DeletePath(p) => {
                if p.is_dir() { "The whole folder, with everything in it.".into() } else { "The file is gone for good.".into() }
            }
            Pending::RemoveWorktree(_) => {
                format!("The folder is removed, the branch stays. {} will close.", plural_panes(panes))
            }
            _ => format!(
                "{} will close, and what runs in {} is killed.",
                plural_panes(panes),
                if panes == 1 { "it" } else { "them" }
            ),
        };
        let verb = match &what {
            Pending::ClosePane => "close pane",
            Pending::CloseTab(_) => "close tab",
            Pending::CloseSpace(_) => "close space",
            Pending::CloseGroup(_) => "close group",
            Pending::RemoveWorktree(_) => "delete checkout",
            _ => "delete",
        };
        Overlay::Confirm(crate::menu::Confirm::new(title, body).does(verb).about(self.on_machine(about)).danger(true), what)
    }

    /// A danger dialog's About line names the Mac it acts on while a client from another one holds the screen.
    fn on_machine(&self, about: String) -> String {
        match &self.caps.machine {
            None => about,
            Some(m) if about.is_empty() => format!("On {m}."),
            Some(m) => format!("{about} on {m}"),
        }
    }

    fn panes_affected(&self, what: &Pending) -> usize {
        let of_space = |sp: &Space| sp.tabs.iter().map(|t| t.panes.len()).sum::<usize>();
        match what {
            Pending::ClosePane => 1,
            Pending::CloseTab(i) => self.space().and_then(|sp| sp.tabs.get(*i)).map(|t| t.panes.len()).unwrap_or(0),
            Pending::CloseSpace(i) | Pending::RemoveWorktree(i) => self.spaces.get(*i).map(of_space).unwrap_or(0),
            Pending::DeletePath(_) => 0,
            // Detach never reaches here: its dialog is built on its own.
            Pending::Detach | Pending::DisconnectAll | Pending::Update | Pending::LeaveTmux | Pending::CopyResumeCommand => 0,
            Pending::OpenRemote(_) | Pending::CopyCommand(_) | Pending::Dismiss | Pending::ShowInFinder(_) | Pending::InstallHook(_) | Pending::RemoveHook | Pending::StopServer | Pending::PluginReady(..) | Pending::RemovePlugin(_) | Pending::Rename(..) | Pending::Topology(..) => 0,
            Pending::CloseGroup(key) => self
                .spaces
                .iter()
                .filter(|sp| self.repos.get(&sp.cwd).is_some_and(|r| r.key.as_ref() == Some(key)))
                .map(of_space)
                .sum(),
        }
    }

    /// Detaching is a close, so it asks first — and asks on its own terms, because
    /// one stray key would otherwise take the whole session off the screen with nothing to undo.
    pub(super) fn detach_overlay(&mut self) -> Overlay {
        self.dirty = true;
        Overlay::Confirm(
            crate::menu::Confirm::new("Detach", "Close this window? The server and everything in it keep running.").does("detach"),
            Pending::Detach,
        )
    }

    /// Ends every connection, this one too; the About line names the sockets that never attached.
    pub(super) fn disconnect_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let about = match self.waiting.lock().map(|w| w.len()).unwrap_or(0) {
            0 => String::new(),
            1 => "This window and 1 connection that has not attached".to_string(),
            n => format!("This window and {n} connections that have not attached"),
        };
        Overlay::Confirm(
            crate::menu::Confirm::new("Disconnect everyone", "Disconnect everyone? This window closes too. Programs keep running.")
                .does("disconnect everyone")
                .about(self.on_machine(about))
                .danger(true),
            Pending::DisconnectAll,
        )
    }

    pub(super) fn about_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let id = crate::session::instance_name();
        let up = server_started().and_then(|t| t.elapsed().ok()).map(|d| format!(", running {}", uptime(d))).unwrap_or_default();
        let rows = vec![
            vec!["This server".into(), format!("{}{up}", crate::session::instance_label(&id))],
            vec!["Settings".into(), crate::paths::shown(&crate::paths::config_path())],
            vec!["Logs".into(), crate::paths::shown(&crate::session::state_dir().join("daemon.log"))],
            vec!["Licence".into(), "MIT".into()],
        ];
        let version = crate::version_line();
        let dialog = crate::menu::Confirm::new("About nerd", version.clone()).rows(rows).labels(" [Enter] copy version ", " [Esc] close ");
        Overlay::Confirm(dialog, Pending::CopyCommand(version))
    }

    /// The way back names the server's own Mac: a window from another one would otherwise read it as its own.
    pub(super) fn stop_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let panes: usize = self.spaces.iter().flat_map(|s| &s.tabs).map(|t| t.panes.len()).sum();
        let id = crate::session::instance_name();
        let back = if id.is_empty() { "nerd".to_string() } else { format!("nerd --resume {}", crate::session::instance_label(&id)) };
        let host = nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        let body = format!(
            "{} {}, and what runs in {} is killed. Start it again on {} with {back}; its spaces and the panes' screens are kept.",
            plural_panes(panes),
            if panes == 1 { "closes" } else { "close" },
            if panes == 1 { "it" } else { "them" },
            host.strip_suffix(".local").unwrap_or(&host),
        );
        Overlay::Confirm(crate::menu::Confirm::new("Stop this server", body).does("stop the server").danger(true).about(self.on_machine(String::new())), Pending::StopServer)
    }

    /// Update item / option+U: confirms first if a new build is installed, otherwise says there's nothing to update.
    pub(super) fn update_overlay(&mut self) -> Overlay {
        self.dirty = true;
        if self.update_available {
            let mut rows = vec![vec!["Running".to_string(), crate::version_line()], vec!["Available".to_string(), self.update_info.clone()]];
            if !self.update_changes.is_empty() {
                rows.push(vec![String::new(), String::new()]);
                rows.extend(self.update_changes.iter().cloned());
            }
            let confirm = crate::menu::Confirm::new("Update nerd", "Panes, agents and history stay open.")
                .rows(rows)
                .labels(" [Enter] update now ", " [Esc] later ")
                .other(" [s] skip this build ");
            Overlay::Confirm(confirm, Pending::Update)
        } else {
            self.toast = Some(format!("no new build is installed: {} is running", crate::version_line()));
            Overlay::None
        }
    }

    /// The hook goes into an agent's config only after the human has read where and what resumes it.
    pub(super) fn install_hook_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let ask = crate::hook::first_installs();
        Overlay::Confirm(crate::menu::Confirm::new("Install the hook", ask.intro()).table(ask.table()).does("install hooks"), Pending::InstallHook(ask.summary()))
    }

    pub(super) fn remove_hook_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let confirm = crate::menu::Confirm::new("Remove agent hooks", "Agents already running keep calling the hook until they restart.")
            .table(crate::hook::installed_table())
            .does("remove hooks")
            .danger(true);
        Overlay::Confirm(confirm, Pending::RemoveHook)
    }

    /// What the Settings screen asked for; the screen stays open unless the outcome leaves it.
    pub(super) fn settings_outcome(&mut self, s: crate::settings::Settings, outcome: crate::settings::SettingsOutcome) -> Overlay {
        use crate::settings::SettingsOutcome as O;
        match outcome {
            O::Close => Overlay::None,
            O::InstallHook => self.install_hook_overlay(),
            O::RemoveHook => self.remove_hook_overlay(),
            O::EditConfig if self.caps.bye => {
                let host = nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
                self.notice = Some(format!("config.json is on {}: {}", host.strip_suffix(".local").unwrap_or(&host), crate::paths::shown(&crate::paths::config_path())));
                Overlay::None
            }
            O::EditConfig => {
                match crate::links::open_text(&crate::paths::config_path()) {
                    Ok(()) => self.toast = Some("opened".to_string()),
                    Err(e) => self.notice = Some(e),
                }
                Overlay::None
            }
            O::Changed => {
                self.config_changed();
                Overlay::Settings(s)
            }
            O::Mail(a) => {
                self.mail_done(a);
                Overlay::Settings(s)
            }
            O::Topology => self.topology_overlay(None),
            O::Accounts => Overlay::Accounts(crate::isolation_ui::Accounts::default()),
            O::None => Overlay::Settings(s),
        }
    }

    /// The notice in full, for one too long for its card; it closes with the window.
    /// A card that opens a window of its own, while it still shows.
    pub fn card_more(&self) -> Option<More> {
        self.toast_more.as_ref().filter(|(t, _)| self.notice.is_none() && self.toast.as_ref() == Some(t)).map(|(_, m)| *m)
    }

    /// A card with "… more" that opens `more`.
    pub fn toast_with_more(&mut self, text: String, more: More) {
        self.toast_more = Some((text.clone(), more));
        self.toast = Some(text);
    }

    pub(super) fn notice_window(&mut self) -> Overlay {
        self.dirty = true;
        if let Some(more) = self.card_more() {
            self.toast = None;
            return match more {
                More::Update => self.update_overlay(),
                More::Updated => self.updated_overlay(),
            };
        }
        let (title, text) = match (self.notice.take(), self.toast.take()) {
            (Some(n), _) => ("What went wrong", n),
            (None, Some(t)) => ("Message", t),
            (None, None) => return Overlay::None,
        };
        let text = crate::render::unspoofed(&text);
        Overlay::Confirm(crate::menu::Confirm::new(title, text).info(), Pending::Dismiss)
    }

    fn remove_plugin(&mut self, name: &str, data: bool) {
        match crate::install::remove(name, data) {
            Ok(()) => self.toast = Some(format!("{name} removed{}", if data { " with its data" } else { "" })),
            Err(e) => self.notice = Some(e),
        }
    }

    /// A dialog answered No: only the first hook install is logged.
    pub(super) fn declined(&mut self, what: &Pending) {
        if let Pending::InstallHook(first) = what {
            crate::log::info(&format!("first hook install declined: {first}"));
        }
        if let Pending::PluginReady(id, _) = what {
            crate::log::info(&format!("staged plugin {id} declined"));
            crate::install::discard(id);
        }
    }

    /// The third button of a dialog that offers one.
    /// What this build brought over the one before, after an update.
    pub(super) fn updated_overlay(&mut self) -> Overlay {
        let body = match &self.updated_changes {
            None => "The build before did not say which it was.",
            Some(rows) if rows.is_empty() => "Nothing new over the build before.",
            Some(_) => "What is new in this build.",
        };
        let title = format!("Updated to {}", crate::changes::short_version());
        Overlay::Confirm(crate::menu::Confirm::new(title, body).rows(self.updated_changes.clone().unwrap_or_default()).info(), Pending::Dismiss)
    }

    pub(super) fn perform_other(&mut self, what: Pending) {
        self.dirty = true;
        if let Pending::Rename(pane, name) = &what {
            self.mail_done(MailAction::Rename { pane: *pane, name: Some(name.clone()), keep: false });
        }
        if let Pending::RemovePlugin(n) = &what {
            self.remove_plugin(n, true);
        }
        if matches!(what, Pending::Update) {
            // Only this build: the next one is said again.
            let _ = std::fs::write(crate::paths::state_root().join("update-skipped"), &self.update_info);
            self.update_skipped = true;
        }
        if matches!(what, Pending::LeaveTmux | Pending::CopyResumeCommand) {
            self.cfg.tmux = crate::config::TmuxOffer::Stay;
            self.config_changed();
        }
    }

    pub(super) fn perform(&mut self, what: Pending) {
        self.dirty = true;
        match what {
            Pending::Update => self.upgrade = true,
            Pending::StopServer => self.stop_server = true,
            Pending::RemovePlugin(n) => self.remove_plugin(&n, false),
            // What is placed is what was asked about: the same commit, checked again now.
            Pending::PluginReady(id, asked) => match crate::install::load_ready(&id, false)
                .and_then(|r| if r.staged.pin.commit == asked { Ok(r) } else { Err("its files changed after the question".into()) })
                .and_then(crate::install::finish_ready)
            {
                Ok(done) => self.toast = Some(done),
                Err(e) => {
                    crate::install::discard(&id);
                    self.notice = Some(format!("the plugin was not installed: {}", crate::render::unspoofed(&e).lines().next().unwrap_or_default()));
                }
            },
            Pending::Detach => self.quit = Some("detached by the user"),
            Pending::DisconnectAll => self.disconnect_all = true,
            Pending::LeaveTmux => self.open_outside_tmux(),
            Pending::CopyResumeCommand => {
                let cmd = self.resume_command();
                self.to_client.extend(crate::select::copy(&cmd));
                self.toast = Some(format!("copied: {cmd}"));
            }
            Pending::CopyCommand(cmd) => {
                self.to_client.extend(crate::select::copy(&cmd));
                self.toast = Some(format!("copied: {cmd}"));
            }
            Pending::OpenRemote(name) if self.caps.connects => {
                let _ = self.open_remote(&name, None, true);
            }
            Pending::OpenRemote(_) => self.notice = Some("this window cannot open another; connect from a nerd window on this Mac".into()),
            Pending::Dismiss => {}
            Pending::Rename(pane, name) => {
                self.mail_done(MailAction::Rename { pane, name: Some(name), keep: true });
            }
            Pending::Topology(routes, _) => {
                self.mail_done(MailAction::Topology(routes));
            }
            Pending::InstallHook(first) => {
                crate::log::info(&format!("first hook install confirmed: {first}"));
                match crate::hook::install() {
                    Ok(done) => {
                        let failed: Vec<String> = done.iter().filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}"))).collect();
                        if failed.is_empty() {
                            self.toast = Some(format!("hook installed: {}", done.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")));
                        } else {
                            self.notice = Some(failed.join("; "));
                        }
                    }
                    Err(e) => self.notice = Some(format!("couldn't install the hook: {e}")),
                }
            }
            Pending::RemoveHook => match crate::hook::uninstall() {
                Ok(done) => self.toast = Some(format!("hooks removed from {}", done.join(", "))),
                Err(e) => self.notice = Some(format!("couldn't remove the hooks: {e}")),
            },
            Pending::ShowInFinder(path) => {
                if let Err(e) = crate::spawn::detached(crate::spawn::cmd("/usr/bin/open").arg("-R").arg(&path)) {
                    self.notice = Some(format!("Finder did not open: {e}"));
                }
            }
            Pending::ClosePane => self.close_pane(),
            Pending::CloseTab(i) => {
                if let Some(t) = self.space().and_then(|sp| sp.tabs.get(i)) {
                    crate::log::info(&format!("tab closed by the user with {}", plural_panes(t.panes.len())));
                }
                self.close_tab(self.active, i);
            }
            Pending::CloseSpace(i) => {
                if let Some(n) = self.space_panes(i) {
                    crate::log::info(&format!("space closed by the user with {}", plural_panes(n)));
                }
                self.close_space(i);
            }
            Pending::DeletePath(path) => {
                let Some(tree) = self.tree.as_mut() else { return };
                match tree.delete(&path) {
                    Ok(()) => {
                        tree.reload();
                        self.refresh_tree_marks();
                    }
                    Err(e) => self.notice = Some(e),
                }
            }
            Pending::RemoveWorktree(i) => {
                let Some(cwd) = self.spaces.get(i).map(|s| s.cwd.clone()) else { return };
                // We run the command from the main copy: git refuses from the folder being deleted.
                let main = crate::git::group_key(&cwd)
                    .and_then(|g| g.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_else(|| cwd.clone());
                crate::log::info(&format!("worktree checkout deleted by the user; its space closed with {}", plural_panes(self.space_panes(i).unwrap_or(0))));
                // Close panes first: their shells sit in this folder.
                self.close_space(i);
                if let Err(e) = crate::git::remove_worktree(&main, &cwd) {
                    // git won't remove a dirty tree; we show its own words.
                    self.notice = Some(e);
                }
                // Another space on the same folder keeps what git said about it.
                if !self.spaces.iter().any(|s| s.cwd == cwd) {
                    self.repos.remove(&cwd);
                }
            }
            Pending::CloseGroup(key) => {
                let members: Vec<usize> = (0..self.spaces.len())
                    .filter(|&i| self.repos.get(&self.spaces[i].cwd).is_some_and(|r| r.key.as_ref() == Some(&key)))
                    .collect();
                let n: usize = members.iter().filter_map(|&i| self.space_panes(i)).sum();
                crate::log::info(&format!("group closed by the user: {} with {}", plural_spaces(members.len()), plural_panes(n)));
                // From the end, so indices ahead don't shift.
                for i in members.into_iter().rev() {
                    self.close_space(i);
                }
                self.collapsed_groups.remove(&key);
            }
        }
    }

    /// Tree: hidden -> open and take focus; open without focus -> take focus; open with focus -> close.
    pub fn toggle_tree(&mut self) {
        match (&self.tree, self.focus == Focus::Tree) {
            (None, _) => {
                let cwd = self.space().map(|s| s.cwd.clone()).unwrap_or_default();
                self.tree = Some(crate::tree::Tree::new(cwd));
                self.focus = Focus::Tree;
                self.refresh_tree_marks();
            }
            (Some(_), false) => self.focus = Focus::Tree,
            (Some(_), true) => self.close_tree(),
        }
        self.dirty = true;
    }

    pub fn close_tree(&mut self) {
        self.tree_zoom = false;
        self.tree = None;
        self.focus = Focus::Panes;
        self.view.tree_rect = None;
        self.view.tree_frame = None;
        self.view.preview_rect = None;
        self.view.tree = Default::default();
        // The image, if any, lives in the terminal on its own; we clear it.
        if self.graphics && !self.caps.mux {
            self.to_client.extend(crate::preview::clear_image_bytes(1));
        }
        self.dirty = true;
    }

    /// Runs a tree outcome that opens a prompt or confirmation, returning the overlay it set.
    fn tree_ask(&mut self, o: crate::tree::Outcome) -> Overlay {
        self.tree_outcome(o);
        std::mem::replace(&mut self.overlay, Overlay::None)
    }

    pub(super) fn tree_outcome(&mut self, o: crate::tree::Outcome) {
        use crate::tree::Outcome;
        match o {
            Outcome::None => {}
            Outcome::Close => self.focus = Focus::Panes,
            Outcome::Zoom => self.tree_zoom = !self.tree_zoom && self.cfg.preview,
            Outcome::Wrap => self.toggle_wrap(),
            Outcome::View => self.next_view(),
            Outcome::Hide => self.close_tree(),
                // Editor opens in the active pane, since the human pressed `e` themselves.
            Outcome::OpenInEditor(p, line) => {
                self.plugins.emit(crate::plugins::Event::FileChosen { path: p.clone() });
                let editor = std::env::var("EDITOR").ok().filter(|e| !e.is_empty()).unwrap_or_else(|| "vi".to_string());
                let at = line.map(|l| format!("+{l} ")).unwrap_or_default();
                match shell_word(&p.to_string_lossy()) {
                    Some(w) => self.pane_type(&format!("{editor} {at}{w}\n")),
                    None => self.notice = Some(CONTROL_NAME.into()),
                }
                self.focus = Focus::Panes;
            }
            Outcome::InsertPath(p) => {
                match shell_word(&p.to_string_lossy()) {
                    Some(w) => self.pane_type(&w),
                    None => self.notice = Some(CONTROL_NAME.into()),
                }
                self.focus = Focus::Panes;
            }
            Outcome::Cd(p) => {
                match shell_word(&p.to_string_lossy()) {
                    Some(w) => self.pane_type(&format!("cd {w}\n")),
                    None => self.notice = Some(CONTROL_NAME.into()),
                }
                self.focus = Focus::Panes;
            }
            // A workspace of its own, with its shell; the tree follows the new space's folder.
            Outcome::NewSpace(p) => {
                self.open_space(p);
                self.focus = Focus::Panes;
            }
            Outcome::OpenSystem(p) => {
                self.plugins.emit(crate::plugins::Event::FileChosen { path: p.clone() });
                let cwd = self.space().map(|s| s.cwd.clone()).unwrap_or_default();
                let link = crate::links::Link::Path { path: p.display().to_string(), line: None };
                match crate::links::open(&link, &cwd) {
                    Ok(()) => self.toast = Some("opened".to_string()),
                    Err(e) => self.notice = Some(e),
                }
            }
            Outcome::CopyPath(p) => {
                let text = p.display().to_string();
                self.to_client.extend(crate::select::copy(&text));
                self.toast = Some(format!("copied: {text}"));
            }
            Outcome::AskCreate(dir) => {
                self.overlay = Overlay::Prompt(crate::menu::Prompt::new("New file (name/ = folder)", "").does("create"), Ask::CreateIn(dir));
            }
            Outcome::AskRename(p) => {
                let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                self.overlay = Overlay::Prompt(crate::menu::Prompt::new("Rename", name).does("rename"), Ask::RenamePath(p));
            }
            Outcome::AskDelete(p) => self.overlay = self.confirm_overlay(Pending::DeletePath(p)),
        }
    }

    /// To the panel's plugin over its socket; no built-in has panels.
    pub(super) fn panel_event(&mut self, src: &crate::slots::Src, ev: crate::plugins::Event) {
        if src.0
            && let Err(e) = self.plugins.command(&src.1, ev)
        {
            self.notice = Some(e);
        }
    }

    /// A chosen row goes to the panel's source and the panel closes.
    pub(super) fn plugins_outcome(&mut self, w: crate::plugin_list::PluginList, out: crate::plugin_list::Out) -> Overlay {
        match out {
            crate::plugin_list::Out::None => Overlay::Plugins(w),
            crate::plugin_list::Out::Close => {
                // What a finished fetch said has been read; one still running keeps going.
                if self.plugin_job.as_ref().is_some_and(|j| j.done.is_some()) {
                    self.plugin_job = None;
                }
                Overlay::None
            }
            crate::plugin_list::Out::Act(name, act) => {
                use crate::plugin_list::Act;
                let ctx = MenuCtx::Plugin(name.clone());
                match act {
                    Act::Add => self.run_action(Action::PluginAdd, &MenuCtx::Global),
                    Act::Refresh => {
                        self.plugin_fetch(Act::Refresh, String::new(), &["--plugin-catalog"]);
                        self.plugins_overlay()
                    }
                    Act::Cancel => {
                        if let (Some(j), Some(b)) = (self.plugin_job.take(), crate::broker::exec_handle()) {
                            b.stop_exec(j.exec);
                        }
                        let mut w = w;
                        w.job = None;
                        Overlay::Plugins(w)
                    }
                    Act::Install => self.run_action(Action::PluginInstall, &ctx),
                    Act::Update => self.run_action(Action::PluginUpdate, &ctx),
                    // A failed fetch's row shows git's output, not the plugin's.
                    Act::Log if self.plugin_job.as_ref().is_some_and(|j| j.failed() && (name.is_empty() || j.name == name)) => {
                        self.open_tab("git log".into(), vec!["/usr/bin/less".into(), "-L".into(), "+G".into(), crate::install::fetch_log().to_string_lossy().into_owned()]);
                        Overlay::None
                    }
                    Act::Log => self.run_action(Action::PluginLog, &ctx),
                    // In place: the same as the menu's Reload plugins and agents, then the window again.
                    Act::Reload => {
                        let _ = self.run_action(Action::ReloadConfig, &ctx);
                        self.plugins_overlay_at(&name)
                    }
                    // In place: the window stays, the row says what changed.
                    Act::Off | Act::On => {
                        let _ = self.run_action(if act == Act::On { Action::PluginOn } else { Action::PluginOff }, &ctx);
                        self.plugins_overlay_at(&name)
                    }
                    // Asked first; the window comes back once it is answered either way.
                    Act::Remove => {
                        self.plugins_back = Some(name.clone());
                        self.run_action(Action::PluginRemove, &ctx)
                    }
                }
            }
            crate::plugin_list::Out::Menu(name, true, at) => self.plugin_menu(&name, at),
            crate::plugin_list::Out::Menu(name, false, at) => {
                use crate::menu::{Item, Menu};
                Overlay::Menu(Menu::new(vec![Item::new(format!("Install {name}…"), Action::PluginInstall)], at), MenuCtx::Plugin(name))
            }
        }
    }

    pub(super) fn panel_outcome(&mut self, p: crate::panel::Open, out: crate::panel::Outcome) -> Overlay {
        match out {
            crate::panel::Outcome::None => Overlay::Panel(p),
            crate::panel::Outcome::Close => Overlay::None,
            crate::panel::Outcome::Chosen(item) => {
                self.panel_event(&p.src, crate::plugins::Event::PanelItem { panel: p.id, item });
                Overlay::None
            }
            crate::panel::Outcome::Opened(item) => {
                // A plugin that cannot hear it would leave the panel waiting for content that never comes.
                if p.src.0
                    && let Err(e) = self.plugins.command(&p.src.1, crate::plugins::Event::PanelItem { panel: p.id.clone(), item })
                {
                    self.notice = Some(e);
                    return Overlay::None;
                }
                Overlay::Panel(p)
            }
        }
    }

    /// After creating or renaming: reread and land on the result.
    fn tree_after_change(&mut self, p: &std::path::Path) {
        if let Some(tree) = self.tree.as_mut() {
            tree.reload();
            tree.select_path(p);
        }
        self.refresh_tree_marks();
    }

    /// Open the navigator across all panes (option+n).
    pub fn open_navigator(&mut self) {
        use crate::navigator::{Entry, Navigator};
        let focused = self.tab().map(|t| t.layout.focused());
        let mut entries = Vec::new();
        for (si, sp) in self.spaces.iter().enumerate() {
            for (ti, tab) in sp.tabs.iter().enumerate() {
                let mut ids = tab.layout.pane_ids();
                ids.sort_unstable();
                for id in ids {
                    let Some(p) = tab.panes.get(&id) else { continue };
                    entries.push(Entry {
                        space: si,
                        tab: ti,
                        pane: p.id,
                        space_name: sp.label(),
                        tab_label: tab.name.clone().unwrap_or_else(|| (ti + 1).to_string()),
                        title: p.label.clone().or_else(|| p.agent.clone()).unwrap_or_else(|| p.command.clone()),
                        state: p.term.state(),
                        is_current: si == self.active && ti == sp.active_tab && Some(p.id) == focused,
                    });
                }
            }
        }
        self.overlay = Overlay::Navigator(Navigator::new(entries));
        self.dirty = true;
    }

    pub(super) fn split(&mut self, dir: Dir) {
        let Some(t) = self.tab() else { return };
        // A split is a plain shell in the pane's folder; a second agent is started deliberately, not via split.
        let Some(cwd) = t.panes.get(&t.layout.focused()).map(|p| p.cwd.clone()) else { return };
        let iso = self.isolation_for_new(self.active, self.spaces.get(self.active).map(|sp| sp.active_tab), Some(t.layout.focused()));
        if let Some(pane) = self.spawn_pane(cwd, shell_argv(), iso) {
            let id = pane.id;
            if let Some(t) = self.tab_mut() {
                t.layout.split(dir, id);
                t.panes.insert(id, pane);
                self.dirty = true;
            }
        }
    }

    /// A new tab is a plain terminal, not another agent; agents start from settings.
    pub(super) fn new_tab(&mut self) {
        self.new_tab_in(self.active);
    }

    fn new_tab_in(&mut self, si: usize) {
        let cwd = self
            .spaces
            .get(si)
            .and_then(|sp| sp.tab())
            .and_then(|t| t.panes.get(&t.layout.focused()))
            .map(|p| p.cwd.clone())
            .or_else(|| self.spaces.get(si).map(|sp| sp.cwd.clone()))
            .or_else(crate::paths::home)
            .unwrap_or_default();
        let name = cwd
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".to_string());

        let iso = self.isolation_for_new(si, None, None);
        if let Some(pane) = self.spawn_pane(cwd, shell_argv(), iso) {
            let layout = Layout::new(pane.id);
            let mut panes = HashMap::new();
            panes.insert(pane.id, pane);
            if let Some(sp) = self.spaces.get_mut(si) {
                sp.tabs.push(Tab { name: Some(name), layout, panes, isolation_new: None });
                sp.active_tab = sp.tabs.len() - 1;
                self.dirty = true;
            }
        }
    }

    fn close_pane(&mut self) {
        let Some(id) = self.tab().map(|t| t.layout.focused()) else { return };
        crate::log::info(&format!("pane {id} closed by the user"));
        self.drop_pane(id, None);
    }

    fn space_panes(&self, i: usize) -> Option<usize> {
        self.spaces.get(i).map(|sp| sp.tabs.iter().map(|t| t.panes.len()).sum())
    }

    pub fn space_menu(&self, index: usize, at: (u16, u16)) -> Option<Overlay> {
        use crate::git::RepoKind;
        use crate::menu::{Item, Menu};
        let sp = self.spaces.get(index)?;
        let kind = crate::git::repo_kind(&sp.cwd);
        // Group parent: there are other workspaces with the same `.git`.
        let key = self.repos.get(&sp.cwd).and_then(|r| r.key.as_ref());
        let in_group = kind == RepoKind::Main
            && key.is_some_and(|key| self.spaces.iter().filter(|s| self.repos.get(&s.cwd).is_some_and(|r| r.key.as_ref() == Some(key))).count() > 1);
        let mut git = Vec::new();
        if kind == RepoKind::Main {
            git.push(Item::new("New worktree…", Action::NewWorktree));
            git.push(Item::new("Open worktree…", Action::OpenWorktree));
        }
        if in_group {
            git.push(Item::new(if key.is_some_and(|k| self.collapsed_groups.contains(k)) { "Expand group" } else { "Collapse group" }, Action::ToggleGroup));
        }
        let mut close = vec![Item::new("Close space…", Action::Close)];
        if in_group {
            close.push(Item::new("Close group…", Action::CloseGroup));
        }
        if kind == RepoKind::Linked {
            close.push(Item::new("Delete worktree checkout…", Action::RemoveWorktree));
        }
        let items = grouped(vec![
            vec![Item::new("New tab in this space", Action::NewTab)],
            git,
            vec![Item::new("Isolation for new panes…", Action::IsolationNew)],
            vec![
                Item::new("Rename space…", Action::Rename),
                Item::new("Move up", Action::MoveUp).enabled(self.space_neighbour(index, -1).is_some()),
                Item::new("Move down", Action::MoveDown).enabled(self.space_neighbour(index, 1).is_some()),
            ],
            close,
        ]);
        Some(Overlay::Menu(Menu::new(items, at), MenuCtx::Space(index)))
    }

    pub fn tab_menu(&self, index: usize, at: (u16, u16)) -> Overlay {
        use crate::menu::{Item, Menu};
        let items = grouped(vec![
            vec![Item::new("New tab", Action::NewTab).key(global_label(KeyActionName::Act(KeyAction::NewTab)))],
            vec![Item::new("Isolation for new panes…", Action::IsolationNew)],
            vec![Item::new("Rename tab…", Action::Rename), Item::new("Close tab…", Action::Close)],
        ]);
        Overlay::Menu(Menu::new(items, at), MenuCtx::Tab(index))
    }

    pub fn pane_menu(&self, id: PaneId, at: (u16, u16)) -> Overlay {
        use crate::menu::{Item, Menu};
        // The pane's own tab, not the active one: a sidebar row can name a pane in another space/tab.
        let layout = self.locate(id).map(|(si, ti)| &self.spaces[si].tabs[ti].layout);
        let split = layout.is_some_and(|l| l.pane_count() > 1);
        let zoomed = layout.is_some_and(|l| l.is_zoomed());
        let pane = self.pane(id);
        let mut view = vec![
            Item::new(if zoomed { "Leave full screen" } else { "Full screen" }, Action::Zoom).key(global_label(KeyActionName::Act(KeyAction::Zoom))).enabled(split),
            Item::new("Search in pane", Action::Search).key(global_label(KeyActionName::Surface(3))),
        ];
        if let Some(p) = pane.filter(|p| p.agent.is_some()) {
            view.push(Item::new("Subagents…", Action::Subagents).enabled(!p.activity.subs.is_empty()));
        }
        let mut mail = vec![
            Item::new("Send message…", Action::SendMessage).enabled(self.address(id).is_some()),
            Item::new("Show inbox…", Action::ShowInbox),
            Item::new("Who can write here…", Action::WhoCanWrite),
            Item::new("Where it can write…", Action::WhereCanWrite),
        ];
        // Settings' Agent hooks row is where the owner acts on it.
        if pane.and_then(|p| p.agent.as_deref()).is_some_and(|a| crate::agents::get(a).is_some_and(|m| m.agent.mail.is_some()) && !crate::hook::hook_current(a)) {
            mail.push(Item::new("Install the hook for exact delivery", Action::Settings).enabled(false));
        }
        let items = grouped(vec![
            vec![
                Item::new("Split right", Action::SplitRight).key(global_label(KeyActionName::Act(KeyAction::SplitSide))),
                Item::new("Split down", Action::SplitDown).key(global_label(KeyActionName::Act(KeyAction::SplitDown))),
            ],
            view,
            mail,
            vec![Item::new("Isolation…", Action::Isolation)],
            plugin_group(crate::agents::Place::Pane, self.builtins),
            vec![
                Item::new("Rename pane…", Action::RenamePane),
                Item::new("Close pane…", Action::ClosePane).key(global_label(KeyActionName::Act(KeyAction::ClosePane))),
            ],
        ]);
        Overlay::Menu(Menu::new(items, at), MenuCtx::Pane(id))
    }

    pub fn global_menu(&self, at: (u16, u16)) -> Overlay {
        use crate::menu::{Item, Menu};
        let mut server = vec![
            Item::new("Messages…", Action::Messages),
            Item::new("Routes map…", Action::RoutesMap),
            Item::new("Rename this server…", Action::RenameInstance),
            Item::new("Connect to another computer…", Action::Connect),
            Item::new("Session history…", Action::Sessions),
        ];
        if self.caps.mux {
            server.push(Item::new(format!("Open outside {}…", self.mux_name()), Action::TmuxWindow));
        }
        let items = grouped(vec![
            vec![
                Item::new("New space…", Action::NewSpace),
                Item::new("New tab", Action::NewTab).key(global_label(KeyActionName::Act(KeyAction::NewTab))),
                Item::new("Navigator…", Action::Navigator).key(global_label(KeyActionName::Surface(5))),
            ],
            vec![
                Item::new(if self.tree.is_some() { "Hide file tree" } else { "Show file tree" }, Action::ToggleTree).key(global_label(KeyActionName::Surface(4))),
                Item::new(if self.sidebar { "Collapse sidebar" } else { "Expand sidebar" }, Action::ToggleSidebar).key(global_label(KeyActionName::Act(KeyAction::Sidebar))),
            ],
            vec![
                Item::new("Settings…", Action::Settings).key(global_label(KeyActionName::Surface(6))),
                Item::new("Keys and mouse…", Action::Keybinds).key(global_label(KeyActionName::Surface(1))),
                Item::new("Plugins…", Action::Plugins),
                Item::new("Reload plugins and agents", Action::ReloadConfig),
            ],
            server,
            vec![
                Item::new("About nerd…", Action::About),
                Item::new("Update…", Action::Update).key(global_label(KeyActionName::Act(KeyAction::Update))).mark(self.update_available && !self.update_skipped),
                Item::new("Export logs…", Action::Report),
            ],
            plugin_group(crate::agents::Place::Global, self.builtins),
            vec![
                Item::new("Detach…", Action::Detach).key(global_label(KeyActionName::Act(KeyAction::Detach))),
                Item::new("Disconnect everyone…", Action::DisconnectAll),
                Item::new("Stop this server…", Action::StopServer),
            ],
        ]);
        Overlay::Menu(Menu::new(items, at), MenuCtx::Global)
    }

    /// Right-click menu for a tree row: every tree action, reachable without a keyboard shortcut.
    /// The Plugins window with this plugin's row chosen.
    pub(super) fn plugins_overlay_at(&mut self, name: &str) -> Overlay {
        let mut o = self.plugins_overlay();
        if let Overlay::Plugins(w) = &mut o {
            w.choose(name);
        }
        o
    }

    /// Menu > Plugins…, read afresh each time it opens.
    pub(super) fn plugins_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let running = |n: &str| self.plugins.state_of(n);
        let (catalog, _) = crate::catalog::read();
        let list = crate::plugin_list::entries(&catalog, running);
        Overlay::Plugins(crate::plugin_list::PluginList::new(list, catalog).with_job(self.plugin_job.clone()))
    }

    /// The open Plugins window read afresh, its chosen row kept.
    pub(super) fn plugins_renew(&mut self) {
        if !matches!(self.overlay, Overlay::Plugins(_)) {
            return;
        }
        if let Overlay::Plugins(fresh) = self.plugins_overlay()
            && let Overlay::Plugins(w) = &mut self.overlay
        {
            w.renew(fresh);
        }
    }

    /// A plugin's own menu in the Plugins window.
    pub(super) fn plugin_menu(&self, name: &str, at: (u16, u16)) -> Overlay {
        use crate::menu::{Item, Menu};
        let pinned = crate::install::read_lock().unwrap_or_default().iter().any(|p| p.name == name);
        let off = crate::install::read_off().ok().is_some_and(|o| o.contains_key(name));
        let mut tools = Vec::new();
        if pinned {
            tools.push(Item::new(format!("Update {name}…"), Action::PluginUpdate));
        }
        tools.push(Item::new(format!("Show {name}'s log"), Action::PluginLog));
        let items = grouped(vec![
            tools,
            vec![if off { Item::new(format!("Turn {name} on"), Action::PluginOn) } else { Item::new(format!("Turn {name} off"), Action::PluginOff) }],
            vec![Item::new(format!("Remove {name}…"), Action::PluginRemove)],
        ]);
        Overlay::Menu(Menu::new(items, at), MenuCtx::Plugin(name.into()))
    }

    /// `nerd <args> --window`, run by the broker outside the sandbox with no tab and no terminal: it only stages what
    /// it fetched, and writes to the fetch log. The Plugins window shows it as its row's state.
    pub(super) fn plugin_fetch(&mut self, act: crate::plugin_list::Act, name: String, args: &[&str]) {
        if self.plugin_job.as_ref().is_some_and(|j| j.done.is_none()) {
            self.notice = Some("git is still fetching for the Plugins window: wait for it, or cancel it there".into());
            return;
        }
        let exec = self.plugins.take_id();
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "nerd".into());
        let argv = std::iter::once(exe).chain(args.iter().map(|a| a.to_string())).chain(["--window".into()]).collect();
        let started = match (crate::broker::handle(), crate::broker::exec_handle()) {
            (Some(_), _) if crate::broker::exec_coming() => Err("plugins are starting; try again in a moment".to_string()),
            (Some(_), None) => Err(crate::plugins::OLD_BROKER.to_string()),
            (Some(_), Some(b)) => b.exec(exec, argv, Vec::new(), crate::paths::home().unwrap_or_default()).map_err(|e| format!("the broker did not take it: {e}")),
            // Tests run with no broker: the job stays as started.
            (None, _) if cfg!(test) => Ok(()),
            (None, _) => Err("no broker to run git".into()),
        };
        self.plugin_job = Some(crate::plugin_list::Job { act, name, exec, done: started.err().map(Err) });
    }

    /// A new tab in this space running `argv`.
    fn open_tab(&mut self, name: String, argv: Vec<String>) {
        let cwd = crate::paths::home().unwrap_or_default();
        let iso = self.isolation_for_new(self.active, None, None);
        if let Some(pane) = self.spawn_pane(cwd, argv, iso) {
            let layout = Layout::new(pane.id);
            let mut panes = HashMap::new();
            panes.insert(pane.id, pane);
            if let Some(sp) = self.spaces.get_mut(self.active) {
                sp.tabs.push(Tab { name: Some(name), layout, panes, isolation_new: None });
                sp.active_tab = sp.tabs.len() - 1;
                self.focus = Focus::Panes;
                self.dirty = true;
            }
        }
    }

    pub fn tree_menu(&self, path: PathBuf, is_dir: bool, at: (u16, u16)) -> Overlay {
        use crate::menu::{Item, Menu};
        use crossterm::event::KeyCode::{Backspace, Char};
        use crate::tree::key_label as k;
        let t = self.tree.as_ref();
        let changes = t.is_some_and(|t| t.view() == crate::tree::View::Changes);
        let what = if is_dir {
            vec![
                Item::new("Go to this folder in the pane", Action::TreeCd).key(k(Char('d'), true)),
                Item::new("Open as a space", Action::TreeSpace).key(k(Char('p'), true)),
                Item::new("Make it the root", Action::TreeRoot).key(k(Char('='), true)),
                Item::new("New file or folder…", Action::TreeNew).key(k(Char('a'), true)),
                Item::new("Open in Finder", Action::TreeOpen).key(k(Char('o'), true)),
                Item::new("Copy path", Action::TreeCopy).key(k(Char('y'), true)),
            ]
        } else {
            vec![
                Item::new("Open in editor", Action::TreeEdit).key(k(Char('e'), true)),
                Item::new("Open in its app", Action::TreeOpen).key(k(Char('o'), true)),
                Item::new("Insert path into pane", Action::TreeInsert).key(k(Char('i'), true)),
                Item::new("Copy path", Action::TreeCopy).key(k(Char('y'), true)),
            ]
        };
        let mut view = vec![Item::new(if changes { "Show files" } else { "Show changes" }, Action::TreeView)];
        view.push(Item::new(if t.is_some_and(|t| t.hidden()) { "Hide hidden files" } else { "Show hidden files" }, Action::TreeHidden).key(k(Char('h'), true)));
        view.push(Item::new("Filter names…", Action::TreeFilter));
        view.push(Item::new("Search contents…", Action::TreeGrep).key(k(Char('g'), true)));
        view.push(Item::new(if t.is_some_and(|t| t.changes_flat()) { "Changes as a tree" } else { "Changes as a list" }, Action::TreeShape).enabled(changes));
        let moves = vec![Item::new("Root one level up", Action::TreeUp).key(k(Backspace, false)), Item::new("Reload", Action::TreeReload).key(k(Char('r'), true))];
        let items = grouped(vec![
            what,
            view,
            moves,
            vec![
                Item::new(if self.tree_zoom { "Leave full screen" } else { "Full screen" }, Action::TreeZoom).key(k(Char('z'), false)).enabled(self.cfg.preview),
                Item::new(self.next_view_label(), Action::NextView).key(k(Char('v'), false)).enabled(self.preview_views().is_some()),
                Item::new(if self.cfg.preview_wrap { "Don't wrap long lines" } else { "Wrap long lines" }, Action::TreeWrap).key(k(Char('w'), false)).enabled(self.cfg.preview),
            ],
            plugin_group(if is_dir { crate::agents::Place::Folder } else { crate::agents::Place::File }, self.builtins),
            vec![
                Item::new("Rename…", Action::TreeRename).key(k(Char('l'), true)),
                Item::new("Delete…", Action::TreeDelete).key(k(Char('x'), true)),
            ],
            vec![Item::new("Close file tree", Action::TreeClose).key(global_label(KeyActionName::Surface(4)))],
        ]);
        Overlay::Menu(Menu::new(items, at), MenuCtx::Tree(path))
    }

    /// Executes the selected item; some open an input field instead (renaming, a worktree branch) rather than acting right away.
    pub fn run_action(&mut self, action: Action, ctx: &MenuCtx) -> Overlay {
        use crate::menu::Prompt;
        self.dirty = true;
        match (action, ctx) {
            (Action::Plugins, _) => return self.plugins_overlay(),
            (Action::PluginAdd, _) => return Overlay::Prompt(Prompt::new("Add from GitHub: https://github.com/<owner>/<repo>", "").does("add"), Ask::PluginAddress),
            (Action::PluginUpdate, MenuCtx::Plugin(n)) => {
                self.plugin_fetch(crate::plugin_list::Act::Update, n.clone(), &["--plugin-update", n]);
                return self.plugins_overlay_at(n);
            }
            (Action::PluginInstall, MenuCtx::Plugin(n)) => match crate::catalog::read().0.entries.into_iter().find(|e| e.name == *n) {
                Some(e) => {
                    self.plugin_fetch(crate::plugin_list::Act::Install, n.clone(), &["--plugin-add", &e.url]);
                    return self.plugins_overlay_at(n);
                }
                None => self.notice = Some(format!("{n} is no longer in the catalog")),
            },
            (Action::PluginOff | Action::PluginOn, MenuCtx::Plugin(n)) => {
                let on = action == Action::PluginOn;
                match crate::install::turn(n, on, "off") {
                    Ok(()) => self.toast = Some(format!("{n} turned {}", if on { "on" } else { "off" })),
                    Err(e) => self.notice = Some(e),
                }
                return self.plugins_overlay();
            }
            (Action::PluginLog, MenuCtx::Plugin(n)) => {
                let log = crate::plugins::data_dir(n).join("output.log");
                // The plugin chose what sits there: only a plain file is shown, never a link's target or a pipe.
                if crate::plugins::plain_in_data(n, "output.log") {
                    self.open_tab(format!("{n} log"), vec!["/usr/bin/less".into(), "-L".into(), "+G".into(), log.to_string_lossy().into_owned()]);
                } else {
                    self.notice = Some(format!("{n} has written no log yet"));
                }
            }
            (Action::PluginRemove, MenuCtx::Plugin(n)) => {
                let data = crate::plugins::data_dir(n);
                let body = if data.exists() { format!("Its folder goes, and nerd's hooks leave any agent config it used. Its data in {} stays unless you take it too.", data.display()) } else { "Its folder goes, and nerd's hooks leave any agent config it used.".into() };
                let mut c = crate::menu::Confirm::new("Remove plugin", body).about(n.clone()).does("remove plugin").danger(true);
                if data.exists() {
                    c = c.other(" [d] remove it and its data ");
                }
                return Overlay::Confirm(c, Pending::RemovePlugin(n.clone()));
            }
            (Action::Rename, MenuCtx::Space(i)) => {
                let cur = self.spaces.get(*i).map(Space::label).unwrap_or_default();
                return Overlay::Prompt(Prompt::new("Rename space", cur).does("rename"), Ask::RenameSpace(*i));
            }
            (Action::Rename, MenuCtx::Tab(i)) => {
                let cur = self
                    .space()
                    .and_then(|sp| sp.tabs.get(*i))
                    .and_then(|t| t.name.clone())
                    .unwrap_or_else(|| format!("{}", i + 1));
                return Overlay::Prompt(Prompt::new("Rename tab", cur).does("rename"), Ask::RenameTab(*i));
            }
            (Action::RenamePane, MenuCtx::Pane(id)) => {
                let cur = self.pane(*id).and_then(|p| p.label.clone()).unwrap_or_default();
                return Overlay::Prompt(Prompt::new("Rename pane", cur).does("rename").note("A named pane can receive messages from other agents."), Ask::RenamePane(*id));
            }
            (Action::Messages, _) => return Overlay::Messages(crate::messages::Messages::new()),
            (Action::RoutesMap, _) => return Overlay::Messages(crate::messages::Messages::routes_map()),
            (Action::SendMessage, MenuCtx::Pane(id)) => {
                let Some(name) = self.address(*id) else { return Overlay::None };
                return Overlay::Prompt(Prompt::new(format!("Send message to {name}"), "").does("send"), Ask::SendTo(name));
            }
            (Action::ShowInbox, MenuCtx::Pane(id)) => return Overlay::Messages(crate::messages::Messages::inbox(*id, self.address(*id))),
            (Action::WhoCanWrite, MenuCtx::Pane(id)) => return Overlay::Messages(crate::messages::Messages::on(self.address(*id), true)),
            (Action::WhereCanWrite, MenuCtx::Pane(id)) => return Overlay::Messages(crate::messages::Messages::on(self.address(*id), false)),
            (Action::Isolation, MenuCtx::Pane(id)) => return self.isolation_overlay(IsolationTarget::Pane(*id)),
            (Action::IsolationNew, MenuCtx::Tab(i)) => return self.isolation_overlay(IsolationTarget::Tab(self.active, *i)),
            (Action::IsolationNew, MenuCtx::Space(i)) => return self.isolation_overlay(IsolationTarget::Space(*i)),
            (Action::NewWorktree, MenuCtx::Space(i)) => {
                return Overlay::Prompt(Prompt::new("New worktree branch", "").does("create"), Ask::NewWorktree(*i));
            }
            (Action::OpenWorktree, MenuCtx::Space(i)) => {
                // A filtered list of existing worktrees; git creates nothing here.
                let Some(cwd) = self.spaces.get(*i).map(|s| s.cwd.clone()) else { return Overlay::None };
                let list = crate::git::worktrees(&cwd);
                if list.is_empty() {
                    self.notice = Some("no worktrees found".to_string());
                    return Overlay::None;
                }
                let paths: Vec<PathBuf> = list.iter().map(|w| w.path.clone()).collect();
                let items = list
                    .iter()
                    .enumerate()
                    .map(|(id, w)| crate::menu::PickerItem {
                        label: w.branch.clone().unwrap_or_else(|| "(detached)".to_string()),
                        detail: w.path.display().to_string(),
                        id,
                    })
                    .collect();
                return Overlay::Picker(crate::menu::Picker::new("Open worktree", items), Pick::OpenWorktree(paths));
            }
            (Action::RemoveWorktree, MenuCtx::Space(i)) => return self.confirm_overlay(Pending::RemoveWorktree(*i)),
            (Action::NewSpace, _) => return self.new_space_overlay(),

            (Action::NewTab, MenuCtx::Space(i)) => self.new_tab_in(*i),
            (Action::NewTab, _) => self.new_tab(),
            (Action::About, _) => return self.about_overlay(),
            (Action::StopServer, _) => return self.stop_overlay(),
            (Action::MoveUp, MenuCtx::Space(i)) => {
                if let Some(to) = self.space_neighbour(*i, -1) {
                    self.move_space(*i, to);
                }
            }
            (Action::MoveDown, MenuCtx::Space(i)) => {
                if let Some(to) = self.space_neighbour(*i, 1) {
                    self.move_space(*i, to);
                }
            }
            (Action::Close, MenuCtx::Space(i)) => return self.confirm_overlay(Pending::CloseSpace(*i)),
            (Action::Close, MenuCtx::Tab(i)) => return self.confirm_overlay(Pending::CloseTab(*i)),
            (Action::ToggleGroup, MenuCtx::Space(i)) => {
                let cwd = self.spaces.get(*i).map(|s| s.cwd.clone());
                if let Some(key) = cwd.and_then(|c| self.group_key_of(&c))
                    && !self.collapsed_groups.remove(&key) {
                        self.collapsed_groups.insert(key);
                    }
            }
            (Action::CloseGroup, MenuCtx::Space(i)) => {
                let cwd = self.spaces.get(*i).map(|s| s.cwd.clone());
                if let Some(key) = cwd.and_then(|c| self.group_key_of(&c)) {
                    return self.confirm_overlay(Pending::CloseGroup(key));
                }
            }
            (Action::SplitRight, MenuCtx::Pane(id)) => {
                self.focus_pane(*id);
                self.split(Dir::Horizontal);
            }
            (Action::SplitDown, MenuCtx::Pane(id)) => {
                self.focus_pane(*id);
                self.split(Dir::Vertical);
            }
            (Action::Zoom, MenuCtx::Pane(id)) => {
                self.focus_pane(*id);
                if let Some(t) = self.tab_mut() {
                    t.layout.toggle_zoom();
                }
            }
            (Action::ClosePane, MenuCtx::Pane(id)) => {
                self.focus_pane(*id);
                return self.confirm_overlay(Pending::ClosePane);
            }
            (Action::Subagents, MenuCtx::Pane(id)) => return self.subagents_overlay(*id),
            (Action::ToggleTree, _) => {
                if self.tree.is_some() {
                    self.close_tree();
                } else {
                    self.toggle_tree();
                }
            }
            (Action::Settings, _) => return Overlay::Settings(crate::settings::Settings::new(&self.cfg)),
            (Action::Keybinds, _) => return Overlay::Keys(crate::keyshelp::KeysHelp::new(self.cfg.prefix)),
            (Action::ReloadConfig, _) => {
                crate::agents::reload();
                self.plugins.reload();
                self.reload_cfg();
            }
            (Action::Detach, _) => return self.detach_overlay(),
            (Action::Plugin(n), ctx) => {
                let Some((name, id)) = crate::plugins::menu_command(n) else { return Overlay::None };
                if !crate::plugins::commands().iter().any(|(p, c)| *p == name && c.id == id) {
                    self.notice = Some(format!("plugin {name} has no command {id} any more"));
                    return Overlay::None;
                }
                let (pane, path) = ctx.origin();
                if let Err(e) = self.plugins.command(&name, crate::plugins::Event::Command { id, pane, path }) {
                    self.notice = Some(e);
                }
            }
            (Action::Panel(n), ctx) => {
                let Some((name, id)) = crate::plugins::menu_command(n) else { return Overlay::None };
                let Some((src, (_, title, _))) = crate::plugins::panels(self.builtins).into_iter().find(|(s, p)| s.1 == name && p.0 == id) else {
                    self.notice = Some(format!("{name} has no panel {id} any more"));
                    return Overlay::None;
                };
                let (pane, path) = ctx.origin();
                // A panel of a pane or a tree row shows that one's content, never what another pane's opening left.
                if pane.is_some() || path.is_some() {
                    self.plugins.slots.forget_panel(&src, &id);
                }
                self.panel_event(&src, crate::plugins::Event::PanelOpened { id: id.clone(), pane, path });
                return Overlay::Panel(crate::panel::Open::new(src, id, title));
            }
            (Action::DisconnectAll, _) => return self.disconnect_overlay(),
            (Action::Connect, _) => return self.connect_overlay(),
            (Action::Report, _) => {
                let items = REPORT_PERIODS
                    .iter()
                    .enumerate()
                    .map(|(id, (label, _))| crate::menu::PickerItem { label: label.to_string(), detail: String::new(), id })
                    .collect();
                return Overlay::Picker(crate::menu::Picker::new("Export logs", items), Pick::Report);
            }
            (Action::Sessions, _) => {
                let own = nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned());
                return Overlay::Sessions(crate::clients::Sessions::new(self.clients.clone(), own));
            }
            (Action::RenameInstance, _) => {
                let id = crate::session::instance_name();
                if id.is_empty() {
                    self.notice = Some("the legacy server cannot be named; start a new one with nerd".into());
                    return Overlay::None;
                }
                let current = crate::session::instance_label(&id);
                let prefill = if current == id { String::new() } else { current };
                return Overlay::Prompt(Prompt::new("Rename this server", prefill).does("rename"), Ask::RenameInstance(id));
            }
            // The same dialog as on attach: it explains itself and offers the command as a fallback.
            (Action::TmuxWindow, _) => return self.tmux_offer(),
            (Action::Update, _) => return self.update_overlay(),
            (Action::Search, ctx) => {
                let id = match ctx {
                    MenuCtx::Pane(id) => Some(*id),
                    _ => self.tab().map(|t| t.layout.focused()),
                };
                if let Some(id) = id {
                    self.focus_pane(id);
                    self.search = Some((id, crate::search::Search::new()));
                }
            }
            // Returned, not left in place: the caller puts what this returns over the overlay.
            (Action::Navigator, _) => {
                self.open_navigator();
                return std::mem::replace(&mut self.overlay, Overlay::None);
            }
            (Action::ToggleSidebar, _) => self.toggle_sidebar(),
            (Action::TreeEdit, MenuCtx::Tree(p)) => self.tree_outcome(crate::tree::Outcome::OpenInEditor(p.clone(), None)),
            (Action::TreeInsert, MenuCtx::Tree(p)) => self.tree_outcome(crate::tree::Outcome::InsertPath(p.clone())),
            (Action::TreeOpen, MenuCtx::Tree(p)) => self.tree_outcome(crate::tree::Outcome::OpenSystem(p.clone())),
            (Action::TreeCopy, MenuCtx::Tree(p)) => self.tree_outcome(crate::tree::Outcome::CopyPath(p.clone())),
            (Action::TreeCd, MenuCtx::Tree(p)) => self.tree_outcome(crate::tree::Outcome::Cd(p.clone())),
            (Action::TreeSpace, MenuCtx::Tree(p)) => {
                let dir = if p.is_dir() { p.clone() } else { p.parent().map(PathBuf::from).unwrap_or_default() };
                self.tree_outcome(crate::tree::Outcome::NewSpace(dir));
            }
            (Action::TreeNew, MenuCtx::Tree(p)) => {
                let dir = if p.is_dir() { p.clone() } else { p.parent().map(PathBuf::from).unwrap_or_default() };
                return self.tree_ask(crate::tree::Outcome::AskCreate(dir));
            }
            (Action::TreeRename, MenuCtx::Tree(p)) => return self.tree_ask(crate::tree::Outcome::AskRename(p.clone())),
            (Action::TreeDelete, MenuCtx::Tree(p)) => return self.tree_ask(crate::tree::Outcome::AskDelete(p.clone())),
            (Action::TreeView, _) => {
                if let Some(t) = self.tree.as_mut() {
                    let next = match t.view() {
                        crate::tree::View::Changes => crate::tree::View::Files,
                        crate::tree::View::Files => crate::tree::View::Changes,
                    };
                    t.set_view(next);
                    self.focus = Focus::Tree;
                }
            }
            (Action::TreeShape, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.toggle_changes_shape();
                    self.focus = Focus::Tree;
                }
            }
            (Action::TreeFilter, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.start_filter("");
                }
            }
            (Action::TreeGrep, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.start_grep();
                }
            }
            (Action::TreeUp, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.root_up();
                }
            }
            (Action::TreeZoom, _) => self.tree_zoom = !self.tree_zoom && self.cfg.preview,
            (Action::TreeWrap, _) => self.toggle_wrap(),
            (Action::NextView, _) => self.next_view(),
            (Action::TreeHidden, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.toggle_hidden();
                }
            }
            (Action::TreeReload, _) => {
                if let Some(t) = self.tree.as_mut() {
                    t.reload();
                }
            }
            (Action::TreeRoot, MenuCtx::Tree(p)) => {
                if let Some(t) = self.tree.as_mut() {
                    t.goto_root(p.clone());
                }
            }
            (Action::TreeClose, _) => self.close_tree(),
            _ => {}
        }
        Overlay::None
    }

    /// Response from the input field. An empty string means "reset the name".
    pub fn run_ask(&mut self, value: String, ask: &Ask) {
        self.dirty = true;
        let value = value.trim().to_string();
        match ask {
            Ask::RenameSpace(i) => {
                if let Some(sp) = self.spaces.get_mut(*i) {
                    sp.name = (!value.is_empty()).then_some(value);
                }
            }
            Ask::RenameInstance(id) => self.rename_instance(id, &value),
            Ask::RenameTab(i) => {
                if let Some(t) = self.space_mut().and_then(|sp| sp.tabs.get_mut(*i)) {
                    t.name = (!value.is_empty()).then_some(value);
                }
            }
            Ask::RenamePane(id) => {
                let _ = self.rename_pane(*id, &value);
            }
            Ask::SendTo(_) | Ask::Broadcast(_) | Ask::Reply(..) | Ask::Admit(_) => {}
            Ask::NewWorktree(i) => {
                if value.is_empty() {
                    return;
                }
                if let Some(cwd) = self.spaces.get(*i).map(|s| s.cwd.clone()) {
                    match crate::git::add_worktree(&cwd, &value) {
                        Ok(path) => self.open_space(path),
                        // We show git's own message: it's more precise than any
                        // paraphrase of ours.
                        Err(e) => self.notice = Some(e),
                    }
                }
            }
            Ask::PluginAddress => {
                // Before a ref is split off, or `https://token@host` would reach a command line.
                let url = if crate::install::secret_in(&value) { value.as_str() } else { crate::install::split_ref(&value).0 };
                match crate::install::source(url, false) {
                    Ok(_) => self.plugin_fetch(crate::plugin_list::Act::Add, crate::plugin_list::short(url), &["--plugin-add", &value]),
                    Err(e) => self.notice = Some(e),
                }
            }
            Ask::CreateIn(dir) => {
                if value.is_empty() {
                    return;
                }
                match crate::tree::create(dir, &value) {
                    Ok(p) => self.tree_after_change(&p),
                    Err(e) => self.notice = Some(e),
                }
            }
            Ask::RenamePath(path) => {
                if value.is_empty() {
                    return;
                }
                match crate::tree::rename(path, &value) {
                    Ok(p) => self.tree_after_change(&p),
                    Err(e) => self.notice = Some(e),
                }
            }
        }
    }

    /// The pane's name when it can receive messages.
    pub(super) fn address(&self, id: PaneId) -> Option<String> {
        self.pane(id)?.label.clone().filter(|l| crate::policy::valid_address(l))
    }

    /// Every change of agent messaging goes here: refused from a window inside a pane, said as a toast or a notice.
    pub(super) fn mail_done(&mut self, a: MailAction) -> bool {
        self.dirty = true;
        match self.mail_act(a, self.client_inside) {
            Ok(t) => {
                if !t.is_empty() {
                    self.toast = Some(t);
                }
                true
            }
            Err(e) => {
                self.notice = Some(e);
                false
            }
        }
    }

    pub(super) fn messages_outcome(&mut self, w: crate::messages::Messages, out: crate::messages::Out) -> Overlay {
        use crate::messages::Out;
        match out {
            Out::None => Overlay::Messages(w),
            Out::Close => Overlay::None,
            Out::Act(a) => {
                self.mail_done(a);
                Overlay::Messages(w)
            }
            Out::Reply(id) => {
                let to = self.mail.get(&id).map(|m| m.from.name().to_string()).unwrap_or_default();
                Overlay::Prompt(crate::menu::Prompt::new(format!("Reply to {to}"), "").does("send"), Ask::Reply(id, Box::new(w)))
            }
            Out::Broadcast => Overlay::Prompt(crate::menu::Prompt::new("Broadcast to every named pane", "").does("send"), Ask::Broadcast(Box::new(w))),
            Out::Topology => self.topology_overlay(Some(Box::new(w))),
        }
    }

    /// Isolation… on a pane, or Isolation for new panes… on a tab or a space, as they are now.
    pub(super) fn isolation_overlay(&self, target: IsolationTarget) -> Overlay {
        let agents = crate::agents::all();
        let providers: Vec<String> = agents.iter().filter(|m| matches!(m.agent.network.net, crate::agents::Network::Internet { .. })).map(|m| m.name.clone()).collect();
        let notes = agents.iter().filter_map(|m| Some(format!("{}: {}", m.title, m.agent.own_sandbox.as_ref()?))).collect();
        let accounts = crate::policy::current().accounts.iter().map(|a| a.name.clone()).collect();
        let (about, mut pick, inherit, agent, folder) = match target {
            IsolationTarget::Pane(id) => {
                let Some(p) = self.pane(id) else { return Overlay::None };
                (p.label(), Some(p.isolation.clone()), None, p.agent.clone().or_else(|| p.agent_session.as_ref().map(|s| s.0.clone())), p.cwd.clone())
            }
            IsolationTarget::Tab(si, ti) => {
                let Some((sp, t)) = self.spaces.get(si).and_then(|sp| Some((sp, sp.tabs.get(ti)?))) else { return Overlay::None };
                (format!("new panes in tab {}/{}", sp.label(), t.label(ti)), t.isolation_new.clone(), Some("as the space"), None, sp.cwd.clone())
            }
            IsolationTarget::Space(si) => {
                let Some(sp) = self.spaces.get(si) else { return Overlay::None };
                (format!("new panes in space {}", sp.label()), sp.isolation_new.clone(), Some("as nerd's default"), None, sp.cwd.clone())
            }
        };
        let canon = |p: &std::path::Path| std::fs::canonicalize(p).ok();
        let home = crate::paths::home().is_some_and(|h| canon(&h).is_some() && canon(&h) == canon(&folder));
        let groups: Vec<_> = agents.iter().filter(|m| !m.agent.network.providers.is_empty()).map(|m| (m.name.clone(), m.agent.network.clone())).collect();
        let (chosen, stale) = match pick.as_ref().map(|i| &i.network) {
            Some(crate::policy::Network::Providers(p)) => crate::isolation_ui::known(p, &providers, &groups),
            _ => (agent.filter(|a| providers.contains(a)).or_else(|| providers.first().cloned()).into_iter().map(|a| (a, None)).collect(), Vec::new()),
        };
        // Save keeps what the window shows: never a name or a group that is gone, never wider than saved.
        if let Some(crate::policy::Isolation { network: crate::policy::Network::Providers(p), .. }) = pick.as_mut() {
            *p = crate::policy::entries(&chosen);
        }
        Overlay::Isolation(crate::isolation_ui::IsolationWin { target, about, pick, inherit, accounts, providers, groups, chosen, stale, view: None, notes, home })
    }

    pub(super) fn isolation_outcome(&mut self, w: crate::isolation_ui::IsolationWin, out: crate::isolation_ui::Out) -> Overlay {
        use crate::isolation_ui::Out;
        match out {
            Out::Close => Overlay::None,
            Out::Save(isolation, restart) if self.mail_done(MailAction::SetIsolation { target: w.target, isolation: isolation.clone(), restart }) => Overlay::None,
            _ => Overlay::Isolation(w),
        }
    }

    pub(super) fn accounts_outcome(&mut self, w: crate::isolation_ui::Accounts, out: crate::isolation_ui::Out) -> Overlay {
        use crate::isolation_ui::Out;
        match out {
            Out::Close => Overlay::None,
            Out::Remove(name) => {
                self.mail_done(MailAction::RemoveAccount(name));
                Overlay::Accounts(w)
            }
            Out::Admit => Overlay::Prompt(crate::menu::Prompt::new("Admit an account", "").does("admit"), Ask::Admit(w)),
            Out::Copy(text) => {
                self.to_client.extend(crate::select::copy(&text));
                self.toast = Some("copied: the commands".into());
                Overlay::Accounts(w)
            }
            _ => Overlay::Accounts(w),
        }
    }

    /// Rename pane…'s answer, checked against the names taken and their history; None keeps the prompt open.
    pub(super) fn rename_pane(&mut self, id: PaneId, value: &str) -> Option<Overlay> {
        let rename = |keep| MailAction::Rename { pane: id, name: (!value.is_empty()).then(|| value.to_string()), keep };
        match self.rename_check(id, value) {
            RenameCheck::Taken { space, tab } => {
                self.notice = Some(format!("A pane in space {space}, tab {tab} is already called {value}"));
                return None;
            }
            RenameCheck::Label => {
                if self.mail_done(rename(true)) && !value.is_empty() {
                    self.toast = Some(format!("{value} is a label only: it receives no messages"));
                }
            }
            RenameCheck::Address => {
                self.mail_done(rename(true));
            }
            RenameCheck::History { routes, waiting } => {
                let count = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
                let list = if routes.is_empty() { String::new() } else { format!(": {}", routes.join(", ")) };
                let body = format!("This name has {} and {} from before{list}.", count(routes.len(), "route"), count(waiting, "waiting message"));
                let ask = crate::menu::Confirm::new("Rename pane", body).about(value).labels(" [k] keep them ", " [Esc] cancel ").other(" [d] drop them ");
                return Some(Overlay::Confirm(ask, Pending::Rename(id, value.to_string())));
            }
        }
        Some(Overlay::None)
    }

    /// A prompt answered: the window that comes next, or None to keep the prompt open.
    pub(super) fn submitted(&mut self, value: String, ask: &Ask) -> Option<Overlay> {
        let text = value.trim().to_string();
        let send = |to: String| MailAction::Send { to, text: text.clone() };
        Some(match ask {
            Ask::PluginAddress => {
                self.run_ask(value, ask);
                self.plugins_overlay()
            }
            Ask::RenamePane(id) => return self.rename_pane(*id, &text),
            Ask::SendTo(name) => {
                if !text.is_empty() {
                    self.mail_done(send(name.clone()));
                }
                Overlay::None
            }
            Ask::Broadcast(w) => {
                if !text.is_empty() {
                    self.mail_done(send("all".into()));
                }
                Overlay::Messages((**w).clone())
            }
            Ask::Reply(id, w) => {
                if !text.is_empty() {
                    self.mail_done(MailAction::Reply { id: id.clone(), text: text.clone() });
                }
                Overlay::Messages((**w).clone())
            }
            Ask::Admit(w) if text.is_empty() => Overlay::Accounts(w.clone()),
            // Refused, most often for an account that does not exist yet: the commands that make it.
            Ask::Admit(w) => Overlay::Accounts(if self.mail_done(MailAction::AdmitAccount(text.clone())) { w.clone() } else { crate::isolation_ui::Accounts::for_name(&text) }),
            _ => {
                self.run_ask(value, ask);
                Overlay::None
            }
        })
    }

    /// A prompt left without an answer: back to the window it came from.
    pub(super) fn prompt_cancelled(&mut self, ask: Ask) -> Overlay {
        match ask {
            Ask::PluginAddress => self.plugins_overlay(),
            Ask::Broadcast(w) | Ask::Reply(_, w) => Overlay::Messages(*w),
            Ask::Admit(w) => Overlay::Accounts(w),
            _ => Overlay::None,
        }
    }

    /// Set up a topology…: a preset first. `back`: the Messages window it was asked from, which every way out returns to.
    pub(super) fn topology_overlay(&mut self, back: Option<Box<crate::messages::Messages>>) -> Overlay {
        let items = TOPOLOGIES.iter().enumerate().map(|(id, l)| crate::menu::PickerItem { label: l.to_string(), detail: String::new(), id }).collect();
        Overlay::Picker(crate::menu::Picker::new("Set up a topology", items), Pick::Topology(None, Vec::new(), Vec::new(), back))
    }

    /// The next name the preset needs, or the routes it makes to confirm.
    fn topology_next(&mut self, preset: usize, picked: Vec<String>, back: Option<Box<crate::messages::Messages>>) -> Overlay {
        use crate::policy::{Action as A, Route, Scope};
        let named = self.named_panes();
        let mut spaces: Vec<String> = named.iter().map(|n| n.space.clone()).collect();
        spaces.dedup();
        let all: Vec<String> = self.spaces.iter().map(Space::label).collect();
        let pane = |n: &str| Scope::Pane(n.into());
        let space = |s: &str| Scope::Space(s.into());
        let route = |from, to, action| Route { from, to, action, until: None };
        let names = |space: Option<&str>| named.iter().filter(|n| space.is_none_or(|s| n.space == s)).map(|n| n.name.clone()).collect::<Vec<_>>();
        let mut ask = |title: String, options: Vec<String>| {
            if options.is_empty() {
                self.notice = Some("no pane is named yet: name panes with Rename pane… first".into());
                return back.clone().map_or(Overlay::None, |w| Overlay::Messages(*w));
            }
            let items = options.iter().enumerate().map(|(id, l)| crate::menu::PickerItem { label: l.clone(), detail: String::new(), id }).collect();
            Overlay::Picker(crate::menu::Picker::new(title, items), Pick::Topology(Some(preset), picked.clone(), options, back.clone()))
        };
        let routes = match (preset, picked.len()) {
            (0, _) => vec![route(Scope::All, Scope::All, A::Allow)],
            (1, 0) => return ask("Pick the hub".into(), names(None)),
            (1, _) => vec![route(pane(&picked[0]), Scope::All, A::Allow), route(Scope::All, pane(&picked[0]), A::Allow), route(Scope::All, Scope::All, A::No)],
            (2, 0) => return ask("Pick the lead".into(), names(None)),
            (2, k) if k <= spaces.len() => return ask(format!("Pick the hub of space {}", spaces[k - 1]), names(Some(&spaces[k - 1]))),
            (2, _) => {
                let lead = &picked[0];
                let mut r = Vec::new();
                for (s, hub) in spaces.iter().zip(&picked[1..]) {
                    if hub != lead {
                        r.extend([route(pane(lead), pane(hub), A::Allow), route(pane(hub), pane(lead), A::Allow)]);
                    }
                    r.extend([route(pane(hub), space(s), A::Allow), route(space(s), pane(hub), A::Allow)]);
                }
                r.push(route(Scope::All, Scope::All, A::No));
                r
            }
            (_, 0) => return ask("Pick the reading space".into(), all),
            (_, 1) => return ask("Pick the private space".into(), all),
            (_, _) => {
                let (reading, private) = (space(&picked[0]), space(&picked[1]));
                vec![route(reading.clone(), private.clone(), A::Ask), route(private.clone(), reading, A::No), route(private, Scope::All, A::Ask)]
            }
        };
        let rows = routes.iter().map(|r| vec![r.said()]).collect();
        let confirm = crate::menu::Confirm::new("Set up a topology", "These routes replace any with the same ends; the others stay.").rows(rows).does("set the routes");
        Overlay::Confirm(confirm, Pending::Topology(routes, back))
    }

    /// The new space dialog's Enter/double-click: opens the folder, or says why not.
    pub(super) fn open_new_space(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.open_space(path);
        } else {
            self.notice = Some(format!("no such folder: {}", path.display()));
        }
    }

    /// New space dialog: the "recent" list is derived once, at the moment it opens.
    pub(super) fn new_space_overlay(&mut self) -> Overlay {
        let base = self.space().map(|s| s.cwd.clone()).unwrap_or_default();
        Overlay::NewSpace(crate::space_picker::SpacePicker::new(base))
    }
}

/// What a menu item or a global key asks for. Only `App` knows their meaning; the widget just returns them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    Rename,
    Close,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    NewSpace,
    ToggleGroup,
    CloseGroup,

    RenamePane,
    SplitRight,
    SplitDown,
    Zoom,
    ClosePane,

    Settings,
    Keybinds,
    ReloadConfig,
    Detach,
    ToggleTree,
    Update,
    Search,
    Navigator,
    ToggleSidebar,
    Sessions,
    Plugins,
    PluginAdd,
    PluginInstall,
    PluginUpdate,
    PluginOff,
    PluginOn,
    PluginLog,
    PluginRemove,

    TreeEdit,
    TreeInsert,
    TreeOpen,
    TreeCopy,
    TreeCd,
    TreeNew,
    TreeRename,
    TreeDelete,
    TreeFilter,
    TreeGrep,
    TreeUp,
    TreeZoom,
    TreeClose,
    TreeSpace,
    TreeView,
    TreeShape,
    TreeWrap,
    NextView,
    TreeHidden,
    TreeReload,
    TreeRoot,

    /// From the menu: the same offer the attach makes by itself.
    TmuxWindow,
    RenameInstance,
    MoveUp,
    MoveDown,
    Connect,
    Report,
    DisconnectAll,
    About,
    StopServer,
    Subagents,
    /// A plugin's menu command, by `plugins::menu_number`.
    Plugin(u16),
    /// A plugin's or a public built-in's panel, by `plugins::menu_number`.
    Panel(u16),
    Messages,
    RoutesMap,
    SendMessage,
    ShowInbox,
    WhoCanWrite,
    WhereCanWrite,
    Isolation,
    /// Of a tab or a space.
    IsolationNew,
}

/// Plugins' commands for this menu, then the panels of public built-ins and plugins; empty when there are none.
fn plugin_group(place: crate::agents::Place, builtins: bool) -> Vec<crate::menu::Item<Action>> {
    let mut mine: Vec<crate::menu::Item<Action>> = crate::plugins::commands()
        .into_iter()
        .filter(|(_, c)| c.place.fits(place))
        .filter_map(|(p, c)| Some(crate::menu::Item::new(c.title, Action::Plugin(crate::plugins::menu_number(&p, &c.id)?))))
        .collect();
    mine.extend(
        crate::plugins::panels(builtins)
            .into_iter()
            .filter(|(_, (_, _, at))| at.fits(place))
            .filter_map(|(src, (id, title, _))| Some(crate::menu::Item::new(format!("{title}…"), Action::Panel(crate::plugins::menu_number(&src.1, &id)?)))),
    );
    mine
}

/// The groups in order with one separator between neighbours; an empty group leaves no line.
fn grouped(groups: Vec<Vec<crate::menu::Item<Action>>>) -> Vec<crate::menu::Item<Action>> {
    let mut out = Vec::new();
    for g in groups.into_iter().filter(|g| !g.is_empty()) {
        if !out.is_empty() {
            out.push(crate::menu::Item::separator(Action::Plugin(u16::MAX)));
        }
        out.extend(g);
    }
    out
}

impl From<KeyAction> for Action {
    fn from(a: KeyAction) -> Self {
        match a {
            KeyAction::NewTab => Action::NewTab,
            KeyAction::ClosePane => Action::ClosePane,
            KeyAction::SplitDown => Action::SplitDown,
            KeyAction::SplitSide => Action::SplitRight,
            KeyAction::Zoom => Action::Zoom,
            KeyAction::Sidebar => Action::ToggleSidebar,
            KeyAction::Update => Action::Update,
            KeyAction::Detach => Action::Detach,
        }
    }
}

/// What the open menu applies to.
#[derive(Clone, Debug)]
pub enum MenuCtx {
    Space(usize),
    Tab(usize),
    Pane(PaneId),
    Global,
    /// A row of the file tree, by path.
    Tree(PathBuf),
    /// A plugin in the Plugins window, by name.
    Plugin(String),
}

impl MenuCtx {
    /// What a plugin's command or panel is told it was opened from: a pane, or a tree row's path.
    fn origin(&self) -> (Option<PaneId>, Option<PathBuf>) {
        match self {
            MenuCtx::Pane(p) => (Some(*p), None),
            MenuCtx::Tree(p) => (None, Some(p.clone())),
            _ => (None, None),
        }
    }
}

/// What exactly we're asking for in the input field.
/// The plugin showing the preview, the (id, label) of each view it offers, and the one shown.
pub(super) type Views = (String, Vec<(String, String)>, String);

#[derive(Clone, Debug)]
pub enum Ask {
    RenameSpace(usize),
    RenameTab(usize),
    RenamePane(PaneId),
    NewWorktree(usize),
    /// Name of the new file (trailing `/` means a directory) in this folder.
    CreateIn(PathBuf),
    RenamePath(PathBuf),
    /// Plugins > Add from GitHub…
    PluginAddress,
    /// This server's instance, by id: a name label beside it, the directory stays.
    RenameInstance(String),
    /// A message from you to this name.
    SendTo(String),
    /// From the Messages window, which comes back after.
    Broadcast(Box<crate::messages::Messages>),
    Reply(String, Box<crate::messages::Messages>),
    /// From the Accounts window, which comes back after.
    Admit(crate::isolation_ui::Accounts),
}

/// No more than one lives on top of the frame at a time.
pub enum Overlay {
    None,
    Menu(crate::menu::Menu<Action>, MenuCtx),
    Prompt(crate::menu::Prompt, Ask),
    Navigator(crate::navigator::Navigator),
    Confirm(crate::menu::Confirm, Pending),
    Picker(crate::menu::Picker, Pick),
    /// New space: a path field with live completion and a recent-folders list.
    NewSpace(crate::space_picker::SpacePicker),
    /// Settings screen: edits apply immediately and are written to config.
    Settings(crate::settings::Settings),
    /// Keys screen, read-only.
    Keys(crate::keyshelp::KeysHelp),
    /// Every client that attached since the server started, read-only.
    Sessions(crate::clients::Sessions),
    Panel(crate::panel::Open),
    Plugins(crate::plugin_list::PluginList),
    Messages(crate::messages::Messages),
    Isolation(crate::isolation_ui::IsolationWin),
    Accounts(crate::isolation_ui::Accounts),
}

/// What a selection in the list means.
#[derive(Clone, Debug)]
pub enum Pick {
    /// Open a worktree: index in the list -> path.
    OpenWorktree(Vec<PathBuf>),
    /// A machine from config.json, by index.
    Remote(Vec<crate::config::RemoteEntry>),
    /// An instance on that machine; `window`: the client can open a terminal for it.
    RemoteInstance { name: String, list: Vec<crate::proto::RemoteInstance>, window: bool },
    /// A subagent of this pane's agent, by id.
    Run(PaneId, Vec<String>),
    /// A period from REPORT_PERIODS, by index.
    Report,
    /// Set up a topology…: the preset once chosen, the names picked so far, and what this picker offers.
    Topology(Option<usize>, Vec<String>, Vec<String>, Option<Box<crate::messages::Messages>>),
}

/// Set up a topology…'s presets, as the picker offers them.
const TOPOLOGIES: [&str; 4] = ["Mesh", "Hub", "Hierarchy", "Reading and private"];

/// Export logs…: what the picker offers, and its length in hours.
pub(super) const REPORT_PERIODS: [(&str, u64); 3] = [("Last hour", 1), ("Last day", 24), ("Last week", 168)];

#[derive(Clone, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Pending {
    ClosePane,
    CloseTab(usize),
    CloseSpace(usize),
    CloseGroup(PathBuf),
    /// Remove a linked worktree from disk (the branch stays).
    RemoveWorktree(usize),
    /// Swap the daemon for the installed build in place.
    Update,
    /// Open a window outside the multiplexer; the third button never asks again.
    LeaveTmux,
    /// Leave the session running and let go of it.
    Detach,
    /// End every connection, this one included; the server and panes stay.
    DisconnectAll,
    /// The same offer with no terminal to open: the button copies the command.
    CopyResumeCommand,
    DeletePath(PathBuf),
    /// Ask the client to open a terminal running `nerd --remote <name>`.
    OpenRemote(String),
    /// A command for the human to run; the button copies it.
    CopyCommand(String),
    /// A dialog that only informs.
    Dismiss,
    /// Reveal a saved report in Finder.
    ShowInFinder(PathBuf),
    /// Hook into agents not hooked before; what the dialog said.
    InstallHook(String),
    /// Take nerd's entries out of every hooked agent.
    RemoveHook,
    /// Remove a plugin, by name; the third button takes its data too.
    RemovePlugin(String),
    /// A plugin a pane staged, by its staging id and the commit the question showed: placed on yes, its files
    /// deleted on no.
    PluginReady(String, String),
    /// End the server the way `nerd --stop` does.
    StopServer,
    /// A pane takes a name with history: keep it, or the third button drops it.
    Rename(PaneId, String),
    Topology(Vec<crate::policy::Route>, Option<Box<crate::messages::Messages>>),
}

impl Overlay {
    pub fn is_open(&self) -> bool {
        !matches!(self, Overlay::None)
    }
}

/// "1 pane", "2 panes": simple English pluralization for the confirm prompt.
pub(super) fn plural_spaces(n: usize) -> String {
    format!("{n} {}", if n == 1 { "space" } else { "spaces" })
}

/// When this process started; an update's exec keeps the process, so it counts from the first start.
fn server_started() -> Option<std::time::SystemTime> {
    use nix::libc;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(std::process::id() as i32, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then(|| std::time::UNIX_EPOCH + std::time::Duration::new(info.pbi_start_tvsec, info.pbi_start_tvusec as u32 * 1000))
}

/// "5 min", "3 h 12 min", "2 d 4 h": the two largest units.
fn uptime(d: std::time::Duration) -> String {
    let m = d.as_secs() / 60;
    match (m / 1440, m / 60 % 24, m % 60) {
        (0, 0, 0) => "less than a minute".into(),
        (0, 0, min) => format!("{min} min"),
        (0, h, min) => format!("{h} h {min} min"),
        (days, h, _) => format!("{days} d {h} h"),
    }
}

fn plural_panes(n: usize) -> String {
    let word = if n == 1 { "pane" } else { "panes" };
    format!("{n} {word}")
}

#[cfg(test)]
mod plural_tests {
    #[test]
    fn uptime_names_the_two_largest_units() {
        let t = |s| super::uptime(std::time::Duration::from_secs(s));
        assert_eq!([t(30), t(5 * 60), t(3 * 3600 + 12 * 60), t(2 * 86400 + 4 * 3600 + 59)], ["less than a minute", "5 min", "3 h 12 min", "2 d 4 h"]);
        assert!(super::server_started().is_some_and(|t| t.elapsed().is_ok()), "this process has a start time");
    }

    #[test]
    fn panes_pluralize() {
        assert_eq!(super::plural_panes(1), "1 pane");
        assert_eq!(super::plural_panes(3), "3 panes");
        assert_eq!(super::plural_panes(11), "11 panes");
        assert_eq!(super::plural_panes(22), "22 panes");
        assert_eq!((super::plural_spaces(1), super::plural_spaces(0), super::plural_spaces(2)), ("1 space".into(), "0 spaces".into(), "2 spaces".into()));
    }
}

const CONTROL_NAME: &str = "that name has control characters";

/// One word typed into a shell. None for a control character, which a line editor acts on even
/// inside quotes; bare when safe, so an alias still expands; `\` escaped too, for fish.
pub(super) fn shell_word(s: &str) -> Option<String> {
    if s.chars().any(char::is_control) {
        return None;
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_./=:@%+,-".contains(c)) {
        return Some(s.to_string());
    }
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str(r"'\''"),
            '\\' => out.push_str(r"'\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    Some(out)
}

#[cfg(test)]
mod quote_tests {
    use super::shell_word;

    #[test]
    fn a_word_is_bare_when_safe_quoted_otherwise_and_refused_with_a_control_character() {
        assert_eq!(shell_word("--resume=0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c").as_deref(), Some("--resume=0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c"));
        assert_eq!(shell_word("/a b/it's.txt").as_deref(), Some(r"'/a b/it'\''s.txt'"));
        assert_eq!(shell_word(r"a\b'c").as_deref(), Some(r"'a'\\'b'\''c'"));
        assert_eq!(shell_word("a; b").as_deref(), Some("'a; b'"));
        assert_eq!(shell_word("").as_deref(), Some("''"));
        assert_eq!(shell_word("a\x15b"), None);
        assert_eq!(shell_word("a\nb"), None);
    }

    /// Typed into a real line editor, as the tree types a path into a pane.
    #[test]
    fn a_hostile_name_typed_into_bash_runs_nothing() {
        use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
        use std::io::{Read, Write};
        let bash = std::path::Path::new("/bin/bash");
        if !bash.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("nerd-quote-pty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (hit, done) = (dir.join("hit"), dir.join("done"));
        let h = hit.display();
        let names = [format!("x'; touch {h}; '"), format!(r"x\'; touch {h} #"), format!("$(touch {h})"), format!("`touch {h}`"), format!("x\x15touch {h}\r")];
        let pair = NativePtySystem::default().openpty(PtySize { rows: 24, cols: 200, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(bash);
        cmd.args(["--norc", "--noprofile", "-i"]);
        cmd.cwd(&dir);
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while reader.read(&mut b).is_ok_and(|n| n > 0) {}
        });
        let mut w = pair.master.take_writer().unwrap();
        for n in &names {
            if let Some(word) = shell_word(n) {
                writeln!(w, ": {word}").unwrap();
            }
        }
        writeln!(w, "touch {}", done.display()).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done.exists() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.kill();
        assert!(!hit.exists(), "a name typed as one word ran a command");
        assert!(done.exists(), "bash ran the lines typed into it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
