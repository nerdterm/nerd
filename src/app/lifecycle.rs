use super::*;

/// A pane's text on disk is at most this; history is dropped from the top.
/// ceiling: default cutoff, not tuned per case (200x50 x 10000 lines ~= 1.29 MB).
const SCREEN_TEXT_LIMIT: usize = 1 << 20;

/// How long an exit notice waits for the pane's last output.
const EXIT_WAIT: Duration = Duration::from_millis(100);

/// Chunks a pane's output takes per round, so a flooding pane cannot hold back a quiet one.
pub(super) const QUANTUM: usize = 16;

/// How long one turn may parse output before the loop moves on.
pub const PUMP_BUDGET: Duration = Duration::from_millis(4);

impl App {
    pub(super) fn alloc_id(&mut self) -> PaneId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Spawns a pane with a process. The size is settled on the first frame.
    pub fn spawn_pane(&mut self, cwd: PathBuf, argv: Vec<String>, iso: crate::policy::Isolation) -> Option<Pane> {
        let id = self.alloc_id();
        crate::hook::clear_state(id);
        let pty = match Pty::spawn(id, &argv, &cwd, 80, 24, self.bell.clone(), &iso) {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some(format!("failed to launch {}: {e}", argv.join(" ")));
                return None;
            }
        };
        Some(Pane { isolation: iso, ..Pane::new(id, Term::new(80, 24, self.cfg.scrollback), pty, cwd, argv) })
    }

    /// Restores the previous layout; false if there's no snapshot or it's empty.
    /// Resumes a session only if nothing else holds it, or two copies would interleave writes into one Claude Code transcript.
    pub fn restore(&mut self) -> bool {
        // Screens replay once then are removed; with the setting off, remove them anyway,
        // so flipping it doesn't leave stale dumps behind.
        let replay = self.cfg.restore_screens;
        if !replay {
            crate::session::clear_screens();
        }
        let Some(snap) = crate::session::load() else {
            crate::session::clear_screens();
            return false;
        };
        if snap.spaces.is_empty() {
            crate::session::clear_screens();
            return false;
        }
        let (mut restored, mut failed) = (0usize, Vec::new());
        self.rebuild_tabs(&snap, |app, ps| {
            let launch = restore_launch(ps);
            // Whatever the pane was running, its own process is the shell again.
            let argv = shell_argv();
            // Stored size, so replay and the pty agree until the first resize; 0 would make a 0-row grid.
            // Clamped to 1000: a hand-edited snapshot must not allocate a giant grid.
            let (cols, rows) = if ps.cols == 0 || ps.rows == 0 { (80, 24) } else { (ps.cols.min(1000), ps.rows.min(1000)) };
            let mut pty = Pty::spawn(ps.id, &argv, &ps.cwd, cols, rows, app.bell.clone(), &ps.isolation)
                .map_err(|e| {
                    crate::log::error(&format!("pane {}: not restored: {e}", ps.id));
                    failed.push(format!("{}: {e}", ps.label.clone().unwrap_or_else(|| format!("pane {}", ps.id))));
                })
                .ok()?;
            if let Some(cmd) = &launch {
                // Typed in as the human would type it, so leaving the agent lands on the prompt.
                match restore_line(cmd) {
                    Some(line) => pty.write(line.as_bytes()),
                    None => app.notice = Some(format!("pane {}: its command has control characters and was not typed", ps.id)),
                }
            }
            let command = argv_name(launch.as_deref().unwrap_or(&argv));
            let mut term = Term::new(cols, rows, app.cfg.scrollback);
            // Fed before the event loop pumps pty output, so old text sits above the fresh prompt.
            if replay && let Ok(text) = std::fs::read(crate::session::screen_path(ps.id)) {
                term.replay(&text);
                restored += 1;
            }
            let _ = std::fs::remove_file(crate::session::screen_path(ps.id));
            Some(Pane { command, label: ps.label.clone(), agent_session: ps.session(), isolation: ps.isolation.clone(), ..Pane::new(ps.id, term, pty, ps.cwd.clone(), argv) })
        });
        if !failed.is_empty() {
            self.notice = Some(format!("{} not restored: {}", if failed.len() == 1 { "1 pane".into() } else { format!("{} panes", failed.len()) }, failed.join("; ")));
        }
        let stale = crate::session::clear_screens();
        if restored > 0 || stale > 0 {
            crate::log::info(&format!("screens: {restored} restored, {stale} removed"));
        }
        !self.spaces.is_empty()
    }

    /// Descriptors marked to survive exec, screens dumped for the handover.
    pub fn handover_panes(&mut self) -> std::io::Result<Vec<crate::session::PaneHandover>> {
        let dir = crate::session::handover_dump_path(0);
        let dir = dir.parent().expect("dump path has a parent");
        std::fs::create_dir_all(dir)?;
        let mut out = Vec::new();
        let trace = self.trace;
        let theme = self.theme;
        for p in self.spaces.iter_mut().flat_map(|s| s.tabs.iter_mut()).flat_map(|t| t.panes.values_mut()) {
            // Only what is queued now: a pane that keeps printing must not hold the Update.
            p.pump(crate::pty::QUEUE, trace, &theme);
            let fd = p.pty.export_fd()?;
            let (since, unseen) = p.term.activity();
            crate::paths::write_private(&crate::session::handover_dump_path(p.id), "tmp", &p.term.dump())?;
            out.push(crate::session::PaneHandover {
                id: p.id,
                fd,
                pid: p.pty.child_pid,
                cols: p.pty.cols,
                rows: p.pty.rows,
                scroll: p.term.scroll,
                alive: true,
                changed_ms_ago: since.map(|d| d.as_millis() as u64),
                unseen,
                wrapped: p.pty.wrapped,
            });
        }
        Ok(out)
    }

    /// The counterpart of `restore` after a handover: same processes, same descriptors, screens replayed from the dumps.
    pub fn adopt(&mut self, h: &crate::session::Handover) -> bool {
        self.next_id = self.next_id.max(h.next_id);
        self.clients = crate::clients::carry(h.clients.clone(), h.client.map(|c| c.0), crate::clients::now());
        self.rebuild_tabs(&h.snapshot, |app, ps| {
            let ph = h.panes.iter().find(|p| p.id == ps.id)?;
            let mut pty = Pty::adopt(ps.id, ph.fd, ph.pid, ph.cols, ph.rows, app.bell.clone()).ok()?;
            pty.wrapped = ph.wrapped;
            // Its exit notice may have gone to the build that handed it over.
            if ph.pid.is_some_and(|pid| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) == Err(nix::errno::Errno::ESRCH))
                && !app.exits.iter().any(|(e, _)| *e == ps.id)
            {
                app.exits.push((ps.id, Instant::now()));
            }
            let mut term = Term::new(ph.cols, ph.rows, app.cfg.scrollback);
            if let Ok(dump) = std::fs::read(crate::session::handover_dump_path(ps.id)) {
                term.restore(&dump, ph.scroll);
            }
            term.carry(ph.changed_ms_ago.map(Duration::from_millis), ph.unseen);
            Some(Pane { label: ps.label.clone(), agent_session: ps.session(), isolation: ps.isolation.clone(), ..Pane::new(ps.id, term, pty, ps.cwd.clone(), ps.argv.clone()) })
        });
        !self.spaces.is_empty()
    }

    /// Spaces and tabs from a snapshot, with panes from `make`; a pane it cannot make is left out.
    pub(super) fn rebuild_tabs(&mut self, snap: &WorkspaceSnapshot, mut make: impl FnMut(&mut Self, &PaneSnapshot) -> Option<Pane>) {
        for ss in &snap.spaces {
            let mut tabs = Vec::new();
            for ts in &ss.tabs {
                let mut panes = HashMap::new();
                for ps in &ts.panes {
                    let Some(pane) = make(self, ps) else { continue };
                    self.next_id = self.next_id.max(ps.id + 1);
                    panes.insert(ps.id, pane);
                }
                if panes.is_empty() {
                    continue;
                }
                let mut layout = crate::layout::Layout::from_snapshot(&ts.layout)
                    .unwrap_or_else(|| Layout::new(*panes.keys().next().unwrap()));
                // Panes that failed to start are dropped from the layout: a hole there
                // would let focus land on a dead id and swallow input.
                for id in layout.pane_ids() {
                    if !panes.contains_key(&id) {
                        layout.close(id);
                    }
                }
                if panes.contains_key(&ts.focused) {
                    layout.focus(ts.focused);
                }
                tabs.push(Tab { name: ts.name.clone(), layout, panes, isolation_new: ts.isolation_new.clone() });
            }
            if tabs.is_empty() {
                continue;
            }
            let active_tab = ss.active_tab.min(tabs.len() - 1);
            self.learn_repo(&ss.cwd);
            self.spaces.push(Space { name: ss.name.clone(), cwd: ss.cwd.clone(), tabs, active_tab, isolation_new: ss.isolation_new.clone() });
        }
        self.active = snap.active_space.min(self.spaces.len().saturating_sub(1));
    }

    /// Every pane's text to disk, for the next start to replay; called only on the way out.
    pub fn dump_screens(&mut self) {
        if !self.cfg.restore_screens {
            return;
        }
        if let Err(e) = crate::session::ensure_screens_dir() {
            crate::log::error(&format!("screens: directory failed: {e}"));
            return;
        }
        let trace = self.trace;
        let theme = self.theme;
        for p in self.spaces.iter_mut().flat_map(|s| s.tabs.iter_mut()).flat_map(|t| t.panes.values_mut()) {
            p.pump(crate::pty::QUEUE, trace, &theme);
            let text = p.term.text(SCREEN_TEXT_LIMIT);
            if let Err(e) = crate::paths::write_private(&crate::session::screen_path(p.id), "tmp", &text) {
                crate::log::error(&format!("screens: write failed for pane {}: {e}", p.id));
            }
        }
    }

    /// Round-robin by quantum until empty or `budget` is spent: (something changed, output still queued).
    pub fn pump_pty(&mut self, budget: Duration) -> (bool, bool) {
        let start = Instant::now();
        let trace = self.trace;
        let theme = self.theme;
        if let Some(op) = self.opening.as_mut()
            && let Some(pty) = op.pty.as_mut()
        {
            for _ in 0..crate::pty::QUEUE {
                let Some(bytes) = pty.next() else { break };
                // Kept for the log: it is the only clue when `open` starts nothing.
                op.out.extend(bytes.iter().take(1024));
            }
        }
        let mut changed = false;
        loop {
            let mut more = false;
            for p in self.spaces.iter_mut().flat_map(|s| s.tabs.iter_mut()).flat_map(|t| t.panes.values_mut()) {
                let took = p.pump(QUANTUM, trace, &theme);
                changed |= took > 0;
                more |= took == QUANTUM;
            }
            if !more || start.elapsed() >= budget {
                return (changed, more);
            }
        }
    }

    /// The broker's answer to a spawn goes to the pane or the `open` helper waiting on that id; true when nobody waited and the process was sent away.
    pub fn spawned(&mut self, (id, got): crate::broker::Spawned) -> bool {
        for p in self.spaces.iter_mut().flat_map(|s| s.tabs.iter_mut()).flat_map(|t| t.panes.values_mut()) {
            p.pty.heard();
        }
        if let Some(pty) = self.opening.as_mut().and_then(|o| o.pty.as_mut()) {
            pty.heard();
        }
        if let Some(p) = self.pane_mut(id).filter(|p| p.pty.starting()) {
            match got.and_then(|(pid, fd)| p.pty.attach(fd, pid).map_err(|e| e.to_string())) {
                Ok(held) => {
                    if held {
                        let how = self.exit_how.remove(&id);
                        self.exited((id, how));
                    }
                }
                Err(e) => self.spawn_failed(id, &e),
            }
        } else if let Some(pty) = self.opening.as_mut().and_then(|o| o.pty.as_mut()).filter(|p| p.id == id && p.starting()) {
            if let Err(e) = got.and_then(|(pid, fd)| pty.attach(fd, pid).map_err(|e| e.to_string())) {
                self.open_failed(&e);
            }
        } else if self.pane(id).is_none() {
            if let Some(b) = crate::broker::handle() {
                b.kill(id);
            }
            return true;
        }
        self.dirty = true;
        false
    }

    /// A broker silent for its whole wait fails every spawn still waiting on it.
    pub fn tick_spawns(&mut self) -> bool {
        let now = Instant::now();
        let late: Vec<PaneId> = self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .flat_map(|t| t.panes.values())
            .filter(|p| p.pty.until().is_some_and(|u| now >= u))
            .map(|p| p.id)
            .collect();
        for id in &late {
            self.spawn_failed(*id, "the broker did not answer");
        }
        let open_late = self.opening.as_ref().and_then(|o| o.pty.as_ref()).and_then(|p| p.until()).is_some_and(|u| now >= u);
        if open_late {
            self.open_failed("the broker did not answer");
        }
        !late.is_empty() || open_late
    }

    /// The latest a spawn still in flight may be answered by; None when nothing waits.
    pub fn spawn_wait(&self) -> Option<Instant> {
        let panes = self.spaces.iter().flat_map(|s| s.tabs.iter()).flat_map(|t| t.panes.values()).map(|p| &p.pty);
        panes.chain(self.opening.as_ref().and_then(|o| o.pty.as_ref())).filter_map(|p| p.until()).max()
    }

    /// Still starting, the pane is closed rather than given a shell: `reshell_pane` checks.
    /// A failure never detaches the client: the last space gone, one fresh space is tried, as an attach to nothing does.
    fn spawn_failed(&mut self, id: PaneId, why: &str) {
        let what = self.pane(id).map(|p| p.argv.join(" ")).unwrap_or_default();
        let cwd = self.locate(id).map(|(si, _)| self.spaces[si].cwd.clone());
        let program = self.pane(id).map(|p| program_only(&p.argv)).unwrap_or_default();
        crate::log::error(&format!("pane {id}: failed to launch {program}: {why}"));
        self.notice = Some(format!("failed to launch {what}: {why}"));
        let quit = self.quit;
        self.drop_pane(id, None);
        self.quit = quit;
        if self.spaces.is_empty() && self.fresh != Some(id) {
            let cwd = cwd.filter(|c| c.is_dir()).or_else(crate::paths::home).unwrap_or_else(|| "/".into());
            // The id `open_space` takes: if that one fails too, nothing more is tried.
            self.fresh = Some(self.next_id);
            self.open_space(cwd);
        }
    }

    /// The broker says the pane's process left; a notice already settled on a pane that was given a new shell names a live process.
    pub fn exited(&mut self, (id, how): crate::broker::Exit) {
        let how = how.unwrap_or_else(|| "an older broker does not say how".into());
        if self.pane_mut(id).is_some_and(|p| p.pty.hold_exit()) {
            self.exit_how.insert(id, how);
            return;
        }
        let live = self.pane(id).and_then(|p| p.pty.child_pid).is_some_and(|pid| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok());
        if !live && !self.exits.iter().any(|(e, _)| *e == id) {
            self.exits.push((id, Instant::now() + EXIT_WAIT));
            self.exit_how.insert(id, how);
        }
    }

    /// Closes a pane only once its reader ended or the wait ran out, so its last lines reach the screen.
    pub fn settle_exits(&mut self) -> bool {
        let trace = self.trace;
        let theme = self.theme;
        let mut closed = false;
        for (id, until) in std::mem::take(&mut self.exits) {
            let Some(p) = self.pane_mut(id) else {
                self.exit_how.remove(&id);
                continue;
            };
            p.pump(crate::pty::QUEUE, trace, &theme);
            if p.pty.ended || Instant::now() >= until {
                // The pane's process is its shell: when that leaves, there is no pane left.
                let how = self.exit_how.remove(&id).unwrap_or_else(|| "it left during the update, how is not known".into());
                self.drop_pane(id, Some(&how));
                closed = true;
            } else {
                self.exits.push((id, until));
            }
        }
        closed
    }

    /// A plugin's `report_state`, written where the hook writes, so one reader serves both.
    pub fn plugin_report(&mut self, plugin: &str, pane: PaneId, state: &str) {
        let agent = self.pane(pane).and_then(|p| p.agent.clone());
        match crate::plugins::may_report(plugin, agent.as_deref(), state) {
            Ok(()) => {
                if let Err(e) = crate::hook::write_state(pane, state) {
                    crate::log::warn_limited("plugin state not written", &format!("plugin {plugin}: pane {pane} state not written: {e}"));
                }
            }
            Err(e) => crate::log::warn_limited("plugin report refused", &format!("plugin {plugin}: report for pane {pane} refused: {e}")),
        }
    }

    /// Asks the process table once a second what runs in each pane; the answer comes back through `take_results`.
    fn ask_foregrounds(&mut self) {
        if self.probed_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.probed_at = Instant::now();
        let panes = self.spaces.iter().flat_map(|s| s.tabs.iter()).flat_map(|t| t.panes.values());
        self.probe.ask(panes.map(|p| ProbeAsk { id: p.id, pid: p.pty.shell_pid().map(|c| c as i32), agent: p.agent.clone(), key: p.activity_key }).collect());
        if let Some(run) = &self.run
            && let Some((_, session)) = self.pane(run.pane).and_then(|p| p.agent_session.as_ref()).filter(|(a, _)| a == "claude")
        {
            let from = run.transcript.as_ref().map_or(0, |t| t.offset);
            self.transcripts.ask(crate::transcript::Ask { pane: run.pane, id: run.id.clone(), session: session.clone(), from });
        }
    }

    /// Applies what the workers finished since the last turn; true when something shown changed.
    pub fn take_results(&mut self) -> bool {
        let found: Vec<_> = self.probe.take().flatten().collect();
        self.apply_foregrounds(found) | self.take_git() | self.take_preview() | self.take_transcript()
    }

    /// A read of the open run's transcript; one for a run since closed is dropped.
    fn take_transcript(&mut self) -> bool {
        let mut changed = false;
        for f in self.transcripts.take().collect::<Vec<_>>() {
            if let Some(run) = self.run.as_mut().filter(|r| r.pane == f.pane && r.id == f.id) {
                changed |= run.take(f);
            }
        }
        changed
    }

    pub(super) fn apply_foregrounds(&mut self, found: Vec<Probed>) -> bool {
        let mut changed = false;
        for f in found {
            if let Some(p) = self.pane_mut(f.id) {
                // A new hook state shows on the next tick, which reads the state anyway.
                let before = (p.agent.clone(), p.command.clone(), p.activity_key);
                p.apply_probe(f);
                changed |= before != (p.agent.clone(), p.command.clone(), p.activity_key);
            }
        }
        changed
    }

    /// Updates pane states and clears "done" from the focused pane. Returns whether
    /// anything shown changed; without it the sidebar never updates.
    pub fn tick_states(&mut self) -> bool {
        self.ask_foregrounds();
        let focused = self
            .space()
            .and_then(|sp| sp.tab().map(|t| (t.layout.focused(), self.active, sp.active_tab)));
        let mut changed = false;
        // Collected while holding the borrow on workspaces; handed to the notifier once it ends.
        let mut seen = Vec::new();
        // a line under an agent counts time too
        let mut timers = false;
        let mut stalled = false;
        let mut panes = HashMap::new();
        for (si, sp) in self.spaces.iter_mut().enumerate() {
            for (ti, tab) in sp.tabs.iter_mut().enumerate() {
                for p in tab.panes.values_mut() {
                    stalled |= std::mem::take(&mut p.pty.stalled);
                    let before = (p.term.shown(), p.agent.clone(), p.command.clone(), p.activity_key);
                    changed |= p.term.tick_state();
                    let is_focused = focused.is_some_and(|(fid, fs, ft)| fid == p.id && fs == si && ft == ti);
                    if is_focused {
                        p.term.mark_seen();
                    }
                    changed |= before != (p.term.shown(), p.agent.clone(), p.command.clone(), p.activity_key);
                    let agent = p.agent.clone().map(|a| (a, format!("{:?}", p.term.shown()).to_lowercase()));
                    panes.insert(p.id, (p.cwd.clone(), agent));
                    let now = crate::hook::now();
                    timers |= p.activity.subs.iter().any(|s| s.shown(now));
                    // Notifications are only for agents: plain terminals are "ready" after every command.
                    if p.agent.is_some() {
                        seen.push((p.id, p.term.shown(), is_focused));
                    }
                }
            }
        }
        // the sidebar counts how long agents wait: once a second a frame, while any does
        let waits = seen.iter().any(|(_, s, _)| matches!(s, AgentState::Done | AgentState::Blocked | AgentState::Failed));
        if (waits || timers) && self.wait_tick.elapsed() >= Duration::from_secs(1) {
            self.wait_tick = Instant::now();
            changed = true;
        }
        if stalled {
            self.notice = Some("pane is not reading input".into());
            changed = true;
        }
        self.plugins.panes(panes);
        let live: Vec<usize> = seen.iter().map(|(id, ..)| *id).collect();
        self.notifier.prune(&live);
        for (id, state, is_focused) in seen {
            let bell = self.notifier.observe(id, state, is_focused, &self.cfg.notify);
            self.to_client.extend(bell);
        }
        changed
    }

    /// Saved to config after half a second of quiet, not on every drag tick, so the file doesn't fall behind the screen.
    pub(super) fn save_cfg_soon(&mut self) {
        self.cfg_save_pending = Some(Instant::now());
        self.dirty = true;
    }

    /// A neutral message leaves by itself; an error stays until it is read or a key is pressed.
    fn expire_toast(&mut self) {
        match (&self.toast, &self.toast_since) {
            (Some(t), Some((seen, at))) if t == seen => {
                if at.elapsed() >= TOAST {
                    self.toast = None;
                    self.toast_since = None;
                    self.dirty = true;
                }
            }
            (Some(t), _) => self.toast_since = Some((t.clone(), Instant::now())),
            (None, _) => self.toast_since = None,
        }
    }

    pub fn tick_cfg(&mut self) {
        self.expire_toast();
        if self.cfg_save_pending.is_some_and(|t| t.elapsed() >= Duration::from_millis(500)) {
            self.cfg_save_pending = None;
            self.save_cfg();
        }
        self.reload_cfg_if_edited();
    }

    /// The file is the source of truth for colors, so an edit in an editor lands without a restart.
    /// Skipped while our own save is pending, or it would read back the file we are about to replace.
    fn reload_cfg_if_edited(&mut self) {
        if self.cfg_save_pending.is_some() || self.cfg_poll_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.cfg_poll_at = Instant::now();
        self.reload_plugins_if_changed();
        self.ask_staged_plugin();
        let seen = crate::config::mtime();
        if seen == self.cfg_mtime {
            return;
        }
        self.reload_cfg();
    }

    /// A plugin added, updated, removed or turned off or on outside the server: only the ones whose manifest changed
    /// restart. A folder that no lock names and that was not loaded waits for Reload plugins.
    pub(super) fn reload_plugins_if_changed(&mut self) {
        let seen = crate::install::seen();
        if seen == self.plugins_seen {
            return;
        }
        self.plugins_seen = seen;
        let before = crate::agents::all();
        let pinned: HashSet<String> = crate::install::read_lock().unwrap_or_default().into_iter().map(|p| p.name).collect();
        // A plugin just turned on was known before it was turned off, pinned or not.
        let now_off: HashSet<String> = crate::install::read_off().unwrap_or_default().into_keys().collect();
        // Only a plugin that was loaded when it was turned off: a folder dropped in and toggled still waits.
        let now_off: HashSet<String> = now_off.into_iter().filter(|n| self.plugins_off.contains(n) || before.iter().any(|m| m.name == *n)).collect();
        let was_off = std::mem::replace(&mut self.plugins_off, now_off);
        let after = crate::agents::reload_admitting(&|n| pinned.contains(n) || was_off.contains(n) || before.iter().any(|m| m.name == n));
        let commits = std::mem::replace(&mut self.plugins_commits, crate::install::commits());
        for m in before.iter().filter(|m| !m.builtin) {
            // Removed and added again within one poll, or updated with only its code changed: the manifest reads the same.
            let moved = commits.get(&m.name) != self.plugins_commits.get(&m.name);
            if moved || after.iter().find(|a| a.name == m.name).map(|a| format!("{a:?}")) != Some(format!("{m:?}")) {
                self.plugins.restart(&m.name);
            }
        }
        self.dirty = true;
    }

    /// A pane fetched a plugin: the question is built here from the staged files, never from what the pane wrote.
    /// Over the Plugins window too, which comes back once it is answered; it ends the window's fetch.
    pub(super) fn ask_staged_plugin(&mut self) {
        let over = matches!(self.overlay, Overlay::Plugins(_));
        if !over && !matches!(self.overlay, Overlay::None) {
            return;
        }
        let Some(id) = crate::install::ready_ids().into_iter().find(|id| !self.plugins_asked.contains(id)) else { return };
        self.plugins_asked.insert(id.clone());
        let job = self.plugin_job.take_if(|j| j.done.is_none());
        self.dirty = true;
        match crate::install::load_ready(&id, true) {
            Ok(r) => {
                if over {
                    self.plugins_back = Some(r.staged.pin.name.clone());
                }
                self.overlay = plugin_question(&r, &id);
            }
            Err(e) => {
                crate::install::discard(&id);
                let why = crate::render::unspoofed(&e).lines().next().unwrap_or_default().to_string();
                match job {
                    Some(j) => {
                        self.plugin_job = Some(j);
                        self.fetch_said(Err(format!("what it fetched was refused: {why}")));
                    }
                    None => self.notice = Some(format!("a staged plugin was refused: {why}")),
                }
            }
        }
    }

    /// The broker's word on a process it ran: the Plugins window's fetch, or a plugin.
    pub fn exec_note(&mut self, n: crate::broker::ExecNote) {
        use crate::broker::ExecNote;
        if self.release_note(&n) {
            return;
        }
        let (ExecNote::Started { id, .. } | ExecNote::Failed { id, .. } | ExecNote::Exited { id, .. }) = &n;
        if !self.plugin_job.as_ref().is_some_and(|j| j.exec == *id && j.done.is_none()) {
            return self.plugins.note(n);
        }
        match n {
            ExecNote::Started { .. } => {}
            ExecNote::Failed { message, .. } => self.fetch_said(Err(format!("git did not start: {message}"))),
            ExecNote::Exited { .. } => {
                self.ask_staged_plugin();
                // Staged while a dialog is up: the next tick asks, and that ends the job.
                if !self.plugin_job.as_ref().is_some_and(|j| j.done.is_none()) || crate::install::ready_ids().iter().any(|id| !self.plugins_asked.contains(id)) {
                    return;
                }
                let log = std::fs::read_to_string(crate::install::fetch_log()).unwrap_or_default();
                let lines: Vec<String> = log.lines().map(|l| crate::render::unspoofed(l.trim())).filter(|l| !l.is_empty()).collect();
                let update = self.plugin_job.as_ref().is_some_and(|j| j.act == crate::plugin_list::Act::Update);
                self.fetch_said(match lines.last().map(|l| l.strip_prefix("nerd: ")) {
                    Some(Some(why)) => Err(crate::plugin_list::failure(why)),
                    None => Err("git stopped before it said anything".into()),
                    Some(None) if update => Ok(format!("Nothing newer: {}", lines.join(" "))),
                    Some(None) => Ok(lines.join(" ")),
                });
            }
        }
    }

    /// The fetch is over: the window says so on its row, or a message does when the window is closed.
    fn fetch_said(&mut self, said: Result<String, String>) {
        let Some(j) = self.plugin_job.as_mut() else { return };
        j.done = Some(said);
        self.dirty = true;
        if matches!(self.overlay, Overlay::Plugins(_)) {
            return self.plugins_renew();
        }
        let line = j.lines().remove(0);
        match j.done {
            Some(Err(_)) => self.notice = Some(line),
            _ => self.toast = Some(line),
        }
        self.plugin_job = None;
    }

    /// The server started from a config.json that did not parse.
    pub fn mark_cfg_broken(&mut self) {
        self.cfg_broken = true;
        self.notice = Some(CFG_BROKEN.into());
    }

    /// The one writer of config.json; a file that does not parse is never overwritten with defaults.
    pub(super) fn save_cfg(&mut self) {
        if self.cfg_broken {
            return;
        }
        // An edit made since we last looked wins; the next change saves on top of it.
        if crate::config::mtime() != self.cfg_mtime {
            self.reload_cfg();
            return;
        }
        if let Err(e) = crate::config::save(&self.cfg) {
            self.notice = Some(format!("config failed to save: {e}"));
        }
        self.cfg_mtime = crate::config::mtime();
    }

    /// Reads the file again; one that does not parse leaves the last good settings in place.
    pub(super) fn reload_cfg(&mut self) {
        // own clock patterns may have changed: write the clock again
        self.clock_key = None;
        let (fresh, broken) = crate::config::load_checked();
        self.cfg_mtime = crate::config::mtime();
        self.cfg_broken = broken;
        self.dirty = true;
        if broken {
            self.notice = Some(CFG_BROKEN.into());
            return;
        }
        if self.notice.as_deref() == Some(CFG_BROKEN) {
            self.notice = None;
        }
        let restyled = fresh.colors != self.cfg.colors || fresh.theme != self.cfg.theme;
        self.cfg = fresh;
        if restyled {
            self.apply_theme();
        }
    }

    /// Names this server's instance and retitles the window at once; a refusal says why.
    pub fn rename_instance(&mut self, id: &str, name: &str) {
        match crate::session::set_name(id, name) {
            Ok(()) => {
                let label = crate::session::instance_label(id);
                // Appended, not assigned: other bytes may already be queued this tick.
                let at = self.caps.machine.as_ref().map(|m| format!(" @ {m}")).unwrap_or_default();
                self.to_client.extend(format!("\x1b]2;nerd {}\x07", crate::render::unspoofed(&format!("{label}{at}"))).into_bytes());
                self.toast = Some(match name.is_empty() {
                    true => format!("server {id} has no name now"),
                    false => format!("this server is {label}: nerd --resume {label}"),
                });
            }
            Err(e) => self.notice = Some(format!("not renamed: {e}")),
        }
        self.dirty = true;
    }

    /// Right after our own edits: git marks for the tree.
    pub(super) fn refresh_tree_marks(&mut self) {
        self.ask_marks();
    }

    fn ask_marks(&mut self) {
        let Some(cwd) = self.spaces.get(self.active).map(|s| s.cwd.clone()) else { return };
        self.git.ask(GitAsk::Marks(cwd));
    }

    /// Every 3 s: the branches, and the tree's marks while it is open; the answers redraw only if they changed.
    pub fn tick_git(&mut self) {
        if self.git_at.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.git_at = Instant::now();
        self.git.ask(GitAsk::Branches(self.spaces.iter().map(|s| s.cwd.clone()).collect()));
        if self.tree.is_some() {
            self.ask_marks();
        }
    }

    /// The top row's clock as the menu bar writes it; true when its text changed.
    pub fn tick_clock(&mut self) -> bool {
        // Empty draws no clock, and its room goes to the plugins' segments.
        if self.cfg.clock == crate::config::Clock::Off {
            self.clock_key = None;
            return !std::mem::take(&mut self.clock).is_empty();
        }
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let shows = self.cfg.clock;
        // the formatter runs once a minute, or once a second when the menu bar shows seconds
        let ticks = |secs_too: bool| if secs_too { secs } else { secs / 60 };
        if self.clock_key.is_some_and(|(k, s, secs_too)| s == shows && k == ticks(secs_too)) {
            return false;
        }
        let (now, secs_too) = crate::clock::now(shows == crate::config::Clock::Date, &self.cfg.clock_time, &self.cfg.clock_date);
        self.clock_key = Some((ticks(secs_too), shows, secs_too));
        now != std::mem::replace(&mut self.clock, now.clone())
    }

    /// The battery's charge as the reader last saw it; true when it changed.
    pub fn tick_battery(&mut self) -> bool {
        // a pointer that left the window upward sends no event, so the details go by themselves
        let gone = self.battery_hover.is_some_and(|t| t.elapsed() >= crate::battery::HOVER);
        if gone {
            self.battery_hover = None;
        }
        self.battery_seen(crate::battery::now()) | gone
    }

    /// Takes the charge the reader saw; true when the row changes.
    pub(super) fn battery_seen(&mut self, now: Option<crate::battery::Charge>) -> bool {
        let now = now.filter(|_| self.cfg.battery != crate::config::Battery::Off);
        let critical = now.filter(|b| b.level(self.cfg.battery_low, self.cfg.battery_critical) == crate::battery::Level::Critical);
        // told once; again only once the charge rose above the threshold, and never over another notice
        if let Some(b) = critical
            && !self.battery_warned
            && self.notice.is_none()
        {
            self.notice = Some(format!("Battery at {}%: plug in the charger.", b.percent));
            self.battery_warned = true;
        }
        if now.is_some_and(|b| b.percent > self.cfg.battery_critical) {
            self.battery_warned = false;
        }
        now != std::mem::replace(&mut self.battery, now)
    }

    /// What git says about a folder, asked once when a space opens there, before any frame shows it.
    pub(super) fn learn_repo(&mut self, cwd: &std::path::Path) {
        if !self.repos.contains_key(cwd) {
            let key = crate::git::group_key(cwd);
            let main = key.is_some() && crate::git::is_main_checkout(cwd);
            self.repos.insert(cwd.to_path_buf(), Repo { key, main, branch: crate::chrome::git_branch(cwd) });
        }
    }

    /// Worktree group key for a folder.
    pub fn group_key_of(&mut self, cwd: &std::path::Path) -> Option<PathBuf> {
        self.learn_repo(cwd);
        self.repos.get(cwd).and_then(|r| r.key.clone())
    }

    /// How the tree's images show: from the settings and what the client terminal can do.
    fn image_view(&self) -> crate::preview::ImageView {
        use crate::config::ImageMode;
        use crate::preview::{BlockHint, ImageView};
        match self.cfg.images {
            ImageMode::Off => ImageView::Off,
            ImageMode::Blocks => ImageView::Blocks { hint: BlockHint::None },
            // Kitty bytes only reach a terminal that answered the graphics query; tmux/screen would mangle them.
            ImageMode::Auto if self.graphics && !self.caps.mux => ImageView::Pixels,
            ImageMode::Auto if self.caps.mux => ImageView::Blocks { hint: BlockHint::Tmux },
            ImageMode::Auto => ImageView::Blocks { hint: BlockHint::Terminal },
        }
    }

    /// Before each frame: the tree follows the active space, and its preview is asked for when the selection moved.
    pub fn sync_tree(&mut self) {
        self.take_answers();
        let cwd = self.spaces.get(self.active).map(|s| s.cwd.clone()).unwrap_or_default();
        let (on, wrap, active, view) = (self.cfg.preview, self.cfg.preview_wrap, self.focus == Focus::Tree, self.image_view());
        let Some(tree) = self.tree.as_mut() else { return };
        tree.set_root(cwd);
        tree.preview_on = on;
        tree.active = active;
        tree.wrap = wrap;
        tree.set_image_view(view);
        tree.follow(&self.view.tree);
        if !on {
            return;
        }
        // A client that shows pixels can attach at any time, so the PNG is asked for whenever the view wants it.
        let (ask, whole, hidden, space, need) = (tree.next_ask(), tree.whole_file(), tree.hidden(), tree.space().to_path_buf(), tree.next_need());
        if let Some(w) = ask {
            self.preview_asked = None;
            if w.diff {
                self.git.ask(GitAsk::Diff(space, w));
            } else {
                self.preview.ask(PreviewAsk::Load(w.clone(), hidden));
                if whole {
                    self.ask_view(w);
                }
            }
        }
        if let Some(n) = need {
            self.preview.ask(PreviewAsk::Make(n));
        }
    }

    /// The one source that previews this file's type is asked, off the loop; the plain preview shows until it answers.
    pub(super) fn ask_view(&mut self, w: crate::tree::Wanted) {
        let Some(ext) = crate::previews::extension(&w.path) else { return };
        let Some(by) = crate::plugins::preview_owner(&ext, self.builtins, &crate::agents::all()) else { return };
        self.preview_req += 1;
        let req = self.preview_req;
        let (cols, rows) = self.view.tree.preview_size();
        if by.0 {
            // A plugin that cannot be started never answers, and the plain preview stays.
            let mode = self.preview_modes.get(&by.1).cloned();
            let _ = self.plugins.command(&by.1, crate::plugins::Event::PreviewRequest { req, path: w.path.clone(), cols, rows, mode });
        } else {
            self.preview.ask(PreviewAsk::View(req, w.path.clone(), cols));
        }
        self.preview_asked = Some(Asked { req, by, w, shown: false });
    }

    /// Preview answers: the one asked for the selection still wanted replaces the plain preview; late or stray ones are dropped.
    pub(super) fn take_answers(&mut self) -> bool {
        let mut changed = false;
        for a in std::mem::take(&mut self.plugins.slots.answers) {
            let (Some(asked), Some(tree)) = (self.preview_asked.as_mut().filter(|k| k.req == a.req && k.by.1 == a.source), self.tree.as_mut()) else { continue };
            let by = if asked.by.0 { asked.by.1.clone() } else { crate::previews::kind(&crate::previews::extension(&asked.w.path).unwrap_or_default()).to_string() };
            if tree.set_preview(&mut self.view.tree, &asked.w, crate::preview::Preview::styled(&asked.w.path, a.lines, a.cut, &by, a.modes, a.mode)) {
                asked.shown = true;
                changed = true;
            }
        }
        changed
    }

    fn take_preview(&mut self) -> bool {
        let mut changed = false;
        let found: Vec<PreviewFound> = self.preview.take().collect();
        let Some(tree) = self.tree.as_mut() else { return false };
        for f in found {
            changed |= match f {
                // An answer that came first stays: the plain preview never replaces it.
                PreviewFound::Loaded(w, _) if self.preview_asked.as_ref().is_some_and(|a| a.shown && a.w == w) => false,
                PreviewFound::Loaded(w, p) => tree.set_preview(&mut self.view.tree, &w, p),
                PreviewFound::Png(path, png) => tree.give_png(&path, png),
                PreviewFound::Pixels(path, px) => tree.give_pixels(&path, px),
                PreviewFound::View(m) => self.plugins.slots.take(&crate::slots::Decl::builtin("previews"), m),
            };
        }
        changed | self.take_answers()
    }

    fn take_git(&mut self) -> bool {
        let found: Vec<GitFound> = self.git.take().collect();
        found.into_iter().fold(false, |changed, f| self.apply_git(f) | changed)
    }

    pub(super) fn apply_git(&mut self, f: GitFound) -> bool {
        let mut changed = false;
        match f {
            GitFound::Diff(w, text) => {
                let Some(tree) = self.tree.as_mut() else { return false };
                // An untracked file has no diff, and falls back to itself; only if still wanted, or it would crowd out the load that is.
                if text.is_empty() {
                    if tree.wanted().as_ref() == Some(&w) {
                        self.preview.ask(PreviewAsk::Load(w, tree.hidden()));
                    }
                } else {
                    let p = crate::preview::Preview::from_text(&w.path, &text, crate::preview::Lang::Diff);
                    changed |= tree.set_preview(&mut self.view.tree, &w, p);
                }
            }
            GitFound::Marks(cwd, marks) => {
                let here = self.spaces.get(self.active).is_some_and(|s| s.cwd == cwd);
                if let Some(tree) = self.tree.as_mut().filter(|_| here) {
                    changed |= tree.set_git_marks(marks);
                }
            }
            GitFound::Branches(list) => {
                for (cwd, branch) in list {
                    if let Some(r) = self.repos.get_mut(&cwd)
                        && r.branch != branch
                    {
                        r.branch = branch;
                        changed = true;
                    }
                }
            }
        }
        changed
    }
}

/// A pane as the probe worker needs to know it.
pub struct ProbeAsk {
    id: PaneId,
    pid: Option<i32>,
    /// The agent that ran there at the last look.
    agent: Option<String>,
    key: [Option<(std::time::SystemTime, u64)>; 2],
}

/// What runs in a pane and what its agent's hook wrote.
pub struct Probed {
    pub id: PaneId,
    /// None when the process table had no answer: the previous one stands.
    pub fg: Option<crate::detect::Foreground>,
    pub hook: Option<AgentState>,
    /// The agent and its session id.
    pub session: Option<(String, String)>,
    pub key: [Option<(std::time::SystemTime, u64)>; 2],
    /// Read again only when its files changed.
    pub activity: Option<crate::hook::Activity>,
    /// The folder its shell is in now; None when the kernel did not say.
    pub cwd: Option<PathBuf>,
}

/// Runs on the probe worker, once a second: the process table parsed once for every pane, then each agent's hook files.
pub(super) fn probe_work(mut jobs: Vec<Vec<ProbeAsk>>) -> Vec<Vec<Probed>> {
    let panes = jobs.pop().unwrap_or_default();
    let procs = crate::detect::procs();
    let found = panes
        .into_iter()
        .map(|p| {
            let fg = procs.as_deref().zip(p.pid).and_then(|(t, pid)| crate::detect::foreground_of(t, pid));
            let name = fg.as_ref().map_or(p.agent.clone(), |f| f.agent.clone());
            let agent = name.is_some();
            // Clear only on departure: the hook writes before the process poll
            // notices a new launch, so clearing on arrival would race with it.
            if p.agent.is_some() && !agent {
                crate::hook::clear_state(p.id);
            }
            let key = if agent { crate::hook::activity_key(p.id) } else { [None, None] };
            Probed {
                id: p.id,
                fg,
                hook: agent.then(|| crate::hook::read_state(p.id)).flatten(),
                session: name.and_then(|n| crate::hook::read_session(p.id, &n)),
                activity: (agent && key != p.key).then(|| crate::hook::read_activity(p.id)),
                key,
                cwd: p.pid.and_then(crate::detect::cwd_of),
            }
        })
        .collect();
    vec![found]
}

pub enum GitAsk {
    /// The status marks of the repository around this folder.
    Marks(PathBuf),
    /// The diff of the wanted file, taken in the space's folder.
    Diff(PathBuf, crate::tree::Wanted),
    Branches(Vec<PathBuf>),
}

pub enum GitFound {
    Marks(PathBuf, HashMap<PathBuf, char>),
    Diff(crate::tree::Wanted, String),
    Branches(Vec<(PathBuf, Option<String>)>),
}

pub enum PreviewAsk {
    /// The wanted file or folder; the flag lists hidden entries.
    Load(crate::tree::Wanted, bool),
    Make(crate::preview::Need),
    /// A public built-in's view of this file, for request `req`, `cols` wide.
    View(u64, PathBuf, u16),
}

/// A preview asked of a plugin or a public built-in: its number, its source, the selection, and whether its answer shows.
pub struct Asked {
    pub(super) req: u64,
    pub(super) by: crate::slots::Src,
    pub(super) w: crate::tree::Wanted,
    pub(super) shown: bool,
}

pub enum PreviewFound {
    Loaded(crate::tree::Wanted, crate::preview::Preview),
    View(crate::proto::PluginMsg),
    Png(PathBuf, Option<std::sync::Arc<Vec<u8>>>),
    Pixels(PathBuf, Option<std::sync::Arc<crate::preview::Pixels>>),
}

/// What `sips` made, by path and mtime; a full-size PNG runs to tens of MB, so only the last `PREVIEW_KEEP` stay.
struct Made<T>(Vec<((PathBuf, Option<std::time::SystemTime>), T)>);

const PREVIEW_KEEP: usize = 2;

impl<T: Clone> Made<T> {
    fn get(&mut self, p: &PathBuf, make: impl FnOnce() -> T) -> T {
        let key = (p.clone(), std::fs::metadata(p).and_then(|m| m.modified()).ok());
        if let Some(i) = self.0.iter().position(|(k, _)| *k == key) {
            let hit = self.0.remove(i);
            self.0.push(hit);
        } else {
            if self.0.len() >= PREVIEW_KEEP {
                self.0.remove(0);
            }
            self.0.push((key, make()));
        }
        self.0.last().map(|(_, v)| v.clone()).expect("just pushed")
    }
}

/// Runs on the preview worker: only the newest load counts (the tree's mark is always the newest ask), and what `sips` made is kept by path and mtime.
pub(super) fn preview_work() -> impl FnMut(Vec<PreviewAsk>) -> Vec<PreviewFound> {
    use crate::preview::Need;
    // Shared, so a hit hands over a handle, not tens of MB.
    let mut pngs: Made<Option<std::sync::Arc<Vec<u8>>>> = Made(Vec::new());
    let mut pixels: Made<Option<std::sync::Arc<crate::preview::Pixels>>> = Made(Vec::new());
    move |jobs| {
        let (mut load, mut png, mut px, mut view) = (None, None, None, None);
        for j in jobs {
            match j {
                PreviewAsk::View(req, p, cols) => view = Some((req, p, cols)),
                PreviewAsk::Load(w, hidden) => load = Some((w, hidden)),
                PreviewAsk::Make(Need::Png(p)) => png = Some(p),
                PreviewAsk::Make(Need::Pixels(p, dims)) => px = Some((p, dims)),
            }
        }
        let mut out = Vec::new();
        if let Some((w, hidden)) = load {
            let p = crate::tree::load(&w.path, hidden);
            out.push(PreviewFound::Loaded(w, p));
        }
        if let Some(m) = view.and_then(|(req, p, cols)| crate::previews::answer(req, &p, cols)) {
            out.push(PreviewFound::View(m));
        }
        if let Some(p) = png {
            let made = pngs.get(&p, || crate::preview::to_png(&p));
            out.push(PreviewFound::Png(p, made));
        }
        if let Some((p, dims)) = px {
            let made = pixels.get(&p, || crate::preview::to_pixels(&p, dims));
            out.push(PreviewFound::Pixels(p, made));
        }
        out
    }
}

/// A diff that failed falls back to the file; marks and branches are asked again on their tick.
pub(super) fn git_failed(j: &GitAsk) -> Option<GitFound> {
    match j {
        GitAsk::Diff(_, w) => Some(GitFound::Diff(w.clone(), String::new())),
        _ => None,
    }
}

/// A load or conversion that failed is still answered, so the tree does not wait on it forever; a view is not waited on.
pub(super) fn preview_failed(j: &PreviewAsk) -> Option<PreviewFound> {
    use crate::preview::Need;
    Some(match j {
        PreviewAsk::View(..) => return None,
        PreviewAsk::Load(w, _) => PreviewFound::Loaded(w.clone(), crate::preview::Preview::note(&w.path, "the preview failed; the log says why")),
        PreviewAsk::Make(Need::Png(p)) => PreviewFound::Png(p.clone(), None),
        PreviewAsk::Make(Need::Pixels(p, _)) => PreviewFound::Pixels(p.clone(), None),
    })
}

/// Runs on the git worker. Of each kind only the newest job counts; the diff goes first, a human waits on it.
pub(super) fn git_work(jobs: Vec<GitAsk>) -> Vec<GitFound> {
    let (mut marks, mut diff, mut branches) = (None, None, None);
    for j in jobs {
        match j {
            GitAsk::Marks(c) => marks = Some(c),
            GitAsk::Diff(space, w) => diff = Some((space, w)),
            GitAsk::Branches(v) => branches = Some(v),
        }
    }
    let mut out = Vec::new();
    if let Some((space, w)) = diff {
        let text = crate::git::diff(&space, &w.path);
        out.push(GitFound::Diff(w, text));
    }
    if let Some(cwd) = marks {
        let m = crate::git::status_marks(&cwd);
        out.push(GitFound::Marks(cwd, m));
    }
    if let Some(dirs) = branches {
        out.push(GitFound::Branches(dirs.into_iter().map(|d| { let b = crate::chrome::git_branch(&d); (d, b) }).collect()));
    }
    out
}

/// A command as the log keeps it: the program and how many arguments followed, never the arguments.
pub(super) fn program_only(argv: &[String]) -> String {
    match argv {
        [] => String::new(),
        [p] => p.clone(),
        [p, rest @ ..] => format!("{p} with {} argument{}", rest.len(), if rest.len() == 1 { "" } else { "s" }),
    }
}

/// Command name from the launch argv, before the system answers what's really there.
pub(super) fn argv_name(argv: &[String]) -> String {
    argv.first()
        .map(|a| {
            std::path::Path::new(a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| a.clone())
        })
        .unwrap_or_else(|| "shell".to_string())
}

/// What a restored pane types, said in the log: its agent's session resumed only when nobody else holds
/// it and the human confirmed the agent's resume line as it is now.
pub(super) fn restore_launch(ps: &PaneSnapshot) -> Option<Vec<String>> {
    let session = ps.agent_session.as_deref().map(|id| (ps.agent.as_str(), id));
    let m = session.and_then(|(a, _)| crate::agents::get(a));
    let live = session.is_some_and(|(_, id)| m.as_ref().is_some_and(|m| m.session_is_live(id)));
    let confirmed = m.as_ref().is_some_and(crate::hook::resume_confirmed);
    let launch = restored_command(&ps.argv, session, live || !confirmed);
    if let Some((agent, id)) = session {
        let line = launch.as_ref().map_or("nothing".to_string(), |l| l.join(" "));
        let resumes = m.as_ref().is_some_and(|m| m.resume_argv(id).is_some());
        let why = match (live, confirmed, resumes) {
            (true, _, _) => "held by another process, so not resumed",
            (_, false, true) => "not resumed: its resume line changed since it was confirmed",
            (_, _, true) => "resumed",
            _ => "not resumable",
        };
        let say = if live || (resumes && !confirmed) { crate::log::warn } else { crate::log::info };
        // Only the agent's own resume line is written whole: a command the user typed may carry a key in its arguments.
        let shown = if why == "resumed" { line } else { launch.as_deref().map_or(line, program_only) };
        say(&format!("pane {}: {agent} session {id} {why}; typed: {shown}", ps.id));
    }
    launch
}

/// What a restored pane types into its shell. A session held by another process (`live`), or one whose
/// resume line nobody confirmed, is never resumed; an old snapshot naming the agent in argv starts it fresh.
pub(super) fn restored_command(argv: &[String], session: Option<(&str, &str)>, live: bool) -> Option<Vec<String>> {
    let stored = argv
        .first()
        .and_then(|c| crate::detect::identify(c, argv))
        .map(|a| crate::agents::get(&a).map_or_else(|| argv.to_vec(), |m| m.strip_resume(argv)));
    let Some((agent, id)) = session else { return stored };
    let Some(m) = crate::agents::get(agent) else { return stored };
    (!live).then(|| m.resume_argv(id)).flatten().or(stored).or_else(|| Some(vec![m.program().to_string()]))
}

/// What a restarted pane types: its session resumed, else the agent it ran started fresh; a plain shell types nothing.
pub(super) fn restart_command(argv: &[String], session: Option<(&str, &str)>, agent: Option<&str>) -> Option<Vec<String>> {
    restored_command(argv, session, false).or_else(|| agent.and_then(crate::agents::get).map(|m| vec![m.program().to_string()]))
}

/// The line typed into a restored pane's shell; None when an argument holds a control character.
pub(super) fn restore_line(cmd: &[String]) -> Option<String> {
    let words = cmd.iter().map(|a| shell_word(a)).collect::<Option<Vec<_>>>()?;
    Some(format!("{}\n", words.join(" ")))
}

/// Whether a pane's own process was a command rather than the shell. A pane started as the
/// shell is the window itself and closes with it; one started as an agent is a window that
/// was handed a command, and the command leaving is not the window leaving.
pub(super) fn pane_was_a_command(argv: &[String], shell: &str) -> bool {
    match argv.first() {
        None => false,
        Some(first) => first != shell && std::path::Path::new(first).file_name() != std::path::Path::new(shell).file_name(),
    }
}

/// The user's shell, read from the environment.
pub(super) fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string())
}

/// The user's shell as a pane starts it: a login shell, or a server started with a bare environment hands panes no profile PATH.
pub(super) fn shell_argv() -> Vec<String> {
    vec![user_shell(), "-l".into()]
}

/// How long a neutral message stays.
pub(super) const TOAST: Duration = Duration::from_secs(4);

/// Commits shown in the window; the terminal shows up to fifty.
const PLUGIN_LOG: usize = 10;

/// What a staged plugin can do, from where, and for an update what is new, as the Plugins window asks it.
fn plugin_question(r: &crate::install::Ready, id: &str) -> Overlay {
    let s = &r.staged;
    let p = &s.pin;
    let release = p.tag.as_ref().map(|t| format!("release {t}")).unwrap_or_else(|| "no release tag".into());
    let body = format!(
        "From {}, {release}, commit {}. {} Risk: {}.",
        crate::render::unspoofed(&p.source),
        &p.commit[..p.commit.len().min(12)],
        r.words,
        crate::install::RISK[s.check.risk as usize]
    );
    let mut rows: Vec<Vec<String>> = if r.update && r.new.is_empty() {
        vec![vec!["It can do nothing it could not before.".into()]]
    } else if r.update {
        std::iter::once(vec!["It can now also:".into()]).chain(r.new.iter().map(|a| vec![format!("  {a}")])).collect()
    } else {
        std::iter::once(vec!["It can:".into()]).chain(s.check.abilities.iter().map(|a| vec![format!("  {a}")])).collect()
    };
    if !r.log.is_empty() {
        rows.push(vec![String::new()]);
        rows.push(vec!["New commits:".into()]);
        rows.extend(r.log.iter().take(PLUGIN_LOG).map(|l| vec![format!("  {l}")]));
        if r.log.len() > PLUGIN_LOG {
            rows.push(vec![format!("  and {} more", r.log.len() - PLUGIN_LOG)]);
        }
    }
    let (title, verb) = if r.update { ("Update a plugin", format!("update {}", p.name)) } else { ("Install a plugin", format!("install {}", p.name)) };
    Overlay::Confirm(crate::menu::Confirm::new(title, body).about(format!("{} {}", p.name, s.check.version.as_deref().unwrap_or_default())).rows(rows).does(&verb).click_yes(), Pending::PluginReady(id.into(), p.commit.clone()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn at_most_two_conversions_are_kept_and_a_hit_makes_nothing() {
        let dir = std::env::temp_dir().join(format!("nerd-made-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::path::PathBuf> = (0..3).map(|i| dir.join(format!("{i}.jpg"))).collect();
        for f in &files {
            std::fs::write(f, "x").unwrap();
        }
        let mut made = super::Made(Vec::new());
        let mut runs = 0;
        for f in files.iter().chain(&files[1..]) {
            made.get(f, || {
                runs += 1;
                vec![0u8; 8]
            });
            assert!(made.0.len() <= super::PREVIEW_KEEP);
        }
        assert_eq!(runs, 3, "the last two were still kept");
        made.get(&files[0], || {
            runs += 1;
            Vec::new()
        });
        assert_eq!(runs, 4, "the oldest was dropped");
        let mut shared = super::Made(Vec::new());
        let first = shared.get(&files[0], || std::sync::Arc::new(vec![0u8; 1 << 20]));
        let again = shared.get(&files[0], || unreachable!("a hit makes nothing"));
        assert!(std::sync::Arc::ptr_eq(&first, &again), "a hit hands over the same bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
    }

    /// The 3 s tick, now.
    fn tick3(app: &mut super::App) {
        app.git_at = std::time::Instant::now() - std::time::Duration::from_secs(4);
        app.tick_git();
    }

    #[test]
    fn the_critical_charge_is_told_once_until_the_battery_leaves_it() {
        use crate::battery::{Charge, Power};
        let mut app = super::snapshots::one_space();
        let at = |percent, power| Some(Charge { percent, power, low_power: false, minutes: None, watts: None });
        assert!(app.battery_seen(at(11, Power::Battery)) && app.notice.is_none(), "11% is low, not critical");
        app.battery_seen(at(10, Power::Battery));
        assert_eq!(app.notice.as_deref(), Some("Battery at 10%: plug in the charger."));
        app.notice = None;
        app.battery_seen(at(9, Power::Battery));
        assert!(app.notice.is_none(), "once, not at every percent");
        app.battery_seen(at(9, Power::Charging));
        app.battery_seen(at(8, Power::Battery));
        assert!(app.notice.is_none(), "a second on the charger is not a charge");
        app.battery_seen(at(11, Power::Charging));
        app.battery_seen(at(10, Power::Battery));
        assert!(app.notice.is_some(), "again once it rose above the threshold and ran down");
        app.notice = Some("config failed to save".into());
        app.battery_seen(at(11, Power::Battery));
        app.battery_seen(at(10, Power::Battery));
        assert_eq!(app.notice.as_deref(), Some("config failed to save"), "never over another notice");
        app.notice = None;
        app.cfg.battery = crate::config::Battery::Off;
        app.battery_seen(at(5, Power::Battery));
        assert!(app.battery.is_none() && app.notice.is_none(), "off: nothing shown, nothing told");
    }

    #[test]
    fn the_clock_goes_when_it_is_turned_off_and_comes_back_when_on() {
        let mut app = super::snapshots::one_space();
        // a fixed pattern: the menu bar's own depends on this Mac's settings
        app.cfg.clock_time = "HH:mm".into();
        assert!(app.tick_clock() && app.clock.len() == 5, "on by default: {:?}", app.clock);
        app.cfg.clock = crate::config::Clock::Off;
        assert!(app.tick_clock() && app.clock.is_empty(), "off: no clock, and a redraw");
        assert!(!app.tick_clock(), "still off: nothing to redraw");
        app.cfg.clock = crate::config::Clock::Time;
        assert!(app.tick_clock() && app.clock.len() == 5, "back on");
    }

    #[test]
    fn the_clock_redraws_only_when_the_minute_turns() {
        let mut app = super::snapshots::one_space();
        app.cfg.clock_time = "HH:mm".into();
        assert!(app.tick_clock());
        let c = app.clock.as_bytes();
        assert!(c.len() == 5 && c[2] == b':' && c.iter().enumerate().all(|(i, b)| i == 2 || b.is_ascii_digit()), "{}", app.clock);
        // a turn between the two ticks is rare and still correct: it redraws
        let again = app.tick_clock();
        assert!(!again || app.clock.ends_with(":00") || app.clock.ends_with('0'), "same minute, no redraw");
        app.cfg.clock_time = "'own' HH".into();
        assert!(!app.tick_clock() || app.clock.ends_with('0'), "the formatter waits for the minute");
        app.clock_key = None;
        assert!(app.tick_clock() && app.clock.starts_with("own "), "a reloaded config writes it at once: {}", app.clock);
    }

    #[test]
    fn with_the_tree_closed_git_status_is_not_asked() {
        let dir = std::env::temp_dir().join(format!("nerd-nostatus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        let mut app = super::snapshots::one_space();
        app.builtins = true;
        app.spaces[0].cwd = dir.clone();
        let asked = |app: &mut super::App| {
            tick3(app);
            let t = std::time::Instant::now();
            let mut found = Vec::new();
            while !found.iter().any(|f| matches!(f, super::GitFound::Branches(_))) {
                assert!(t.elapsed() < std::time::Duration::from_secs(10));
                found.extend(app.git.take());
            }
            found.iter().any(|f| matches!(f, super::GitFound::Marks(..)))
        };
        assert!(!asked(&mut app), "the tree is closed");
        app.toggle_tree();
        assert!(asked(&mut app), "and open");
        let _ = std::fs::remove_dir_all(&dir);
    }

}

