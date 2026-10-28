use super::*;
use crate::mouse::Action;

/// What `shift`, `option` and an arrow did with a key: moved to another area, did something inside this
/// one, or does not want it - only then may the area itself have the bare arrow.
enum Step {
    Go(Focus),
    Took,
    Pass,
}

impl App {
    /// The selection is dragged past the pane's edge: the pane scrolls on its own.
    pub fn autoscroll_selection(&mut self) -> bool {
        let dir = self.router.autoscroll;
        if dir == 0 {
            return false;
        }
        let Some(sel) = self.selection else { return false };
        let step = 3isize * dir as isize;
        match self.pane_mut(sel.pane) {
            Some(p) => p.term.scroll_by(-step),
            None => false,
        }
    }

    /// `option` and a letter naming an action: the menu's action, on the focused pane.
    fn act(&mut self, a: KeyAction) {
        let ctx = self.tab().map_or(MenuCtx::Global, |t| MenuCtx::Pane(t.layout.focused()));
        self.overlay = self.run_action(a.into(), &ctx);
    }

    pub fn key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if self.swallow_reply(k) {
            return;
        }
        // Taken before the areas, so the search, the tree and the sidebar never eat either half.
        let armed = std::mem::take(&mut self.prefix);
        self.dirty |= armed;
        if let Some(p) = self.cfg.prefix.letter()
            && !self.overlay.is_open()
        {
            // read off the key, so Control + B arms it on a Russian layout too
            let again = matches!(k.code, KeyCode::Char(c) if crate::keys::latin(c) == p) && k.modifiers == KeyModifiers::CONTROL;
            if armed && again {
                // pressed twice, the chord belongs to the pane
                if let Some(pane) = self.tab_mut().and_then(Tab::focused_mut) {
                    pane.pty.write(&[p as u8 - b'a' + 1]);
                    let id = pane.id;
                    self.owner_typed(id);
                }
                return;
            }
            if armed && self.shortcut(k) {
                return;
            }
            if again {
                self.prefix = true;
                self.dirty = true;
                return;
            }
        }
        // after the prefix the tree takes its own `option` letters, shift or not; a pane gets the plain key
        let k = match k.code {
            KeyCode::Char(c) if armed && self.focus == Focus::Tree => KeyEvent::new(KeyCode::Char(crate::keys::latin(c)), KeyModifiers::ALT),
            _ => k,
        };
        // Esc leaves a zoomed section first, then closes one clicked last; otherwise it is the pane's
        if k.code == KeyCode::Esc && self.run.is_some() && !self.overlay.is_open() && self.search.is_none() && (self.run_zoom || self.run_keys) {
            if !std::mem::take(&mut self.run_zoom) {
                self.run = None;
                self.run_keys = false;
            }
            self.dirty = true;
            return;
        }
        // shift, option and an arrow move the keys from one area to the next. The modifier is
        // needed only to leave a pane, where the shell owns the bare arrows; every other area
        // uses them as they are. Taken whatever the direction, so that a step at an edge stops
        // rather than falling through to the shell and doing something else entirely.
        // Not control: macOS keeps control with the arrows for Mission Control, and they never arrive.
        // Not option alone: that is the shell's word motion, and the editors' in every pane.
        // With the modifier held an arrow only ever moves between areas. Where there is no area
        // that way it does nothing: falling through to the area's own arrow is what made the
        // tree impossible to leave, since there the left arrow climbs a directory.
        let arrow = matches!(k.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down);
        let chord = if arrow { KeyModifiers::ALT | KeyModifiers::SHIFT } else { KeyModifiers::ALT };
        if k.modifiers == chord && !self.overlay.is_open() && self.shortcut(k) {
            return;
        }
        // Cleared on the first key, not a specific one, or the toast would outlive the notice.
        if self.notice.take().is_some() | self.toast.take().is_some() {
            self.dirty = true;
        }
        // Search takes keys only while its pane is focused; clicking elsewhere types there instead.
        let focused = self.tab().map(|t| t.layout.focused());
        if self.search.as_ref().is_some_and(|(id, _)| Some(*id) == focused) && !self.overlay.is_open() {
            let outcome = self.search.as_mut().map(|(_, s)| s.key(k)).unwrap_or(crate::search::SearchOutcome::None);
            match outcome {
                crate::search::SearchOutcome::Changed => self.search_refresh(true),
                crate::search::SearchOutcome::Close => self.search = None,
                crate::search::SearchOutcome::None => {}
            }
            self.dirty = true;
            return;
        }
        // `tree_rect` confirms the tree is actually drawn; in a narrow window it isn't, and keys must not go to it.
        // the sidebar and the tab bar use bare arrows: nothing in them competes for the keys
        if matches!(self.focus, Focus::Sidebar | Focus::Tabs) && !self.overlay.is_open() {
            self.area_key(k);
            return;
        }
        if self.focus == Focus::Tree
            && self.view.tree_rect.is_some()
            && !self.overlay.is_open()
            && let Some(tree) = self.tree.as_mut()
        {
            let outcome = tree.key(&self.view.tree, k);
            self.tree_outcome(outcome);
            self.dirty = true;
            return;
        }
        if self.overlay.is_open() {
            self.overlay_key(k);
            return;
        }

        // Backspace on a selection at the cursor sends one erase per character (the shell
        // doesn't know about selections). ceiling: elsewhere, a selection gets a plain Backspace.
        if k.code == KeyCode::Backspace
            && let Some(sel) = self.selection.filter(|s| Some(s.pane) == focused)
            && let Some(n) = self.pane_screen(sel.pane).and_then(|scr| {
                let ((_, _), (er, ec)) = sel.ordered();
                let (cr, cc) = scr.cursor_position();
                (er == cr && ec + 1 >= cc).then(|| sel.text(scr).chars().filter(|c| *c != '\n').count())
            })
        {
            self.selection = None;
            self.router.has_selection = false;
            if let Some(p) = self.tab_mut().and_then(Tab::focused_mut) {
                p.pty.write(&vec![0x7f; n]);
                let id = p.id;
                self.owner_typed(id);
            }
            self.dirty = true;
            return;
        }
        if let Some(bytes) = crate::keys::encode_key(k).or_else(|| crate::keys::encode_key_fallback(k))
            && let Some(p) = self.tab_mut().and_then(Tab::focused_mut) {
                p.pty.write(&crate::keys::cursor_keys(bytes, p.term.screen().app_cursor()));
                let id = p.id;
                self.owner_typed(id);
            }
    }

    /// A terminal reply (`ESC ] ... ESC \\`) arrives from crossterm as ordinary keys;
    /// swallowed and logged instead of leaking into the pane as garbage.
    /// ceiling: 300ms timeout for the end, which also eats a real Alt+] typed meanwhile.
    fn swallow_reply(&mut self, k: KeyEvent) -> bool {
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some((until, text)) = self.reply.as_mut() {
            let end = (alt && k.code == KeyCode::Char('\\')) || (ctrl && k.code == KeyCode::Char('g'));
            let late = Instant::now() > *until;
            if let KeyCode::Char(c) = k.code && !end && !late {
                text.push(c);
            }
            if end || late {
                let text = std::mem::take(text);
                self.reply = None;
                // The answer to the client's graphics probe (`query_graphics`), after it stopped waiting.
                if text == "Gi=31;OK" && !self.graphics {
                    self.graphics = true;
                    self.dirty = true;
                    crate::log::info("graphics: the terminal answered late, images on");
                }
                crate::log::warn(&format!("terminal reply swallowed: {}", swallowed(text)));
            }
            if !late {
                return true;
            }
        }
        if alt && matches!(k.code, KeyCode::Char(']') | KeyCode::Char('P') | KeyCode::Char('_')) {
            self.reply = Some((Instant::now() + Duration::from_millis(300), String::new()));
            return true;
        }
        false
    }

    fn prompt_outcome(&mut self, p: crate::menu::Prompt, ask: Ask, out: crate::menu::PromptOutcome) -> Overlay {
        use crate::menu::PromptOutcome;
        match out {
            // A refused name keeps the prompt open, with the notice saying why.
            PromptOutcome::Submitted(v) => match self.submitted(v, &ask) {
                Some(next) => next,
                None => Overlay::Prompt(p, ask),
            },
            PromptOutcome::Cancelled => self.prompt_cancelled(ask),
            PromptOutcome::None => Overlay::Prompt(p, ask),
        }
    }

    fn overlay_key(&mut self, k: KeyEvent) {
        use crate::menu::Outcome;
        self.dirty = true;
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        self.overlay = match overlay {
            Overlay::Menu(mut m, ctx) => match m.key(k) {
                Outcome::Chosen(a) => self.run_action(a, &ctx),
                Outcome::Cancelled => Overlay::None,
                Outcome::None => Overlay::Menu(m, ctx),
            },
            Overlay::Prompt(mut p, ask) => {
                let out = p.key(k);
                self.prompt_outcome(p, ask, out)
            }
            Overlay::Find(mut f) => {
                let out = f.key(k);
                self.find_outcome(f, out)
            }
            Overlay::Confirm(mut c, mut what) => match c.key(k) {
                crate::menu::ConfirmOutcome::Yes => {
                    let back = topology_back(&mut what);
                    self.perform(what);
                    back.unwrap_or_else(|| self.answered())
                }
                crate::menu::ConfirmOutcome::Other => {
                    self.perform_other(what);
                    self.answered()
                }
                crate::menu::ConfirmOutcome::No => {
                    self.declined(&what);
                    topology_back(&mut what).unwrap_or_else(|| self.answered())
                }
                crate::menu::ConfirmOutcome::None => Overlay::Confirm(c, what),
            },
            Overlay::Picker(mut p, pick) => match p.key(k) {
                crate::menu::PickerOutcome::Chosen(id) => self.picked(id, pick),
                crate::menu::PickerOutcome::Cancelled => match pick {
                    Pick::Topology(.., Some(w)) => Overlay::Messages(*w),
                    _ => Overlay::None,
                },
                crate::menu::PickerOutcome::None => Overlay::Picker(p, pick),
            },
            Overlay::NewSpace(mut w) => match w.key(k) {
                crate::space_picker::Outcome::Open(path) => {
                    self.open_new_space(path);
                    Overlay::None
                }
                crate::space_picker::Outcome::Cancelled => Overlay::None,
                crate::space_picker::Outcome::None => Overlay::NewSpace(w),
            },
            Overlay::Settings(mut s) => {
                s.inside = self.client_inside;
                let outcome = s.key(k, &mut self.cfg);
                self.settings_outcome(s, outcome)
            }
            Overlay::Keys(mut h) => {
                if h.key(k) {
                    Overlay::None
                } else {
                    Overlay::Keys(h)
                }
            }
            Overlay::Sessions(mut w) => {
                if w.key(k, self.screen) {
                    Overlay::None
                } else {
                    Overlay::Sessions(w)
                }
            }
            Overlay::Panel(mut p) => {
                let out = p.key(k, self.screen, self.plugins.slots.panel(&p.src, &p.id));
                self.panel_outcome(p, out)
            }
            Overlay::Plugins(mut w) => {
                let out = w.key(k, self.screen, &self.view.dialog.panel);
                self.plugins_outcome(w, out)
            }
            Overlay::Servers(mut w) => {
                let out = w.key(k, self.screen);
                self.servers_outcome(w, out)
            }
            Overlay::Messages(mut w) => {
                let out = w.key(k, &self.mail_view());
                self.messages_outcome(w, out)
            }
            Overlay::Review(mut w) => {
                let out = w.key(k);
                self.review_outcome(w, out)
            }
            Overlay::Isolation(mut w) => {
                let out = w.key(k);
                self.isolation_outcome(w, out)
            }
            Overlay::Accounts(mut w) => {
                let out = w.key(k, &crate::policy::current().accounts);
                self.accounts_outcome(w, out)
            }
            Overlay::None => Overlay::None,
        };
    }

    /// The second half of a shortcut, after `option` or after the prefix: an arrow between areas,
    /// a letter from `GLOBAL`, or a tab's digit. The letter is taken from the key rather than
    /// from what the layout printed, so the same finger works on any keyboard.
    fn shortcut(&mut self, k: KeyEvent) -> bool {
        let c = match k.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => return self.step_focus(k.code),
            KeyCode::Char(c) => crate::keys::latin(c),
            _ => return false,
        };
        if let Some(g) = global_of(c) {
            self.notice.take();
            self.toast.take();
            match g.bind {
                Bind::Do(a) => self.act(a),
                Bind::Open(n) => self.fkey(n),
            }
            self.dirty = true;
            return true;
        }
        if let d @ '1'..='9' = c {
            self.select_tab(d as usize - '1' as usize);
            return true;
        }
        false
    }

    /// One step between areas, in the direction pressed. At an edge nothing happens, so that
    /// "left" never comes to mean "the far right".
    fn step_focus(&mut self, code: KeyCode) -> bool {
        use Step::*;
        let panes = self.pane_area();
        let tree_open = self.tree.is_some() && self.view.tree_rect.is_some();
        let preview_on = self.view.preview_rect.is_some();
        let step = match (self.focus, code) {
            // Sideways inside the panes first; the sidebar and the tree are one step further,
            // reached only from the pane that has nothing beside it.
            (Focus::Panes, KeyCode::Left) => {
                if self.nav(panes, Nav::Left) {
                    Took
                } else if self.sidebar {
                    Go(Focus::Sidebar)
                } else {
                    Took
                }
            }
            (Focus::Panes, KeyCode::Right) => {
                if self.nav(panes, Nav::Right) {
                    Took
                } else if tree_open {
                    Go(Focus::Tree)
                } else {
                    Took
                }
            }
            // the tab bar sits above the topmost pane, so it is one more step up from there
            (Focus::Panes, KeyCode::Up) => {
                if self.nav(panes, Nav::Up) { Took } else { Go(Focus::Tabs) }
            }
            (Focus::Panes, KeyCode::Down) => {
                self.nav(panes, Nav::Down);
                Took
            }
            // The sidebar spans the whole height and the tab bar sits above the panes, so from
            // each of them there is exactly one way on - and with the modifier held, an arrow
            // that leads nowhere does nothing rather than acting as the area's own.
            (Focus::Sidebar, KeyCode::Right) => Go(Focus::Panes),
            (Focus::Sidebar, _) => Took,
            (Focus::Tabs, KeyCode::Down) => Go(Focus::Panes),
            (Focus::Tabs, KeyCode::Left) => {
                if self.sidebar { Go(Focus::Sidebar) } else { Took }
            }
            (Focus::Tabs, KeyCode::Right) => {
                if tree_open { Go(Focus::Tree) } else { Took }
            }
            (Focus::Tabs, _) => Took,
            // Inside the tree the modifier is only ever for leaving: a bare arrow is the tree's
            // own, and one held with option must never walk the rows or climb a directory.
            (Focus::Tree, KeyCode::Left) => {
                // the preview has its own column left of the rows; from it, one more step leaves
                let in_preview = self.tree.as_ref().is_some_and(crate::tree::Tree::in_preview);
                if preview_on && !in_preview {
                    if let Some(t) = self.tree.as_mut() {
                        t.show_preview();
                    }
                    self.dirty = true;
                    Took
                } else {
                    Go(Focus::Panes)
                }
            }
            (Focus::Tree, KeyCode::Right) => {
                if let Some(t) = self.tree.as_mut().filter(|t| t.in_preview()) {
                    t.show_rows();
                    self.dirty = true;
                }
                Took
            }
            (Focus::Tree, _) => Took,
            _ => Pass,
        };
        match step {
            Go(f) => {
                self.enter(f);
                true
            }
            Took => true,
            Pass => false,
        }
    }

    /// Keys inside the sidebar and the tab bar. `esc` always goes back to the panes, as it does
    /// from the tree, so one key leaves whatever you are in.
    fn area_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        if k.code == KeyCode::Esc {
            self.enter(Focus::Panes);
            return;
        }
        match self.focus {
            Focus::Sidebar => {
                let rows = self.sidebar_rows();
                if rows.is_empty() {
                    return;
                }
                let at = self.sidebar_sel.and_then(|sel| rows.iter().position(|r| *r == sel));
                match k.code {
                    KeyCode::Up => self.sidebar_sel = Some(rows[at.map_or(0, |i| i.saturating_sub(1))]),
                    KeyCode::Down => self.sidebar_sel = Some(rows[at.map_or(0, |i| (i + 1).min(rows.len() - 1))]),
                    KeyCode::Enter => {
                        match self.sidebar_sel {
                            Some(SidebarSel::Pane(id)) => self.reveal_pane(id),
                            Some(SidebarSel::Space(i)) if i < self.spaces.len() => self.active = i,
                            _ => {}
                        }
                        self.enter(Focus::Panes);
                    }
                    _ => {}
                }
            }
            Focus::Tabs => {
                let Some(sp) = self.space_mut() else { return };
                let last = sp.tabs.len().saturating_sub(1);
                match k.code {
                    KeyCode::Left => sp.active_tab = sp.active_tab.saturating_sub(1),
                    KeyCode::Right => sp.active_tab = (sp.active_tab + 1).min(last),
                    KeyCode::Enter => self.enter(Focus::Panes),
                    _ => {}
                }
            }
            Focus::Panes | Focus::Tree => {}
        }
    }

    /// Every sidebar row the keys can sit on, in the order they were drawn. Taken from the same
    /// hit map the mouse uses, so the two can never disagree about what is on screen.
    fn sidebar_rows(&self) -> Vec<SidebarSel> {
        let hits = &self.view.hits;
        let mut rows: Vec<(u16, SidebarSel)> = hits
            .sidebar_spaces
            .iter()
            .map(|(r, i)| (r.y, SidebarSel::Space(*i)))
            .chain(hits.sidebar_rows.iter().map(|(r, id)| (r.y, SidebarSel::Pane(*id))))
            .collect();
        rows.sort_by_key(|(y, _)| *y);
        rows.dedup_by_key(|(y, _)| *y);
        rows.into_iter().map(|(_, sel)| sel).collect()
    }

    /// Brings a pane into view wherever it lives, and gives it the keys.
    fn reveal_pane(&mut self, id: PaneId) {
        for (si, sp) in self.spaces.iter().enumerate() {
            for (ti, t) in sp.tabs.iter().enumerate() {
                if t.panes.contains_key(&id) {
                    self.active = si;
                    self.spaces[si].active_tab = ti;
                    self.spaces[si].tabs[ti].layout.focus(id);
                    return;
                }
            }
        }
    }

    /// Switches to the tab at `i`, if the space has one there.
    fn select_tab(&mut self, i: usize) {
        if let Some(sp) = self.space_mut()
            && i < sp.tabs.len()
        {
            sp.active_tab = i;
            self.dirty = true;
        }
    }

    /// Moves the keys to an area and makes it usable straight away.
    pub(super) fn enter(&mut self, f: Focus) {
        if f == Focus::Sidebar {
            let here = self.tab().map(|t| SidebarSel::Pane(t.layout.focused()));
            let rows = self.sidebar_rows();
            self.sidebar_sel = here
                .filter(|h| rows.contains(h))
                .or_else(|| rows.first().copied());
        }
        self.focus = f;
        self.dirty = true;
    }

    /// The seven windows ⌥ and a letter open, numbered as `keys::GLOBAL` binds them. The third one toggles the search.
    fn fkey(&mut self, n: u8) {
        self.dirty = true;
        match n {
            1 => self.overlay = Overlay::Keys(crate::keyshelp::KeysHelp::new(self.cfg.prefix)),
            2 => {
                let at = self.view.hits.menu_at.or(self.view.hits.sidebar_menu.map(|r| (r.x, r.y))).map(|(x, y)| (x, y + 1)).unwrap_or((1, 1));
                self.overlay = self.global_menu(at);
            }
            3 => {
                if self.search.take().is_none()
                    && let Some(id) = self.tab().map(|t| t.layout.focused())
                {
                    self.search = Some((id, crate::search::Search::new()));
                }
            }
            4 => self.toggle_tree(),
            6 => self.overlay = Overlay::Settings(crate::settings::Settings::new(&self.cfg)),
            7 => self.overlay = self.find_overlay(),
            _ => {}
        }
    }

    /// Type text into the active pane, as if the human had typed it.
    pub(super) fn pane_type(&mut self, text: &str) {
        if let Some(p) = self.tab_mut().and_then(Tab::focused_mut) {
            p.pty.write(text.as_bytes());
        }
    }

    /// Recompute matches; `follow` also scrolls to the current one.
    pub(super) fn search_refresh(&mut self, follow: bool) {
        let Some((id, search)) = self.search.as_mut() else { return };
        let id = *id;
        let Some(p) = self.spaces.get_mut(self.active).and_then(|sp| sp.tab_mut()).and_then(|t| t.panes.get_mut(&id)) else {
            self.search = None;
            return;
        };
        search.run(&mut p.term);
        if follow && let Some(target) = search.scroll_target() {
            p.term.scroll_by(p.term.scroll as isize - target as isize);
        }
        self.search_at = Instant::now();
    }

    /// Recomputes stale matches at most twice a second, without scrolling, so the screen doesn't jitter.
    pub fn tick_search(&mut self) -> bool {
        if self.search.is_none() || self.search_at.elapsed() < Duration::from_millis(500) {
            return false;
        }
        self.search_refresh(false);
        true
    }

    /// Returns whether it moved: at the top pane there is nowhere further up, and the caller
    /// takes that as leaving the panes altogether.
    fn nav(&mut self, area: Rect, dir: Nav) -> bool {
        let moved = self.tab_mut().is_some_and(|t| t.layout.navigate(area, dir));
        if moved {
            self.dirty = true;
        }
        moved
    }

    /// The `[Esc] close` button of the open window, for the windows that draw it from their own frame.
    pub(super) fn close_button(&self, screen: Rect) -> Option<Rect> {
        let frame = match &self.overlay {
            Overlay::Settings(w) => w.rect(screen),
            Overlay::Keys(w) => w.rect(screen),
            Overlay::Find(w) => return Some(w.button(screen)),
            Overlay::NewSpace(w) => w.rect(screen),
            Overlay::Picker(w, _) => w.rect(screen),
            Overlay::Messages(_) => return Some(self.view.dialog.messages.close),
            Overlay::Review(_) => return Some(self.view.dialog.review.close),
            Overlay::Isolation(_) | Overlay::Accounts(_) => return Some(self.view.dialog.isolation.close),
            _ => return None,
        };
        Some(crate::chrome::close_rect(frame))
    }

    /// The pointer shape for what is under (x, y), from the last frame's hit map.
    pub(super) fn pointer_at(&self, screen: Rect, x: u16, y: u16) -> String {
        use crate::pointer::*;
        let at = ratatui::layout::Position::new(x, y);
        let hit = |r: &Rect| r.contains(at);
        let d = &self.view.dialog;
        if let Overlay::Review(w) = &self.overlay
            && (w.dragging() || crate::review::on_divider(&d.review, x, y))
        {
            return EW.into();
        }
        let shape = match &self.overlay {
            Overlay::None => None,
            Overlay::Menu(m, _) => Some(if m.points_at(screen, x, y) { POINTER } else { DEFAULT }),
            _ => {
                let prompt = matches!(&self.overlay, Overlay::Prompt(p, _) if p.buttons(screen).iter().any(hit));
                let row = prompt
                    || d.rects.iter().any(hit)
                    || d.rows.iter().any(|(r, _)| hit(r))
                    || d.panel.rows.iter().any(|(r, _)| hit(r))
                    || hit(&d.panel.close)
                    || d.messages.hits.iter().any(|(r, _)| hit(r))
                    || d.review.hits.iter().any(|(r, _)| hit(r))
                    || d.isolation.hits.iter().any(|(r, _)| hit(r))
                    || d.find.hits.iter().any(|(r, _)| hit(r))
                    || self.close_button(screen).is_some_and(|r| hit(&r));
                Some(if row { POINTER } else { DEFAULT })
            }
        };
        if let Some(s) = shape {
            return s.into();
        }
        let h = &self.view.hits;
        // The notice card lies over a pane: all of it is a button, "… more" and × with it.
        if [h.notice, h.notice_close].iter().flatten().any(hit) {
            return POINTER.into();
        }
        // the same slack a drag gets
        let near = |r: &Rect| crate::mouse::near(*r, x, y);
        if [h.tree_divider, h.preview_divider, h.sidebar_divider].iter().flatten().any(near) {
            return EW.into();
        }
        if self.run.is_some()
            && let Some(s) = self.view.run.pointer(x, y)
        {
            return s.into();
        }
        if self.view.preview_rect.as_ref().is_some_and(hit) {
            return TEXT.into();
        }
        if self.view.tree_frame.or(self.view.tree_rect).as_ref().is_some_and(hit) {
            return if self.view.tree.clickable(x, y) { POINTER } else { DEFAULT }.into();
        }
        if let Some(b) = h.borders.iter().find(|b| near(&b.rect)) {
            return if b.dir == crate::layout::Dir::Horizontal { EW } else { NS }.into();
        }
        let clickable = h.tabs.iter().any(|(r, _)| hit(r))
            || h.new_tab.as_ref().is_some_and(hit)
            || h.segments.iter().any(|(r, _)| hit(r))
            || h.sidebar_rows.iter().any(|(r, _)| hit(r))
            || h.sidebar_spaces.iter().any(|(r, _)| hit(r))
            || h.sidebar_group_toggles.iter().any(|(r, _)| hit(r))
            || [h.sidebar_new, h.sidebar_menu, h.sidebar_toggle].iter().flatten().any(hit)
            || h.sidebar_subagents.iter().any(|(r, _)| hit(r))
            || h.sidebar_subagent_rows.iter().any(|(r, _, _)| hit(r));
        if clickable {
            return POINTER.into();
        }
        if let Some(p) = h.panes.iter().find(|p| hit(&p.inner)) {
            let asked = self.tab().and_then(|t| t.panes.get(&p.id)).and_then(|p| p.term.pointer.clone());
            return asked.unwrap_or_else(|| TEXT.into());
        }
        DEFAULT.into()
    }

    /// Sends the pointer shape for (x, y) when it changed; not inside tmux, which does not pass OSC 22 on.
    pub(super) fn point(&mut self, screen: Rect, x: u16, y: u16) {
        if self.caps.mux {
            return;
        }
        let shape = self.pointer_at(screen, x, y);
        if shape != self.pointer {
            self.to_client.extend(crate::pointer::osc(&shape));
            self.pointer = shape;
        }
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect) {
        use crossterm::event::{MouseButton, MouseEventKind};

        self.point(screen, ev.column, ev.row);
        let over = self.view.hits.segments.iter().any(|(r, h)| *h == crate::mouse::SegmentHit::Battery && inside(*r, ev.column, ev.row));
        if over || self.battery_hover.is_some() {
            self.battery_hover = over.then(Instant::now);
            self.dirty = true;
        }

        // a click between the halves of the chord cancels it, or the next letter fires as a command
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            self.dirty |= std::mem::take(&mut self.prefix);
        }
        if self.overlay.is_open() {
            self.overlay_mouse(ev, screen);
            return;
        }
        // The run section sits over its pane's right part: what lands there, on its seam, or in a drag it began is its own.
        if let Some(run) = self.run.as_mut() {
            if run.claims(&self.view.run, &ev) {
                match run.mouse(&mut self.view.run, ev) {
                    crate::run::Asked::Close => {
                        self.run = None;
                        self.run_zoom = false;
                    }
                    crate::run::Asked::Zoom => self.run_zoom = !self.run_zoom,
                    crate::run::Asked::Width(w) => {
                        self.run_widths.insert(run.pane, w);
                    }
                    crate::run::Asked::Nothing => {}
                }
                self.run_keys = self.run.is_some();
                self.dirty = true;
                return;
            }
            if matches!(ev.kind, MouseEventKind::Down(_)) {
                self.run_keys = false;
                run.clear_selection();
            }
        }
        // A click inside the tree focuses it, outside clears focus; the tree's edge and an active drag go to the router instead.
        let near_edge = |d: Option<Rect>| d.is_some_and(|d| ev.column + 1 >= d.x && ev.column <= d.x + 1);
        let on_divider = near_edge(self.view.hits.tree_divider) || near_edge(self.view.hits.preview_divider);
        if !self.router.dragging() && !on_divider
            && ev.kind != MouseEventKind::Down(MouseButton::Right)
            && let (Some(r), Some(tree)) = (self.view.tree_frame.or(self.view.tree_rect), self.tree.as_mut())
        {
            let at = ratatui::layout::Position::new(ev.column, ev.row);
            // The frame, not just the halves: □ and × sit on its title row. A selection being
            // dragged keeps the events even when the pointer wanders out of it.
            let inside = r.contains(at)
                || self.view.preview_rect.is_some_and(|p| p.contains(at))
                || (self.view.tree.selecting() && matches!(ev.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)));
            if inside {
                if matches!(ev.kind, MouseEventKind::Down(_)) {
                    self.focus = Focus::Tree;
                }
                let outcome = tree.mouse(&mut self.view.tree, ev, r, self.view.preview_rect);
                self.tree_outcome(outcome);
                self.dirty = true;
                return;
            } else if matches!(ev.kind, MouseEventKind::Down(_)) {
                self.view.tree.clear_selection();
                self.focus = Focus::Panes;
            }
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Right) {
            self.open_context_menu(ev.column, ev.row);
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some((_, id)) = self.view.sidebar_mail.iter().find(|(r, _)| inside(*r, ev.column, ev.row))
        {
            self.overlay = Overlay::Messages(crate::messages::Messages::at(id.clone()));
            self.dirty = true;
            return;
        }
        let action = self.router.route(ev, &self.view.hits);
        if std::mem::take(&mut self.router.dropped) && self.selection.take().is_some() {
            self.dirty = true;
        }
        match action {
            Action::None => {}
            Action::SelectTab(i) => {
                if let Some(sp) = self.space_mut()
                    && i < sp.tabs.len() {
                        sp.active_tab = i;
                        self.dirty = true;
                    }
            }
            Action::NewTab => self.new_tab(),
            // As its menu item would: the same checks, the same notice when the plugin is gone.
            Action::Segment(crate::mouse::SegmentHit::Battery) => {
                use crate::config::Battery;
                self.cfg.battery = if self.cfg.battery == Battery::Percent { Battery::Time } else { Battery::Percent };
                self.save_cfg();
                self.dirty = true;
            }
            Action::Segment(crate::mouse::SegmentHit::Clock) => {
                use crate::config::Clock;
                self.cfg.clock = if self.cfg.clock == Clock::Time { Clock::Date } else { Clock::Time };
                self.save_cfg();
                self.tick_clock();
                self.dirty = true;
            }
            Action::Segment(crate::mouse::SegmentHit::Command(plugin, id)) => {
                if let Some(n) = crate::plugins::menu_number(&plugin, &id) {
                    self.overlay = self.run_action(super::commands::Action::Plugin(n), &MenuCtx::Global);
                }
                self.dirty = true;
            }
            // Sidebar rows name panes in any space and tab, not just the open one.
            Action::FocusPane(id) => {
                if let Some((si, ti)) = self.locate(id) {
                    self.go_to(si, ti, id);
                }
            }
            Action::BeginDrag => {}
            Action::Drag { path, ratio } => {
                if let Some(t) = self.tab_mut() {
                    t.layout.set_ratio(&path, ratio);
                    self.dirty = true;
                }
            }
            Action::SelectSpace(i) => {
                if i < self.spaces.len() {
                    self.active = i;
                    self.dirty = true;
                }
            }
            Action::MoveSpace { from, to } => self.move_space(from, to),
            Action::OpenRun(pane, id) => self.open_run(pane, id),
            Action::ToggleSubagents(id) => {
                if !self.subagents_open.remove(&id) {
                    self.subagents_open.insert(id);
                }
                self.dirty = true;
            }
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::NoticeClose => {
                self.notice = None;
                self.toast = None;
                self.dirty = true;
            }
            Action::NoticeOpen => self.overlay = self.notice_window(),
            Action::NewSpace => {
                self.overlay = self.new_space_overlay();
                self.dirty = true;
            }
            Action::OpenMenu(x, y) => {
                // one row below the button, or the menu would cover the very thing that opened it
                self.overlay = self.global_menu((x, y + 1));
                self.dirty = true;
            }
            Action::SidebarWidth(w) => {
                self.cfg.sidebar_width = w.clamp(crate::config::SIDEBAR_WIDTH.0, crate::config::SIDEBAR_WIDTH.1);
                self.save_cfg_soon();
            }
            Action::TreeEdge(x) => {
                // The frame's own border: the tree half grows toward it, the preview keeps its width.
                if let Some(r) = self.view.tree_rect {
                    self.cfg.tree_width = r.right().saturating_sub(x + 2).clamp(crate::config::TREE_WIDTH.0, crate::config::TREE_WIDTH.1);
                    self.save_cfg_soon();
                }
            }
            Action::PreviewEdge(x) => {
                // The seam inside the frame: the halves trade width, the pair keeps its own.
                // In full screen the preview takes whatever is left, so the width it keeps for
                // the ordinary layout is not spent on it.
                if let Some(r) = self.view.tree_rect {
                    let border = r.x.saturating_sub(2);
                    let total = self.cfg.tree_width + self.cfg.preview_width;
                    let tree = x.saturating_sub(border + 2).clamp(crate::config::TREE_WIDTH.0, crate::config::TREE_WIDTH.1);
                    self.cfg.tree_width = tree;
                    if !self.tree_zoom {
                        self.cfg.preview_width = total.saturating_sub(tree).clamp(crate::config::PREVIEW_WIDTH.0, crate::config::PREVIEW_WIDTH.1);
                    }
                    self.save_cfg_soon();
                }
            }
            Action::Scroll { id, lines } => {
                if let Some(p) = self.tab_mut().and_then(|t| t.panes.get_mut(&id))
                    && p.term.scroll_by(lines) {
                        self.dirty = true;
                    }
            }
            Action::ToChild { id, bytes } => {
                // The program grabbed the mouse, but focus is still ours, or a pane running claude couldn't be reached with a click.
                if matches!(ev.kind, MouseEventKind::Down(_)) {
                    self.focus_pane(id);
                    self.dirty = true;
                }
                if let Some(p) = self.tab_mut().and_then(|t| t.panes.get_mut(&id)) {
                    p.pty.write(&bytes);
                }
            }
            Action::SelectStart { id, row, col } => {
                self.word_anchor = None;
                self.line_anchor = None;
                self.focus_pane(id);
                self.selection = Some(crate::select::Selection { pane: id, start: (row, col), end: (row, col) });
                self.dirty = true;
            }
            Action::SelectExtend { id, row, col } => {
                if let Some(sel) = self.selection.as_mut().filter(|s| s.pane == id) {
                    sel.end = (row, col);
                    self.dirty = true;
                }
            }
            Action::SelectWord { id, row, col } => {
                self.focus_pane(id);
                let word = self
                    .tab()
                    .and_then(|t| t.panes.get(&id))
                    .and_then(|p| crate::select::word_at(p.term.screen(), row, col));
                if let Some((start, end)) = word {
                    let sel = crate::select::Selection { pane: id, start, end };
                    self.copy_selection(&sel);
                    self.selection = Some(sel);
                    self.word_anchor = Some((start, end));
                    self.dirty = true;
                } else {
            // Missing the word means no selection, and the router shouldn't think otherwise.
                    self.router.has_selection = false;
                }
            }
            // A drag after a double-click grows whole words from the anchor.
            Action::SelectExtendWord { id, row, col } => {
                let word = self
                    .tab()
                    .and_then(|t| t.panes.get(&id))
                    .and_then(|p| crate::select::word_at(p.term.screen(), row, col));
                if let (Some((ws, we)), Some((a_s, a_e))) = (word, self.word_anchor)
                    && let Some(sel) = self.selection.as_mut().filter(|s| s.pane == id) {
                        sel.start = a_s.min(ws);
                        sel.end = a_e.max(we);
                        self.dirty = true;
                    }
            }
            Action::SelectLine { id, row } => {
                self.focus_pane(id);
                let line = self
                    .tab()
                    .and_then(|t| t.panes.get(&id))
                    .map(|p| crate::select::line_at(p.term.screen(), row));
                if let Some((start, end)) = line {
                    let sel = crate::select::Selection { pane: id, start, end };
                    self.copy_selection(&sel);
                    self.selection = Some(sel);
                    self.word_anchor = None;
                    self.line_anchor = Some(row);
                    self.dirty = true;
                }
            }
            // Ctrl+click: a link or path under the cursor opens in the system; nothing found behaves like a plain click.
            Action::OpenLink { id, row, col } => {
                let found = self
                    .tab()
                    .and_then(|t| t.panes.get(&id))
                    .and_then(|p| crate::links::link_at(p.term.screen(), row, col).map(|l| (l, p.cwd.clone())));
                match found {
                    Some((link, cwd)) => match crate::links::open(&link, &cwd) {
                        Ok(()) => self.toast = Some("opened".to_string()),
                        Err(e) => self.notice = Some(e),
                    },
                    None => {
                        self.word_anchor = None;
                        self.line_anchor = None;
                        self.focus_pane(id);
                        self.selection = Some(crate::select::Selection { pane: id, start: (row, col), end: (row, col) });
                    }
                }
                self.dirty = true;
            }
            Action::SelectExtendLine { id, row } => {
                let Some(anchor) = self.line_anchor else { return };
                let (lo, hi) = (anchor.min(row), anchor.max(row));
                let end = self.tab().and_then(|t| t.panes.get(&id)).map(|p| crate::select::line_at(p.term.screen(), hi).1);
                if let (Some(end), Some(sel)) = (end, self.selection.as_mut().filter(|s| s.pane == id)) {
                    sel.start = (lo, 0);
                    sel.end = end;
                    self.dirty = true;
                }
            }
            // Copies immediately on release; the highlight stays until the next click so it's visible what was copied.
            Action::SelectEnd => {
                // A single click is a one-cell "selection": not copied, or it would overwrite the clipboard,
                // and not kept, or the next click elsewhere would be spent on dropping it.
                match self.selection.filter(|s| s.start != s.end) {
                    Some(sel) => self.copy_selection(&sel),
                    None => {
                        self.selection = None;
                        self.router.has_selection = false;
                    }
                }
            }
            Action::ToggleGroup(i) => {
                let cwd = self.spaces.get(i).map(|s| s.cwd.clone());
                if let Some(key) = cwd.and_then(|c| self.group_key_of(&c)) {
                    if !self.collapsed_groups.remove(&key) {
                        self.collapsed_groups.insert(key);
                    }
                    self.dirty = true;
                }
            }
        }
    }

    pub(super) fn copy_selection(&mut self, sel: &crate::select::Selection) {
        let text = self
            .pane_screen(sel.pane)
            .map(|scr| sel.text(scr))
            .unwrap_or_default();
        if !text.is_empty() {
            self.to_client.extend(crate::select::copy(&text));
            self.toast = Some(format!("copied: {} chars", text.chars().count()));
        }
    }

    /// What shows once a dialog is answered: the Servers or Plugins window it was asked from.
    fn answered(&mut self) -> Overlay {
        if let Some(w) = self.servers_back.take() {
            return Overlay::Servers(w);
        }
        match self.plugins_back.take() {
            Some(name) => self.plugins_overlay_at(&name),
            None => Overlay::None,
        }
    }

    fn overlay_mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect) {
        use crate::menu::Outcome;
        let prior = std::mem::replace(&mut self.dirty, true);
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        self.overlay = match overlay {
            Overlay::Menu(mut m, ctx) => match m.mouse(ev, screen) {
                Outcome::Chosen(a) => self.run_action(a, &ctx),
                Outcome::Cancelled => Overlay::None,
                Outcome::None => Overlay::Menu(m, ctx),
            },
            // The input field isn't mouse-driven; only its buttons and a click outside are.
            Overlay::Prompt(p, ask) => {
                let out = p.mouse(ev, screen);
                self.prompt_outcome(p, ask, out)
            }
            Overlay::Confirm(mut c, mut what) => match c.mouse(ev, screen, &self.view.dialog.rects) {
                crate::menu::ConfirmOutcome::Yes => {
                    let back = topology_back(&mut what);
                    self.perform(what);
                    back.unwrap_or_else(|| self.answered())
                }
                crate::menu::ConfirmOutcome::Other => {
                    self.perform_other(what);
                    self.answered()
                }
                crate::menu::ConfirmOutcome::No => {
                    self.declined(&what);
                    topology_back(&mut what).unwrap_or_else(|| self.answered())
                }
                crate::menu::ConfirmOutcome::None => Overlay::Confirm(c, what),
            },
            Overlay::Picker(mut p, pick) => match p.mouse(ev, screen, &self.view.dialog.rects) {
                crate::menu::PickerOutcome::Chosen(id) => self.picked(id, pick),
                crate::menu::PickerOutcome::Cancelled => match pick {
                    Pick::Topology(.., Some(w)) => Overlay::Messages(*w),
                    _ => Overlay::None,
                },
                crate::menu::PickerOutcome::None => Overlay::Picker(p, pick),
            },
            Overlay::NewSpace(mut w) => match w.mouse(ev, screen, &self.view.dialog.rects) {
                crate::space_picker::Outcome::Open(path) => {
                    self.open_new_space(path);
                    Overlay::None
                }
                crate::space_picker::Outcome::Cancelled => Overlay::None,
                crate::space_picker::Outcome::None => Overlay::NewSpace(w),
            },
            Overlay::Settings(mut s) => {
                s.inside = self.client_inside;
                let outcome = s.mouse(ev, screen, &mut self.cfg, &self.view.dialog.rows);
                self.settings_outcome(s, outcome)
            }
            Overlay::Keys(mut h) => {
                if h.mouse(ev, screen) {
                    Overlay::None
                } else {
                    Overlay::Keys(h)
                }
            }
            Overlay::Sessions(mut w) => {
                if w.mouse(ev, screen, &self.view.dialog.panel) {
                    Overlay::None
                } else {
                    Overlay::Sessions(w)
                }
            }
            Overlay::Panel(mut p) => {
                let out = p.mouse(ev, screen, self.plugins.slots.panel(&p.src, &p.id), &self.view.dialog.panel);
                self.panel_outcome(p, out)
            }
            Overlay::Plugins(mut w) => {
                let out = w.mouse(ev, screen, &self.view.dialog.panel);
                self.plugins_outcome(w, out)
            }
            Overlay::Servers(mut w) => {
                let out = w.mouse(ev, screen, &self.view.dialog.panel);
                self.servers_outcome(w, out)
            }
            Overlay::Messages(mut w) => {
                let out = w.mouse(ev, &self.view.dialog.messages);
                self.messages_outcome(w, out)
            }
            Overlay::Review(mut w) => {
                let out = w.mouse(ev, &self.view.dialog.review);
                self.review_outcome(w, out)
            }
            Overlay::Isolation(mut w) => {
                let out = w.mouse(ev, &self.view.dialog.isolation);
                self.isolation_outcome(w, out)
            }
            Overlay::Accounts(mut w) => {
                let out = w.mouse(ev, &self.view.dialog.isolation);
                self.accounts_outcome(w, out)
            }
            Overlay::Find(mut f) => {
                let chosen = f.chosen();
                let out = f.mouse(ev, screen, &self.view.dialog.find);
                // A motion that keeps the same row chosen draws nothing new.
                if ev.kind == crossterm::event::MouseEventKind::Moved && f.chosen() == chosen {
                    self.dirty = prior;
                    Overlay::Find(f)
                } else {
                    self.find_outcome(f, out)
                }
            }
            Overlay::None => Overlay::None,
        };
    }

    /// Right click: opens the menu for what was pointed at - a space, a sidebar or pane row,
    /// a tab, pane content, or the ≡ button, which alone reopens the global menu here. Anything
    /// else (a subagent row, a plain button, a divider, empty space, borders) opens nothing.
    pub(super) fn open_context_menu(&mut self, x: u16, y: u16) {
        let at = (x, y);
        // The file tree first: it sits on top of the pane area's right edge.
        let on_tree = self.view.tree_frame.or(self.view.tree_rect).is_some_and(|r| inside(r, x, y))
            || self.view.preview_rect.is_some_and(|r| inside(r, x, y));
        if on_tree && let Some(tree) = self.tree.as_mut() {
            tree.select_at(&self.view.tree, x, y);
            let (path, is_dir) = tree.row_at(&self.view.tree, x, y).unwrap_or_else(|| (tree.selected_dir(), true));
            self.focus = Focus::Tree;
            self.overlay = self.tree_menu(path, is_dir, at);
            self.dirty = true;
            return;
        }
        let hits = &self.view.hits;

        if let Some((_, i)) = hits.sidebar_spaces.iter().find(|(r, _)| inside(*r, x, y)) {
            let i = *i;
            if let Some(o) = self.space_menu(i, at) {
                self.overlay = o;
                self.dirty = true;
            }
            return;
        }
        // A pane row under a space, or a "needs you" row: both live in `sidebar_rows`.
        if let Some((_, id)) = hits.sidebar_rows.iter().find(|(r, _)| inside(*r, x, y)) {
            let id = *id;
            self.overlay = self.pane_menu(id, at);
            self.dirty = true;
            return;
        }
        if let Some((_, i)) = hits.tabs.iter().find(|(r, _)| inside(*r, x, y)) {
            let i = *i;
            self.overlay = self.tab_menu(i, at);
            self.dirty = true;
            return;
        }
        if let Some(h) = hits.panes.iter().find(|h| inside(h.rect, x, y)) {
            let id = h.id;
            self.overlay = self.pane_menu(id, at);
            self.dirty = true;
            return;
        }
        // Everything else - a subagent row, buttons, dividers, empty sidebar or top bar, pane
        // borders - opens nothing; only the menu button itself reopens the global menu here.
        if hits.sidebar_menu.is_some_and(|r| inside(r, x, y)) {
            self.overlay = self.global_menu(at);
            self.dirty = true;
        }
    }

    /// A paste goes into the pane as one chunk, so multi-line text isn't seen as a series of Enter presses.
    /// Wherever the keys are not on the pane it never reaches one: a text field takes the first line, as typed, as
    /// `key` routes them.
    pub fn paste(&mut self, s: &str) {
        // As the first key does, it clears the notice.
        if self.notice.take().is_some() | self.toast.take().is_some() {
            self.dirty = true;
        }
        // Terminals paste a line break as CR as often as LF.
        let line = crate::render::unspoofed(s.split(['\n', '\r']).next().unwrap_or_default());
        let focused = self.tab().map(|t| t.layout.focused());
        if !self.overlay.is_open() && self.search.as_ref().is_some_and(|(id, _)| Some(*id) == focused) {
            if let Some((_, search)) = self.search.as_mut() {
                search.paste(&line);
            }
            self.search_refresh(true);
            self.dirty = true;
            return;
        }
        if !self.overlay.is_open() && matches!(self.focus, Focus::Sidebar | Focus::Tabs) {
            return;
        }
        if !self.overlay.is_open()
            && self.focus == Focus::Tree
            && self.view.tree_rect.is_some()
            && let Some(tree) = self.tree.as_mut()
        {
            tree.paste(&line);
            self.dirty = true;
            return;
        }
        if self.overlay.is_open() {
            self.dirty = true;
            self.overlay = match std::mem::replace(&mut self.overlay, Overlay::None) {
                Overlay::Find(mut f) => {
                    f.paste(&line);
                    self.find_outcome(f, crate::find::Outcome::None)
                }
                Overlay::Prompt(mut p, ask) => {
                    p.paste(&line);
                    Overlay::Prompt(p, ask)
                }
                Overlay::NewSpace(mut w) => {
                    w.paste(&line);
                    Overlay::NewSpace(w)
                }
                other => other,
            };
            return;
        }
        let Some(id) = self.tab().map(|t| t.layout.focused()) else { return };
        self.owner_typed(id);
        self.paste_into(id, s);
    }

    /// As a paste into pane `id`; false when there is no such pane.
    pub(crate) fn paste_into(&mut self, id: PaneId, s: &str) -> bool {
        // Control characters other than newline/tab are stripped: an ESC sequence in the
        // clipboard could act as a command, or close the paste bracket early via "\x1b[201~".
        let clean: String = s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')).collect();
        let Some(p) = self.pane_mut(id) else { return false };
        if p.term.screen().bracketed_paste() {
            p.pty.write(b"\x1b[200~");
            p.pty.write(clean.as_bytes());
            p.pty.write(b"\x1b[201~");
        } else {
            p.pty.write(clean.as_bytes());
        }
        true
    }

    /// Delivery waits a while after the owner typed into a pane.
    fn owner_typed(&mut self, id: PaneId) {
        self.mail.owner_typed = Some((id, Instant::now()));
    }
}

/// The Messages window a topology was set up from, taken out of the answer so the window comes back.
fn topology_back(what: &mut Pending) -> Option<Overlay> {
    match what {
        Pending::Topology(_, back) => back.take().map(|w| Overlay::Messages(*w)),
        _ => None,
    }
}

/// How a swallowed reply goes into the log: an image reply holds nothing typed, so its text; anything else only its length.
pub(super) fn swallowed(text: String) -> String {
    if text.starts_with("Gi=") { text } else { format!("{} chars", text.chars().count()) }
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}
