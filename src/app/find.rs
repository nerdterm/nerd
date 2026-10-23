//! What Search's results do: each acts through the code its origin uses, so it is refused where its origin is.

use super::*;
use crate::find::{Ctx, Find, Group, Hit, Outcome, Target};

/// Text search stops past this, and says how far it got.
const TEXT_TIME: Duration = Duration::from_millis(300);
/// ceiling: the first 1000 lines found are listed; a longer query finds the rest.
const TEXT_MAX: usize = 1000;
/// ceiling: the home folder's folders macOS guards with a privacy prompt are never entered by Search's file walk, from
/// whatever folder it starts at, so their files are not found; the way past is to walk them once the tree has opened them.
const GUARDED: [&str; 7] = ["Desktop", "Documents", "Downloads", "Library", "Pictures", "Movies", "Music"];

impl App {
    /// Panes, commands and settings are in memory; plugins, help and files are read on the first query that needs them.
    pub(super) fn find_overlay(&mut self) -> Overlay {
        self.dirty = true;
        let mut f = Find::new();
        f.add(self.pane_hits());
        f.add(self.command_hits());
        f.add(setting_hits(&self.cfg));
        Overlay::Find(f)
    }

    fn pane_hits(&self) -> Vec<Hit> {
        let focused = self.tab().map(|t| t.layout.focused());
        let mut out = Vec::new();
        for (si, sp) in self.spaces.iter().enumerate() {
            let branch = self.repos.get(&sp.cwd).and_then(|r| r.branch.clone()).unwrap_or_default();
            for (ti, tab) in sp.tabs.iter().enumerate() {
                let mut ids = tab.layout.pane_ids();
                ids.sort_unstable();
                for id in ids {
                    let Some(p) = tab.panes.get(&id) else { continue };
                    let state = p.term.state();
                    let current = si == self.active && ti == sp.active_tab && Some(id) == focused;
                    out.push(
                        Hit::new(Group::Panes, p.label(), format!("{} › {}", sp.label(), tab.label(ti)), Target::Pane(id))
                            .value(format!("{} {branch}", crate::paths::shown(&p.cwd)).trim_end())
                            .extra(format!("{} {} {}", p.agent.as_deref().unwrap_or_default(), p.command, crate::find::state_word(state)))
                            .pane((si, sp.label()), tab.label(ti), state, current),
                    );
                }
            }
        }
        out
    }

    /// The main menu, then the menus of the space, tab and pane shown; an item two of them hold is listed once.
    fn command_hits(&self) -> Vec<Hit> {
        let mut menus = vec![(self.global_menu((0, 0)), "Main menu".to_string(), Ctx::Global)];
        if let (Some(sp), Some(id)) = (self.space(), self.tab().map(|t| t.layout.focused())) {
            if let Some(m) = self.space_menu(self.active, (0, 0)) {
                menus.push((m, format!("Space {}", sp.label()), Ctx::Space(id)));
            }
            menus.push((self.tab_menu(sp.active_tab, (0, 0)), format!("Tab {}", sp.tabs[sp.active_tab].label(sp.active_tab)), Ctx::Tab(id)));
            let name = self.pane(id).map(Pane::label).unwrap_or_default();
            menus.push((self.pane_menu(id, (0, 0)), format!("Pane {name}"), Ctx::Pane(id)));
        }
        let mut out: Vec<Hit> = Vec::new();
        for (menu, place, ctx) in menus {
            let Overlay::Menu(m, _) = menu else { continue };
            // Search… itself would only open a fresh Search.
            for item in m.items.into_iter().filter(|i| !i.label.is_empty() && i.action != Action::SearchAll) {
                if out.iter().any(|h| h.name == item.label && matches!(&h.target, Target::Command(a, c) if *a == item.action && same_command(c, &ctx))) {
                    continue;
                }
                out.push(Hit::new(Group::Commands, &item.label, &place, Target::Command(item.action, ctx.clone())).value(&item.key).enabled(item.enabled));
            }
        }
        out
    }

    /// Plugins and help on the first query or tab that needs them, files on the first query: once per window.
    fn find_sources(&mut self, f: &mut Find) {
        if f.wants_lazy() {
            f.lazy = true;
            f.add(self.plugin_hits());
            f.add(crate::help::sections().iter().enumerate().map(|(i, s)| Hit::new(Group::Help, &s.name, "", Target::Help(i)).extra(&s.body).top(s.page)).collect());
        }
        if f.wants_files() {
            // ceiling: one snapshot per window; a file made after it is not found until the window reopens.
            f.files = true;
            // The tree's own folder and hidden files; with the tree closed, what opening it would show.
            let (root, hidden) = match &self.tree {
                Some(t) => (t.root().to_path_buf(), t.hidden()),
                None => (self.space().map(|s| std::fs::canonicalize(&s.cwd).unwrap_or_else(|_| s.cwd.clone())).unwrap_or_default(), false),
            };
            let home = crate::paths::home().and_then(|h| std::fs::canonicalize(h).ok());
            let guarded: Vec<PathBuf> = home.iter().flat_map(|h| GUARDED.map(|g| h.join(g))).collect();
            let (names, cut, skipped) = crate::tree::file_names(&root, hidden, &guarded);
            let mut note = Vec::new();
            if let Some(t) = cut {
                note.push(format!("Stopped after {} ms: some files under {} are not listed.", t.as_millis(), crate::paths::shown(&root)));
            }
            if skipped {
                note.push(format!("{} in the home folder are not searched: macOS would ask to let nerd in.", GUARDED.join(", ")));
            }
            f.files_note = (!note.is_empty()).then(|| note.join(" "));
            f.add(
                names
                    .into_iter()
                    .map(|rel| {
                        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", &rel));
                        Hit::new(Group::Files, name, if dir.is_empty() { "." } else { dir }, Target::File(root.join(&rel))).top(false)
                    })
                    .collect(),
            );
        }
    }

    fn plugin_hits(&self) -> Vec<Hit> {
        let (catalog, _) = crate::catalog::read();
        let list = crate::plugin_list::entries(&catalog, |n| self.plugins.state_of(n));
        let risk = |r: u8| crate::install::RISK[r.min(3) as usize];
        let mut out: Vec<Hit> = list
            .iter()
            .map(|e| Hit::new(Group::Plugins, &e.name, &e.title, Target::Plugin(e.name.clone(), true)).value(format!("{} · {}", if e.off.is_some() { "off" } else { "on" }, risk(e.risk))))
            .collect();
        out.extend(catalog.entries.iter().filter(|c| !list.iter().any(|e| e.name == c.name)).map(|c| {
            Hit::new(Group::Plugins, &c.name, &c.title, Target::Plugin(c.name.clone(), false)).value(format!("not installed · {}", risk(c.risk))).extra(&c.summary).top(false)
        }));
        out
    }

    pub(super) fn find_outcome(&mut self, mut f: Find, out: Outcome) -> Overlay {
        self.dirty = true;
        // While the window stays, what a result says goes on its own line: a notice card would sit under the window.
        let earlier = (self.notice.take(), self.toast.take());
        let mut o = match out {
            Outcome::Close => Overlay::None,
            Outcome::Act(t, what) => self.find_act(f, t, &what),
            Outcome::SearchText => {
                self.find_text(&mut f);
                Overlay::Find(f)
            }
            Outcome::Open(url) => {
                self.open_page(&url);
                Overlay::Find(f)
            }
            Outcome::None => {
                self.find_sources(&mut f);
                Overlay::Find(f)
            }
        };
        if let Overlay::Find(f) = &mut o {
            if let Some(n) = self.notice.take() {
                f.note = Some((true, n));
            } else if let Some(t) = self.toast.take() {
                f.note = Some((false, t));
            }
        }
        self.notice = self.notice.take().or(earlier.0);
        self.toast = self.toast.take().or(earlier.1);
        o
    }

    /// A target that is gone is refused, never redirected: the window stays and says so.
    fn find_act(&mut self, mut f: Find, t: Target, what: &str) -> Overlay {
        let gone = |app: &mut App, f: Find| {
            app.notice = Some(format!("{what} is closed"));
            Overlay::Find(f)
        };
        match t {
            Target::Pane(id) | Target::Line(id, ..) => {
                let Some((si, ti)) = self.locate(id) else { return gone(self, f) };
                self.go_to(si, ti, id);
                if let Target::Line(_, row, q, line) = t {
                    let mut s = crate::search::Search::new();
                    s.query = q;
                    self.search = Some((id, s));
                    self.search_refresh(false);
                    // The line itself first: another match printed next to it may sit nearer the row now.
                    if let Some((_, mut s)) = self.search.take() {
                        if let Some(p) = self.pane(id) {
                            s.choose_row(row, |r| p.term.line(r).is_some_and(|l| l.trim() == line));
                        }
                        self.search = Some((id, s));
                    }
                    self.search_refresh(true);
                }
                Overlay::None
            }
            Target::Command(action, ctx) => {
                let ctx = match ctx {
                    Ctx::Global => MenuCtx::Global,
                    Ctx::Space(id) | Ctx::Tab(id) | Ctx::Pane(id) => {
                        let Some((si, ti)) = self.locate(id) else { return gone(self, f) };
                        match ctx {
                            Ctx::Space(_) => MenuCtx::Space(si),
                            // A tab's commands act on the space shown, as its menu does.
                            Ctx::Tab(_) => {
                                self.active = si;
                                MenuCtx::Tab(ti)
                            }
                            _ => {
                                // A pane's commands act on the tab shown, as its menu does.
                                if (si, ti) != (self.active, self.spaces[si].active_tab) {
                                    self.go_to(si, ti, id);
                                }
                                MenuCtx::Pane(id)
                            }
                        }
                    }
                };
                self.run_action(action, &ctx)
            }
            Target::Setting(label, true) => {
                let mut s = crate::settings::Settings::new(&self.cfg);
                s.inside = self.client_inside;
                s.choose(label);
                let out = s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut self.cfg);
                let _ = self.settings_outcome(s, out);
                f.replace(Group::Settings, setting_hits(&self.cfg));
                Overlay::Find(f)
            }
            Target::Setting(label, false) => {
                let mut s = crate::settings::Settings::new(&self.cfg);
                s.choose(label);
                Overlay::Settings(s)
            }
            Target::Plugin(name, true) => {
                let off = crate::install::read_off().ok().is_some_and(|o| o.contains_key(&name));
                let _ = self.run_action(if off { Action::PluginOn } else { Action::PluginOff }, &MenuCtx::Plugin(name));
                f.replace(Group::Plugins, self.plugin_hits());
                Overlay::Find(f)
            }
            Target::Plugin(name, false) => self.plugins_overlay_at(&name),
            Target::File(p) => {
                match self.tree {
                    None => self.toggle_tree(),
                    Some(_) => self.focus = Focus::Tree,
                }
                if let Some(t) = self.tree.as_mut() {
                    t.select_path(&p);
                }
                Overlay::None
            }
            Target::Help(_) => Overlay::Find(f),
        }
    }

    /// Every pane's text, read once on demand; the time is checked as it goes, so it ends within one pane's read past it.
    fn find_text(&mut self, f: &mut Find) {
        let words = f.words();
        if words.is_empty() {
            return;
        }
        let phrase = f.query.trim().to_string();
        let phrase_lc = phrase.to_lowercase();
        let panes: Vec<(PaneId, String)> = self
            .spaces
            .iter()
            .flat_map(|sp| sp.tabs.iter())
            .flat_map(|t| {
                let mut ids = t.layout.pane_ids();
                ids.sort_unstable();
                ids.into_iter().filter_map(|id| t.panes.get(&id).map(|p| (id, p.label()))).collect::<Vec<_>>()
            })
            .collect();
        let started = Instant::now();
        let (mut hits, mut searched, mut part) = (Vec::new(), 0, None);
        for (id, label) in &panes {
            if started.elapsed() >= TEXT_TIME || hits.len() >= TEXT_MAX {
                break;
            }
            let at = Instant::now();
            let Some(p) = self.pane_mut(*id) else { continue };
            let rows = p.term.screen().size().0 as usize;
            let lines = p.term.all_lines();
            let history = lines.len().saturating_sub(rows) as isize;
            let mut late = false;
            for (i, line) in lines.iter().enumerate() {
                if i % 1024 == 0 && started.elapsed() >= TEXT_TIME {
                    late = true;
                    break;
                }
                let lc = line.to_lowercase();
                if hits.len() < TEXT_MAX && words.iter().all(|w| lc.contains(w.as_str())) {
                    // The jump searches the pane once: for the phrase where the line has it, else for the first word.
                    let q = if lc.contains(&phrase_lc) { phrase.clone() } else { words[0].clone() };
                    hits.push(Hit::new(Group::Text, line.trim(), format!("Pane {label}"), Target::Line(*id, i as isize - history, q, line.trim().to_string())));
                }
            }
            if late {
                part = Some(crate::render::unspoofed(label));
                break;
            }
            searched += 1;
            crate::log::debug(&format!("search: pane {id}: {} lines in {} ms", lines.len(), at.elapsed().as_millis()));
        }
        crate::log::info(&format!("search: pane text of {searched} of {} panes in {} ms, {} lines found", panes.len(), started.elapsed().as_millis(), hits.len()));
        let note = if hits.len() >= TEXT_MAX {
            Some(format!("First {TEXT_MAX} lines; type more to narrow."))
        } else {
            (searched < panes.len()).then(|| match &part {
                Some(l) => format!("Searched {searched} of {} panes, and {l} only in part.", panes.len()),
                None => format!("Searched {searched} of {} panes.", panes.len()),
            })
        };
        f.set_text(hits, note);
    }

    /// A help page on nerdterm.dev, opened on the server's Mac; a window from another Mac is told the address.
    fn open_page(&mut self, url: &str) {
        if self.caps.bye {
            self.toast = Some(format!("this window is on another Mac; the page is {url}"));
            return;
        }
        match crate::links::open(&crate::links::Link::Url(url.into()), std::path::Path::new("/")) {
            Ok(()) => self.toast = Some("opened".to_string()),
            Err(e) => self.notice = Some(e),
        }
    }
}

/// Same label and action are one row, unless both are space or tab items of different kinds: each acts on its own object.
fn same_command(a: &Ctx, b: &Ctx) -> bool {
    let shared = |c: &Ctx| matches!(c, Ctx::Global | Ctx::Pane(_));
    std::mem::discriminant(a) == std::mem::discriminant(b) || shared(a) || shared(b)
}

fn setting_hits(cfg: &Config) -> Vec<Hit> {
    crate::settings::Settings::new(cfg)
        .rows()
        .into_iter()
        .map(|(section, label, value, in_place)| Hit::new(Group::Settings, label, format!("Settings › {section}"), Target::Setting(label, in_place)).value(value))
        .collect()
}
