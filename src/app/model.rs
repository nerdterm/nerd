use super::*;

/// One pane: a process, its pseudoterminal, and the parsed screen state.
pub struct Pane {
    pub id: PaneId,
    pub term: Term,
    pub pty: Pty,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    /// Name given by the human. Empty means the border shows the command instead.
    pub label: Option<String>,
    /// The agent that holds a session, and its id.
    pub agent_session: Option<(String, String)>,
    /// Foreground command and agent name, read from the process table — not
    /// argv, since a human might type `claude` inside a plain shell.
    pub agent: Option<String>,
    pub command: String,
    /// What the agent and its subagents are doing, from the hook's files.
    pub activity: crate::hook::Activity,
    /// Those files as last parsed: unchanged ones are not read again.
    pub(super) activity_key: [Option<(std::time::SystemTime, u64)>; 2],
    /// A program in front of the shell left mouse reporting on.
    pub(super) mouse_left_on: bool,
    /// What the pane's programs may reach: files and network.
    pub isolation: crate::policy::Isolation,
}

impl Pane {
    pub(crate) fn new(id: PaneId, term: Term, pty: Pty, cwd: PathBuf, argv: Vec<String>) -> Self {
        Pane {
            id,
            term,
            pty,
            cwd,
            command: argv_name(&argv),
            argv,
            label: None,
            agent_session: None,
            agent: None,
            activity: Default::default(),
            activity_key: [None, None],
            mouse_left_on: false,
            isolation: Default::default(),
        }
    }

    /// Feeds up to `n` queued chunks one at a time, since `answers` reads the cursor after each; returns how many.
    pub(super) fn pump(&mut self, n: usize, trace: bool, theme: &crate::render::Theme) -> usize {
        let rgb = |c: ratatui::style::Color| match c {
            ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
            _ => (0, 0, 0),
        };
        let (fg, bg) = (rgb(theme.text), rgb(theme.ground));
        let mut took = 0;
        while took < n
            && let Some(bytes) = self.pty.next()
        {
            if trace && bytes.windows(3).any(|w| w == b"\x1b]4" || w == b"\x1b]1") {
                crate::log::debug(&format!("trace pane {} OSC: {} bytes", self.id, bytes.len()));
            }
            self.term.feed(&bytes);
            self.watch_mouse();
            let ans = self.term.answers(&bytes, fg, bg);
            if !ans.is_empty() {
                self.pty.reply(&ans);
            }
            took += 1;
        }
        took
    }

    /// A program killed with the mouse on never turns it off, and the shell after it would get every move as typed text.
    fn watch_mouse(&mut self) {
        let Some(shell) = self.pty.shell_pid().filter(|_| self.argv == super::lifecycle::shell_argv()) else { return };
        let Some(front) = self.pty.foreground() else { return };
        let (left_on, off) = mouse_step(self.mouse_left_on, front == shell as i32, self.term.wants_mouse());
        self.mouse_left_on = left_on;
        if off {
            self.term.mouse_off();
        }
    }

    /// What the pane is called: the human's name, the agent in it, or the command it runs.
    pub fn label(&self) -> String {
        self.label.clone().or_else(|| self.agent.clone()).unwrap_or_else(|| self.command.clone())
    }

    /// What the probe worker found: the foreground from the process table, and the hook's files.
    pub(super) fn apply_probe(&mut self, found: Probed) {
        // Where the shell is now: a pane beside it, a restore and plugins follow its `cd`.
        if let Some(cwd) = found.cwd {
            self.cwd = cwd;
        }
        if let Some(fg) = found.fg {
            if fg.agent != self.agent {
                self.term.set_blocked_text(crate::agents::blocked_text(fg.agent.as_deref()));
            }
            self.agent = fg.agent;
            self.command = fg.command;
        }
        self.agent_session = found.session;
        if found.key != self.activity_key {
            self.activity_key = found.key;
            let before = std::mem::take(&mut self.activity);
            if let Some(a) = found.activity {
                self.activity = a;
                self.activity.carry_starts(&before, crate::hook::now());
            }
        }
        self.term.set_hook_state(with_subagents(found.hook, self.activity.live(crate::hook::now())));
    }
}

/// While a program is in front: whether it has the mouse on. Back at the shell: turn it off if the program left it on.
pub(super) fn mouse_step(left_on: bool, shell_in_front: bool, wants: bool) -> (bool, bool) {
    if shell_in_front { (false, left_on && wants) } else { (wants, false) }
}

/// The agent ends its own turn while a background subagent works, so the hook writes `idle`
/// with work still running underneath. `blocked` stays untouched: that one is about the human.
pub(super) fn with_subagents(state: Option<AgentState>, running: bool) -> Option<AgentState> {
    match state {
        Some(AgentState::Idle) if running => Some(AgentState::Working),
        other => other,
    }
}

pub struct Tab {
    pub name: Option<String>,
    pub layout: Layout,
    pub panes: HashMap<PaneId, Pane>,
    /// The isolation a new pane here starts with; None: the space's.
    pub isolation_new: Option<crate::policy::Isolation>,
}

impl Tab {
    /// What the tab bar calls it: the human's name, or its number.
    pub fn label(&self, index: usize) -> String {
        self.name.clone().unwrap_or_else(|| (index + 1).to_string())
    }

    pub(super) fn focused_mut(&mut self) -> Option<&mut Pane> {
        let id = self.layout.focused();
        self.panes.get_mut(&id)
    }
}

/// A workspace: one project with its own set of tabs.
pub struct Space {
    /// Name given by the human. Empty means we use the folder name.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// The isolation a new pane here starts with; None: everything open.
    pub isolation_new: Option<crate::policy::Isolation>,
}

impl Space {
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.cwd
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "space".to_string())
        })
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab)
    }

    pub fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active_tab)
    }
}

impl App {
    pub fn space(&self) -> Option<&Space> {
        self.spaces.get(self.active)
    }

    pub fn space_mut(&mut self) -> Option<&mut Space> {
        self.spaces.get_mut(self.active)
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.space()?.tab()
    }

    pub fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.space_mut()?.tab_mut()
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.spaces.iter().flat_map(|sp| sp.tabs.iter()).find_map(|t| t.panes.get(&id))
    }

    /// A pane by id across every workspace and tab: output arrives from a thread that knows nothing about the layout.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.spaces
            .iter_mut()
            .flat_map(|sp| sp.tabs.iter_mut())
            .find_map(|t| t.panes.get_mut(&id))
    }

    /// Jump to a pane wherever it is: workspace, tab, focus.
    pub fn go_to(&mut self, space: usize, tab: usize, pane: PaneId) {
        if space < self.spaces.len() {
            self.active = space;
            if let Some(sp) = self.space_mut() {
                if tab < sp.tabs.len() {
                    sp.active_tab = tab;
                }
                if let Some(t) = sp.tab_mut() {
                    t.layout.focus(pane);
                }
            }
            self.dirty = true;
        }
    }

    /// Takes a pane out wherever it sits. The tab's last pane closes the tab, which cascades
    /// to the workspace if it was the last tab too.
    /// `exit`: how its process left, when that is why; other callers log their own cause.
    pub(super) fn drop_pane(&mut self, id: PaneId, exit: Option<&str>) {
        if self.run.as_ref().is_some_and(|r| r.pane == id) {
            self.run = None;
            self.run_zoom = false;
        }
        self.run_widths.retain(|p, _| *p != id);
        let Some((si, ti)) = self.locate(id) else { return };
        // A pane whose own process was a command - an agent launched as the pane itself -
        // goes back to a prompt instead of taking the tab down with it. Quitting the agent
        // is not closing the window, and the human who does it expects the shell back.
        if self.reshell_pane(si, ti, id, exit) {
            self.dirty = true;
            return;
        }
        if let Some(how) = exit {
            crate::log::info(&format!("pane {id} closed: its process left ({how})"));
        }
        let tab = &mut self.spaces[si].tabs[ti];
        if tab.layout.pane_count() > 1 {
            tab.layout.close(id);
            tab.panes.remove(&id);
        } else {
            self.close_tab(si, ti);
        }
        self.dirty = true;
    }

    /// Puts a shell back into a pane whose process was something else. Returns false for a
    /// pane that was already the shell - that one closes, as a shell always has.
    fn reshell_pane(&mut self, si: usize, ti: usize, id: PaneId, exit: Option<&str>) -> bool {
        let shell = user_shell();
        let bell = self.bell.clone();
        let panes = &mut self.spaces[si].tabs[ti].panes;
        let Some(old) = panes.remove(&id) else { return false };
        // A shell for a pane still starting would reuse an id with the broker's reply in flight.
        if !pane_was_a_command(&old.argv, &shell) || old.pty.starting() {
            panes.insert(id, old);
            return false;
        }
        // The old pty's Drop asks the broker to kill this id, and the broker keys panes by id
        // alone: it has to be gone before the new shell exists, or the kill lands on the shell
        // we just started. That is how the first attempt at this closed the tab anyway.
        let Pane { mut term, cwd, label, isolation, pty, .. } = old;
        term.modes_off();
        let (cols, rows) = (pty.cols, pty.rows);
        drop(pty);
        let pty = match Pty::spawn(id, &shell_argv(), &cwd, cols, rows, bell, &isolation) {
            Ok(pty) => pty,
            Err(e) => {
                crate::log::error(&format!("pane {id}: the shell would not start after the command left: {e}"));
                return false;
            }
        };
        let how = exit.map(|h| format!(" ({h})")).unwrap_or_default();
        crate::log::info(&format!("pane {id}: the command left{how}, the shell is back"));
        crate::hook::clear_state(id);
        // The screen is kept: what the agent printed is still worth reading above the prompt.
        self.spaces[si].tabs[ti].panes.insert(id, Pane { label, isolation, ..Pane::new(id, term, pty, cwd, shell_argv()) });
        true
    }

    /// The pane's program stops and its shell starts again, under a new id: the broker keys exit
    /// notices by id, and the old program's arrives after the kill. False: no such pane, or still starting.
    pub(crate) fn restart_pane(&mut self, id: PaneId) -> bool {
        let Some((si, ti)) = self.locate(id) else { return false };
        let Some(old) = self.spaces[si].tabs[ti].panes.get(&id) else { return false };
        if old.pty.starting() {
            return false;
        }
        let ps = pane_snapshot(old);
        let agent = old.agent.clone();
        let new = self.alloc_id();
        let mut pty = match Pty::spawn(new, &shell_argv(), &ps.cwd, ps.cols, ps.rows, self.bell.clone(), &ps.isolation) {
            Ok(pty) => pty,
            Err(e) => {
                crate::log::error(&format!("pane {id}: the shell would not start again: {e}"));
                return false;
            }
        };
        // Nobody else holds the session: nerd just stopped its holder.
        let session = ps.session().filter(|(a, _)| crate::agents::get(a).as_ref().is_some_and(crate::hook::resume_confirmed));
        let launch = super::lifecycle::restart_command(&ps.argv, session.as_ref().map(|(a, sid)| (a.as_str(), sid.as_str())), agent.as_deref());
        if let Some(line) = launch.as_deref().and_then(super::lifecycle::restore_line) {
            pty.write(line.as_bytes());
        }
        let command = argv_name(launch.as_deref().unwrap_or(&shell_argv()));
        self.restarted(id, new, pty, session, command)
    }

    /// Pane `id` becomes pane `new` in the same place, with `pty` and what it keeps of the old one.
    pub(super) fn restarted(&mut self, id: PaneId, new: PaneId, pty: Pty, agent_session: Option<(String, String)>, command: String) -> bool {
        let Some((si, ti)) = self.locate(id) else { return false };
        let Some(old) = self.spaces[si].tabs[ti].panes.remove(&id) else { return false };
        // The old pty drops here: the broker kills its program.
        let Pane { mut term, cwd, label, isolation, .. } = old;
        term.modes_off();
        let pane = Pane { label, isolation, command, agent_session, ..Pane::new(new, term, pty, cwd, shell_argv()) };
        let tab = &mut self.spaces[si].tabs[ti];
        tab.panes.insert(new, pane);
        tab.layout.replace(id, new);
        let moved: Vec<String> = self.mail.msgs.iter().filter(|m| m.to.pane() == Some(id)).map(|m| m.id.clone()).collect();
        for m in moved {
            self.mail.update(&m, |m| {
                if let crate::mail::Party::Pane { pane, .. } = &mut m.to {
                    *pane = Some(new);
                }
            });
        }
        if self.run.as_ref().is_some_and(|r| r.pane == id) {
            self.run = None;
            self.run_zoom = false;
        }
        self.run_widths.remove(&id);
        crate::hook::clear_state(id);
        crate::log::info(&format!("pane {id}: restarted by the owner as pane {new}"));
        self.dirty = true;
        true
    }

    /// Which space and tab hold a pane; panes live in any of them, not just the open one.
    pub(super) fn locate(&self, id: PaneId) -> Option<(usize, usize)> {
        self.spaces
            .iter()
            .enumerate()
            .find_map(|(si, sp)| sp.tabs.iter().position(|t| t.panes.contains_key(&id)).map(|ti| (si, ti)))
    }

    pub(super) fn pane_screen(&self, id: PaneId) -> Option<crate::term::Screen<'_>> {
        self.spaces
            .iter()
            .flat_map(|sp| sp.tabs.iter())
            .find_map(|t| t.panes.get(&id))
            .map(|p| p.term.screen())
    }

    /// Snapshot for restoring; no environment or scrollback — secrets can end up in either, and history already lives in the transcript.
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: 2,
            active_space: self.active,
            spaces: self
                .spaces
                .iter()
                .map(|sp| SpaceSnapshot {
                    name: sp.name.clone(),
                    cwd: sp.cwd.clone(),
                    active_tab: sp.active_tab,
                    isolation_new: sp.isolation_new.clone(),
                    tabs: sp
                        .tabs
                        .iter()
                        .map(|t| TabSnapshot {
                            name: t.name.clone(),
                            layout: t.layout.snapshot(),
                            focused: t.layout.focused(),
                            isolation_new: t.isolation_new.clone(),
                            panes: t.panes.values().map(pane_snapshot).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Menu for a workspace; contents depend on whether it's a repo and whether it's a linked worktree.
    /// Moves a space to another place in the list; the active one stays active.
    pub fn move_space(&mut self, from: usize, to: usize) {
        if from == to || from >= self.spaces.len() || to >= self.spaces.len() {
            return;
        }
        let sp = self.spaces.remove(from);
        self.spaces.insert(to, sp);
        self.active = moved_index(self.active, from, to);
        self.dirty = true;
    }

    /// The space drawn right above (`-1`) or below (`1`) this one, as the sidebar shows them.
    pub(super) fn space_neighbour(&self, index: usize, step: isize) -> Option<usize> {
        let order: Vec<usize> = self.view.hits.sidebar_spaces.iter().map(|(_, i)| *i).collect();
        let at = order.iter().position(|i| *i == index)?;
        order.get(at.checked_add_signed(step)?).copied()
    }

    pub(super) fn focus_pane(&mut self, id: PaneId) {
        if let Some(t) = self.tab_mut() {
            t.layout.focus(id);
        }
    }

    /// New workspace: a folder plus one tab with a shell.
    pub fn open_space(&mut self, cwd: PathBuf) {
        self.learn_repo(&cwd);
        if let Some(pane) = self.spawn_pane(cwd.clone(), shell_argv(), crate::policy::Isolation::default()) {
            let layout = Layout::new(pane.id);
            let mut panes = HashMap::new();
            panes.insert(pane.id, pane);
            self.spaces.push(Space {
                name: None,
                cwd,
                tabs: vec![Tab { name: None, layout, panes, isolation_new: None }],
                active_tab: 0,
                isolation_new: None,
            });
            self.active = self.spaces.len() - 1;
            self.dirty = true;
        }
    }

    pub(super) fn close_space(&mut self, i: usize) {
        if i >= self.spaces.len() {
            return;
        }
        self.spaces.remove(i);
        // Closed to the left of the active one: it shifted by one, it didn't stay put.
        if i < self.active {
            self.active -= 1;
        }
        // No workspaces left: detach the client; the next one brings its own folder (see `daemon`).
        if self.spaces.is_empty() {
            self.active = 0;
            self.quit = Some("the last space closed; detached");
        } else {
            self.active = self.active.min(self.spaces.len() - 1);
        }
    }

    pub(super) fn close_tab(&mut self, si: usize, i: usize) {
        let Some(sp) = self.spaces.get_mut(si) else { return };
        if i >= sp.tabs.len() {
            return;
        }
        if sp.tabs.len() == 1 {
            self.close_space(si);
            return;
        }
        sp.tabs.remove(i);
        if i < sp.active_tab {
            sp.active_tab -= 1;
        }
        sp.active_tab = sp.active_tab.min(sp.tabs.len() - 1);
    }
}

/// What a snapshot keeps of a pane.
fn pane_snapshot(p: &Pane) -> PaneSnapshot {
    PaneSnapshot {
        id: p.id,
        cwd: p.cwd.clone(),
        argv: p.argv.clone(),
        label: p.label.clone(),
        agent: p.agent_session.as_ref().map_or_else(crate::session::default_agent, |s| s.0.clone()),
        agent_session: p.agent_session.as_ref().map(|s| s.1.clone()),
        title: p.term.clean_title().map(str::to_string),
        cols: p.pty.cols,
        rows: p.pty.rows,
        isolation: p.isolation.clone(),
    }
}

/// Where the item at `i` ends up after the one at `from` moves to `to`.
fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

#[cfg(test)]
mod move_tests {
    #[test]
    fn every_other_space_keeps_its_identity() {
        for (from, to) in [(0, 3), (3, 0), (1, 2), (2, 1)] {
            let mut v = vec!['a', 'b', 'c', 'd'];
            let before = v.clone();
            let x = v.remove(from);
            v.insert(to, x);
            for i in 0..4 {
                assert_eq!(v[super::moved_index(i, from, to)], before[i], "{from}->{to}: item {i}");
            }
        }
    }
}
