//! The Isolation window (files and network for a pane, or a tab's or space's new panes) and the Accounts window.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::IsolationTarget;
use crate::chrome::Scroll;
use crate::messages::{Drawn, Line, chip};
use crate::agents::AgentNetwork;
use crate::policy::{Account, Chosen, Files, Isolation, Network, entries};
use crate::render::Theme;

#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// No isolation of its own: as the space, or as nerd's default.
    Inherit,
    Files(Files),
    Network(Network),
    /// A provider's chip: on or off among the chosen.
    Provider(String),
    /// Choose providers… for this agent.
    Choose(String),
    /// In Choose providers: every provider, one provider on or off, done.
    All,
    Group(String),
    Done,
    /// true: and restart the program.
    Save(bool),
    Admit,
    Remove(String),
    /// The commands for this account name.
    Show(String),
    Copy,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
    Save(Option<Isolation>, bool),
    Admit,
    Remove(String),
    Copy(String),
}

#[derive(Clone, Debug)]
pub struct IsolationWin {
    pub target: IsolationTarget,
    /// What it is for: the pane's name, or the tab or space.
    pub about: String,
    /// None: nothing of its own.
    pub pick: Option<Isolation>,
    /// A tab's or a space's word for nothing of its own; None for a pane, which always has one.
    pub inherit: Option<&'static str>,
    pub accounts: Vec<String>,
    /// Agents whose manifest names their provider's hosts.
    pub providers: Vec<String>,
    /// The networks of the agents among them that name provider groups.
    pub groups: Vec<(String, AgentNetwork)>,
    /// What "only these providers" means until others are chosen: per agent, all or some of its providers.
    pub chosen: Chosen,
    /// Saved entries no chip or manifest stands for now: shown, and never saved again.
    pub stale: Vec<String>,
    /// Choose providers, open over the window.
    pub view: Option<View>,
    /// `<title>: <how to turn its own sandbox off>`, one per agent that says.
    pub notes: Vec<String>,
    /// The pane's folder (a tab's or space's: the space's) is the home folder itself.
    pub home: bool,
    pub scroll: Scroll,
}

/// Choose providers for one agent: what is ticked (None: all), the lines shown, and whether done found nothing ticked.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub agent: String,
    pub draft: Option<Vec<String>>,
    pub scroll: Scroll,
    pub empty: bool,
}

const CANCEL: &str = " [Esc] cancel ";
const BACK: &str = " [Esc] back ";
const DONE: &str = " [Enter] done ";
const NOTHING_TICKED: &str = "choose at least one provider, or all";
/// Seatbelt refuses a setuid program under a profile.
const HOME_FOLDER: &str = "This pane's folder is your home folder, so it can change your files there, though not dot files, ~/Library or ~/bin; choose a project folder for a real limit.";
/// What a This-folder-only pane costs, each line in what stops working.
const FOLDER_COSTS: [&str; 4] = [
    "To keep it from planting code or instructions for your other sessions, here:",
    "  agents cannot save their settings or memory; Claude Code has no task list",
    "  Codex sessions and Qwen Code workflows cannot be resumed later",
    "  uv, pnpm, Go, pip and others fail unless their cache is in this folder",
];
const NO_AGENT: &str = "  git over ssh and docker do not work: no ssh agent, no Docker";
const SETUID: &str = "ps, top, sudo and other setuid programs do not run in an isolated pane";

/// Down on a frame: its close button or outside it closes; otherwise what was hit.
fn pressed<H: Clone>(ev: crossterm::event::MouseEvent, d: &Drawn<H>) -> Result<Option<H>, Out> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if ev.kind != MouseEventKind::Down(MouseButton::Left) {
        return Ok(None);
    }
    let at = (ev.column, ev.row).into();
    if d.close.contains(at) || !d.frame.contains(at) {
        return Err(Out::Close);
    }
    Ok(d.at(ev.column, ev.row))
}

impl IsolationWin {
    fn title(&self) -> &'static str {
        if self.inherit.is_some() { "Isolation for new panes" } else { "Isolation" }
    }

    fn buttons(&self) -> Vec<(String, Click)> {
        let mut b = vec![(" [Enter] save ".to_string(), Click::Save(false))];
        if self.inherit.is_none() {
            b.push((" [r] save and restart the program ".into(), Click::Save(true)));
        }
        b
    }

    pub fn lines(&self, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let mut out: Vec<Line<Click>> = vec![vec![(self.about.clone(), g.fg(theme.accent).add_modifier(Modifier::BOLD), None)], Vec::new()];
        let radio = |on: bool, label: String, click: Option<Click>| -> Line<Click> {
            let style = g.fg(if click.is_some() { theme.text } else { theme.faint });
            vec![(format!("  {} {label}", if on { "●" } else { "○" }), style, click)]
        };
        if let Some(word) = self.inherit {
            out.push(radio(self.pick.is_none(), word.into(), Some(Click::Inherit)));
        }
        let files = self.pick.as_ref().map(|i| &i.files);
        let network = self.pick.as_ref().map(|i| &i.network);
        out.push(vec![("Files".into(), g.fg(theme.muted), None)]);
        out.push(radio(files == Some(&Files::Everything), "Everything".into(), Some(Click::Files(Files::Everything))));
        out.push(radio(files == Some(&Files::Folder), "This folder only".into(), Some(Click::Files(Files::Folder))));
        for a in &self.accounts {
            let f = Files::Account(a.clone());
            out.push(radio(files == Some(&f), format!("Another account: {a}"), Some(Click::Files(f))));
        }
        out.push(vec![("Network".into(), g.fg(theme.muted), None)]);
        out.push(radio(network == Some(&Network::Open), "Open".into(), Some(Click::Network(Network::Open))));
        let provider = matches!(network, Some(Network::Providers(_)));
        if self.providers.is_empty() {
            out.push(radio(false, "Only these providers: no agent names its hosts".into(), None));
        } else {
            let mut l = radio(provider, "Only these providers: ".into(), Some(Click::Network(Network::Providers(entries(&self.chosen)))));
            for p in &self.providers {
                let lit = (provider && self.chosen.iter().any(|(a, _)| a == p)).then(|| theme.cursor());
                chip(&mut l, p, lit, Some(Click::Provider(p.clone())), theme);
            }
            out.push(l);
            for (a, gs) in self.chosen.iter().filter(|(a, _)| provider && self.groups.iter().any(|(g, _)| g == a)) {
                let said = gs.as_ref().map_or_else(|| "all".to_string(), |gs| gs.join(", "));
                out.push(vec![(format!("      {a}: {said}  "), g.fg(theme.text), None), ("[Choose providers…]".into(), g.fg(theme.accent), Some(Click::Choose(a.clone())))]);
            }
            if provider {
                out.extend(self.stale.iter().map(|s| vec![(format!("      {s}"), g.fg(theme.warn), None)]));
            }
        }
        out.push(radio(network == Some(&Network::None), "None".into(), Some(Click::Network(Network::None))));
        let folder = files == Some(&Files::Folder);
        out.push(Vec::new());
        if folder {
            out.extend(FOLDER_COSTS.iter().map(|t| vec![((*t).into(), g.fg(theme.faint), None)]));
        }
        if folder && network == Some(&Network::Open) {
            out.push(vec![(NO_AGENT.into(), g.fg(theme.faint), None)]);
        }
        out.push(vec![(SETUID.into(), g.fg(theme.faint), None)]);
        for n in &self.notes {
            out.push(vec![(n.clone(), g.fg(theme.faint), None)]);
        }
        if folder && self.home {
            out.push(vec![(HOME_FOLDER.into(), g.fg(theme.warn), None)]);
        }
        out
    }

    /// Choose providers: all, then each provider with its hosts, cut to `width`.
    fn view_lines(&self, v: &View, width: usize, theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let n = self.groups.iter().find(|(a, _)| *a == v.agent).map(|(_, n)| n.clone()).unwrap_or_default();
        let all = match &n.net {
            crate::agents::Network::Internet { hosts, .. } => hosts.len(),
            _ => 0,
        };
        let mark = |on: bool| if on { "●" } else { "○" };
        let mut out: Vec<Line<Click>> = vec![vec![(format!("{}: providers of {}", self.about, v.agent), g.fg(theme.accent).add_modifier(Modifier::BOLD), None)], Vec::new()];
        out.push(vec![(format!("  {} all ({all} hosts)", mark(v.draft.is_none())), g.fg(theme.text), Some(Click::All))]);
        let w = n.providers.iter().map(|(p, _)| p.chars().count()).max().unwrap_or(0);
        for (p, hosts) in &n.providers {
            let on = v.draft.as_ref().is_some_and(|d| d.contains(p));
            let head = format!("  {} {p:<w$}  ", mark(on));
            let room = width.saturating_sub(head.chars().count());
            let hit = Some(Click::Group(p.clone()));
            out.push(vec![(head, g.fg(theme.text), hit.clone()), (crate::messages::cut(&hosts.join(", "), room), g.fg(theme.muted), hit)]);
        }
        if v.empty {
            out.push(Vec::new());
            out.push(vec![(NOTHING_TICKED.into(), g.fg(theme.warn), None)]);
        }
        out
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn<Click> {
        if let Some(v) = &self.view {
            let r = crate::chrome::fit(screen, crate::chrome::WIDE_W, u16::MAX);
            let lines = self.view_lines(v, r.width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal) as usize, theme);
            let r = crate::chrome::fit(screen, r.width, lines.len() as u16);
            return crate::messages::draw(buf, r, "Choose providers", theme, &lines, &v.scroll, &[(DONE.to_string(), Click::Done)], BACK);
        }
        let (r, lines) = crate::messages::fit(screen, self.lines(theme));
        crate::messages::draw(buf, r, self.title(), theme, &lines, &self.scroll, &self.buttons(), CANCEL)
    }

    fn click(&mut self, c: Click) -> Out {
        match c {
            Click::Inherit => self.pick = None,
            Click::Files(f) => self.pick.get_or_insert_default().files = f,
            Click::Network(n) => self.pick.get_or_insert_default().network = n,
            Click::Provider(p) => {
                let on = matches!(self.pick.as_ref().map(|i| &i.network), Some(Network::Providers(_)));
                match self.chosen.iter().position(|(a, _)| *a == p) {
                    _ if !on => self.chosen = vec![(p, None)],
                    Some(i) if self.chosen.len() > 1 => _ = self.chosen.remove(i),
                    Some(_) => {}
                    None => self.chosen.push((p, None)),
                }
                self.pick.get_or_insert_default().network = Network::Providers(entries(&self.chosen));
            }
            Click::Choose(agent) => {
                let draft = self.chosen.iter().find(|(a, _)| *a == agent).and_then(|(_, gs)| gs.clone());
                self.view = Some(View { agent, draft, scroll: Scroll::default(), empty: false });
            }
            Click::All | Click::Group(_) | Click::Done => self.view_click(c),
            Click::Save(restart) => return Out::Save(self.pick.clone(), restart),
            _ => {}
        }
        Out::None
    }

    fn view_click(&mut self, c: Click) {
        let Some(v) = self.view.as_mut() else { return };
        v.empty = false;
        match c {
            Click::All => v.draft = None,
            Click::Group(p) => match v.draft.as_mut() {
                None => v.draft = Some(vec![p]),
                Some(d) if d.contains(&p) => d.retain(|x| *x != p),
                Some(d) => d.push(p),
            },
            Click::Done if v.draft.as_ref().is_some_and(Vec::is_empty) => v.empty = true,
            Click::Done => {
                let Some(View { agent, mut draft, .. }) = self.view.take() else { return };
                if let Some(d) = draft.as_mut() {
                    d.sort();
                }
                if let Some((_, gs)) = self.chosen.iter_mut().find(|(a, _)| *a == agent) {
                    *gs = draft;
                }
                if let Some(i) = self.pick.as_mut().filter(|i| matches!(i.network, Network::Providers(_))) {
                    i.network = Network::Providers(entries(&self.chosen));
                }
            }
            _ => {}
        }
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Out {
        use crossterm::event::KeyCode::*;
        if let Some(v) = self.view.as_mut() {
            match crate::keys::normalize(k).code {
                Enter => self.view_click(Click::Done),
                Esc => self.view = None,
                code => _ = v.scroll.key(code),
            }
            return Out::None;
        }
        match crate::keys::normalize(k).code {
            Enter => self.click(Click::Save(false)),
            Char('r') if self.inherit.is_none() => self.click(Click::Save(true)),
            Esc => Out::Close,
            code => {
                self.scroll.key(code);
                Out::None
            }
        }
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        use crossterm::event::MouseEventKind::{ScrollDown, ScrollUp};
        if let Some(v) = self.view.as_mut() {
            match (ev.kind, pressed(ev, d)) {
                (ScrollUp | ScrollDown, _) => _ = v.scroll.wheel(ev.kind),
                (_, Err(_)) => self.view = None,
                (_, Ok(Some(c))) => _ = self.click(c),
                (_, Ok(None)) => {}
            }
            return Out::None;
        }
        if self.scroll.wheel(ev.kind) {
            return Out::None;
        }
        match pressed(ev, d) {
            Err(out) => out,
            Ok(Some(c)) => self.click(c),
            Ok(None) => Out::None,
        }
    }
}

/// The saved providers entries a chip and a manifest's group still stand for, and words for the rest.
pub fn known(saved: &[String], providers: &[String], groups: &[(String, AgentNetwork)]) -> (Chosen, Vec<String>) {
    let (chosen, bad) = crate::policy::chosen(saved);
    let mut stale: Vec<String> = bad.iter().map(|e| format!("{e}: not an agent or an agent/provider")).collect();
    let mut out = Vec::new();
    for (a, gs) in chosen {
        if !providers.contains(&a) {
            let why = if crate::agents::get(&a).is_some() { "names no hosts to choose from" } else { "no such agent is installed" };
            stale.push(format!("{a}: {why}"));
            continue;
        }
        let Some(gs) = gs else {
            out.push((a, None));
            continue;
        };
        let have = groups.iter().find(|(n, _)| *n == a).map(|(_, n)| &n.providers);
        let (keep, gone): (Vec<String>, Vec<String>) = gs.into_iter().partition(|g| have.is_some_and(|h| h.iter().any(|(n, _)| n == g)));
        stale.extend(gone.iter().map(|g| format!("{g}: no longer in {a}'s manifest")));
        if !keep.is_empty() {
            out.push((a, Some(keep)));
        }
    }
    (out, stale)
}

/// Accounts…: the admitted accounts, and the commands that make one.
#[derive(Clone, Debug)]
pub struct Accounts {
    /// The account the commands are written for.
    pub name: String,
    owner: String,
    scroll: Scroll,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts { name: "agent".into(), owner: std::env::var("USER").unwrap_or_else(|_| "you".into()), scroll: Scroll::default() }
    }
}

impl Accounts {
    pub fn for_name(name: &str) -> Self {
        Accounts { name: name.into(), ..Self::default() }
    }

    /// What the owner runs to make the account: shown, never run by nerd. No sudoers line: sudo asks each time.
    pub fn commands(&self) -> [String; 3] {
        let a = &self.name;
        [
            format!("sudo sysadminctl -addUser {a} -fullName \"{a} agent\" -shell /bin/zsh -home /Users/{a} -password - interactive"),
            format!("sudo dscl . create /Users/{a} IsHidden 1"),
            format!("sudo chmod 700 /Users/{a}"),
        ]
    }

    /// Once, and after each update: the copy of this nerd only root can change, the one account panes run.
    fn install(&self) -> [String; 2] {
        crate::isolation::install_commands(&std::env::current_exe().unwrap_or_default())
    }

    /// The optional, less safe line that starts the account's panes without a password.
    fn sudoers(&self) -> [String; 2] {
        let (a, owner) = (&self.name, &self.owner);
        let file = format!("/etc/sudoers.d/nerd-{a}");
        [format!("printf '{owner} ALL=({a}) NOPASSWD: ALL\\n' | sudo tee {file}"), format!("sudo visudo -c -f {file}")]
    }

    fn pf(&self) -> String {
        format!("echo 'block out quick proto {{ tcp udp }} from any to any user {0}' | sudo tee /etc/pf.anchors/nerd-{0}", self.name)
    }

    pub fn lines(&self, accounts: &[Account], theme: &Theme) -> Vec<Line<Click>> {
        let g = Style::default().bg(theme.ground);
        let mut out: Vec<Line<Click>> = vec![vec![("Admitted accounts".into(), g.fg(theme.muted), None)]];
        if accounts.is_empty() {
            out.push(vec![("  none yet".into(), g.fg(theme.faint), None)]);
        }
        for a in accounts {
            let shown = a.name == self.name;
            let mut l: Line<Click> = vec![(format!("  {}  uid {}  ", a.name, a.uid), g.fg(if shown { theme.accent } else { theme.text }), Some(Click::Show(a.name.clone())))];
            if shown {
                crate::messages::button(&mut l, "x", "remove", Click::Remove(a.name.clone()), theme);
            } else {
                chip(&mut l, "remove", None, Some(Click::Remove(a.name.clone())), theme);
            }
            out.push(l);
        }
        out.push(Vec::new());
        out.push(vec![(format!("To give an agent the account {}, run these in your terminal:", self.name), g.fg(theme.muted), None)]);
        for c in self.commands() {
            out.push(vec![(format!("  {c}"), g.fg(theme.text), None)]);
        }
        out.push(vec![("A pane of the account then asks for your password (sudo) each time it starts.".into(), g.fg(theme.muted), None)]);
        out.push(Vec::new());
        out.push(vec![("Once, and again after each nerd update: the copy of nerd its panes run, which only root can change".into(), g.fg(theme.muted), None)]);
        for c in self.install() {
            out.push(vec![(format!("  {c}"), g.fg(theme.text), None)]);
        }
        out.push(vec![("Until that copy matches the running nerd, panes of the account refuse to start.".into(), g.fg(theme.muted), None)]);
        out.push(Vec::new());
        out.push(vec![("Optional, less safe: start its panes without a password".into(), g.fg(theme.muted), None)]);
        for c in self.sudoers() {
            out.push(vec![(format!("  {c}"), g.fg(theme.text), None)]);
        }
        let a = &self.name;
        out.push(vec![(format!("with this line: any program running as you can act as {a} at any time, without a password"), g.fg(theme.warn), None)]);
        out.push(vec![(format!("without it: sudo asks for your password each time a pane of {a} starts, so nothing acts as {a} unless you type it"), g.fg(theme.muted), None)]);
        out.push(vec![("neither stops a program that already controls your own session".into(), g.fg(theme.muted), None)]);
        out.push(Vec::new());
        out.push(vec![("Optional, not checked by nerd: no network for the account".into(), g.fg(theme.muted), None)]);
        out.push(vec![(format!("  {}", self.pf()), g.fg(theme.text), None)]);
        out
    }

    fn buttons() -> Vec<(String, Click)> {
        vec![(" [a] admit an account… ".into(), Click::Admit), (" [c] copy the commands ".into(), Click::Copy)]
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, accounts: &[Account]) -> Drawn<Click> {
        let (r, lines) = crate::messages::fit(screen, self.lines(accounts, theme));
        crate::messages::draw(buf, r, "Accounts", theme, &lines, &self.scroll, &Self::buttons(), crate::messages::CLOSE)
    }

    fn click(&mut self, c: Click) -> Out {
        match c {
            Click::Show(n) => self.name = n,
            Click::Remove(n) => return Out::Remove(n),
            Click::Admit => return Out::Admit,
            Click::Copy => return Out::Copy(self.commands().into_iter().chain(self.install()).collect::<Vec<_>>().join("\n")),
            _ => {}
        }
        Out::None
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent, accounts: &[Account]) -> Out {
        use crossterm::event::KeyCode::*;
        match crate::keys::normalize(k).code {
            Char('a') => Out::Admit,
            Char('c') => self.click(Click::Copy),
            Char('x') if accounts.iter().any(|a| a.name == self.name) => Out::Remove(self.name.clone()),
            Esc => Out::Close,
            code => {
                self.scroll.key(code);
                Out::None
            }
        }
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        if self.scroll.wheel(ev.kind) {
            return Out::None;
        }
        match pressed(ev, d) {
            Err(out) => out,
            Ok(Some(c)) => self.click(c),
            Ok(None) => Out::None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn pane() -> IsolationWin {
        IsolationWin {
            target: IsolationTarget::Pane(3),
            about: "reader".into(),
            pick: Some(Isolation::default()),
            inherit: None,
            accounts: vec!["scout".into()],
            providers: vec!["claude".into(), "codex".into()],
            groups: Vec::new(),
            chosen: vec![("codex".into(), None)],
            stale: Vec::new(),
            view: None,
            notes: vec!["Codex: run it with --sandbox danger-full-access".into()],
            home: false,
            scroll: Scroll::default(),
        }
    }

    /// `fake` has the providers alpha (a.dev) and beta (b.dev, c.dev); the pane is set to only claude.
    pub(crate) fn grouped() -> IsolationWin {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let net = AgentNetwork { net: crate::agents::Network::Internet { hosts: s(&["a.dev", "b.dev", "c.dev"]), ports: vec![443] }, providers: vec![("alpha".into(), s(&["a.dev"])), ("beta".into(), s(&["b.dev", "c.dev"]))] };
        IsolationWin {
            providers: s(&["claude", "fake"]),
            groups: vec![("fake".into(), net)],
            chosen: vec![("claude".into(), None)],
            pick: Some(Isolation { files: Files::Everything, network: Network::Providers(s(&["claude"])) }),
            ..pane()
        }
    }

    fn saved(w: &IsolationWin) -> Vec<String> {
        match w.pick.as_ref().map(|i| &i.network) {
            Some(Network::Providers(p)) => p.clone(),
            n => panic!("{n:?}"),
        }
    }

    fn shown(w: &IsolationWin) -> String {
        let screen = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(screen);
        w.draw(&mut buf, screen, &Theme::dark());
        crate::app::snapshots::dump(&buf, screen)
    }

    #[test]
    fn choose_providers_ticks_by_mouse_and_done_writes_the_choice_back() {
        let mut w = grouped();
        click_on(&mut w, " fake ");
        assert_eq!(saved(&w), ["claude", "fake"]);
        assert!(shown(&w).contains("fake: all  [Choose providers…]"), "{}", shown(&w));
        click_on(&mut w, "[Choose providers…]");
        let view = shown(&w);
        assert!(view.contains("Choose providers") && view.contains("● all (3 hosts)") && view.contains("○ alpha  a.dev") && view.contains("○ beta   b.dev, c.dev") && view.contains("[Enter] done") && view.contains("[Esc] back"), "{view}");
        click_on(&mut w, "alpha");
        assert!(shown(&w).contains("● alpha") && shown(&w).contains("○ all"), "a provider clears all");
        assert_eq!(saved(&w), ["claude", "fake"], "nothing changes until done");
        click_on(&mut w, "[Enter] done");
        assert_eq!((w.view.as_ref(), saved(&w)), (None, vec!["claude".to_string(), "fake/alpha".into()]));
        assert!(shown(&w).contains("fake: alpha  [Choose providers…]"), "{}", shown(&w));
        click_on(&mut w, "[Choose providers…]");
        click_on(&mut w, "beta");
        assert_eq!(w.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Out::None, "Esc goes back, not out of the window");
        assert_eq!((w.view.as_ref(), saved(&w)), (None, vec!["claude".to_string(), "fake/alpha".into()]), "back leaves the choice");
        click_on(&mut w, "[Choose providers…]");
        click_on(&mut w, "alpha");
        click_on(&mut w, "[Enter] done");
        assert!(w.view.as_ref().is_some_and(|v| v.empty) && shown(&w).contains(NOTHING_TICKED), "nothing ticked stays: {}", shown(&w));
        click_on(&mut w, "all (3 hosts)");
        assert!(!shown(&w).contains(NOTHING_TICKED));
        assert_eq!(w.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Out::None, "Enter is done in the view");
        assert_eq!(saved(&w), ["claude", "fake"]);
        click_on(&mut w, "[Choose providers…]");
        click_on(&mut w, "[Esc] back");
        assert!(w.view.is_none(), "the back button");
        assert_eq!(click_on(&mut w, "[Enter] save"), Out::Save(Some(Isolation { files: Files::Everything, network: Network::Providers(vec!["claude".into(), "fake".into()]) }), false));
    }

    #[test]
    fn the_only_agent_s_chip_keeps_its_providers() {
        let mut w = IsolationWin { chosen: vec![("fake".into(), Some(vec!["alpha".into()]))], ..grouped() };
        w.pick = Some(Isolation { files: Files::Everything, network: Network::Providers(vec!["fake/alpha".into()]) });
        click_on(&mut w, " fake ");
        assert_eq!(saved(&w), ["fake/alpha"], "the last chip stays, and never as all of the agent");
    }

    #[test]
    fn leaving_the_view_by_mouse_keeps_the_choice() {
        let mut w = grouped();
        w.click(Click::Provider("fake".into()));
        w.click(Click::Choose("fake".into()));
        w.click(Click::Group("alpha".into()));
        w.click(Click::Done);
        assert_eq!(saved(&w), ["claude", "fake/alpha"]);
        click_on(&mut w, "[Choose providers…]");
        click_on(&mut w, "beta");
        click_on(&mut w, "[Esc] back");
        assert_eq!((w.view.as_ref(), saved(&w)), (None, vec!["claude".to_string(), "fake/alpha".into()]), "the back button drops the ticks");
        click_on(&mut w, "[Choose providers…]");
        click_on(&mut w, "all (3 hosts)");
        let screen = Rect::new(0, 0, 100, 40);
        let d = w.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        assert!(!d.frame.contains(ratatui::layout::Position::new(0, 0)));
        w.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 0, row: 0, modifiers: KeyModifiers::NONE }, &d);
        assert_eq!((w.view.as_ref(), saved(&w)), (None, vec!["claude".to_string(), "fake/alpha".into()]), "a click outside drops the ticks");
    }

    #[test]
    fn choose_providers_scrolls_as_every_window() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let many: Vec<(String, Vec<String>)> = (0..60).map(|i| (format!("p{i:02}"), s(&[&format!("h{i}.dev")]))).collect();
        let hosts = many.iter().flat_map(|(_, h)| h.clone()).collect();
        let net = AgentNetwork { net: crate::agents::Network::Internet { hosts, ports: vec![443] }, providers: many };
        let mut w = IsolationWin { groups: vec![("fake".into(), net)], ..grouped() };
        w.click(Click::Provider("fake".into()));
        w.click(Click::Choose("fake".into()));
        let (screen, theme, w) = (Rect::new(0, 0, 160, 24), Theme::dark(), std::cell::RefCell::new(w));
        crate::chrome::tests::scrolls("Choose providers", screen, false, &|b| _ = w.borrow().draw(b, screen, &theme), &|k| _ = w.borrow_mut().key(KeyEvent::new(k, KeyModifiers::NONE)), &|kind| {
            let d = w.borrow().draw(&mut Buffer::empty(screen), screen, &theme);
            w.borrow_mut().mouse(MouseEvent { kind, column: 80, row: 12, modifiers: KeyModifiers::NONE }, &d);
        });
    }

    #[test]
    fn keys_scroll_the_view_no_further_than_the_wheel() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let many: Vec<(String, Vec<String>)> = (0..60).map(|i| (format!("p{i:02}"), s(&[&format!("h{i}.dev")]))).collect();
        let hosts = many.iter().flat_map(|(_, h)| h.clone()).collect();
        let net = AgentNetwork { net: crate::agents::Network::Internet { hosts, ports: vec![443] }, providers: many };
        let mut w = IsolationWin { groups: vec![("fake".into(), net)], ..grouped() };
        w.click(Click::Provider("fake".into()));
        w.click(Click::Choose("fake".into()));
        shown(&w);
        let key = |w: &mut IsolationWin, c| _ = w.key(KeyEvent::new(c, KeyModifiers::NONE));
        key(&mut w, KeyCode::End);
        let last = w.view.as_ref().unwrap().scroll.get();
        assert!(last > 0, "60 providers do not fit 40 rows");
        for _ in 0..200 {
            key(&mut w, KeyCode::Down);
        }
        assert_eq!(w.view.as_ref().unwrap().scroll.get(), last);
        key(&mut w, KeyCode::Up);
        assert_eq!(w.view.as_ref().unwrap().scroll.get(), last - 1, "one Up moves the view at once");
    }

    #[test]
    fn every_provider_ticked_one_by_one_stays_a_list() {
        let mut w = grouped();
        w.click(Click::Provider("fake".into()));
        w.click(Click::Choose("fake".into()));
        w.click(Click::Group("beta".into()));
        w.click(Click::Group("alpha".into()));
        w.click(Click::Done);
        assert_eq!(saved(&w), ["claude", "fake/alpha", "fake/beta"], "a provider added to the manifest later stays closed");
    }

    #[test]
    fn a_chip_turned_off_drops_every_entry_of_its_agent() {
        let mut w = IsolationWin { chosen: vec![("fake".into(), Some(vec!["alpha".into()])), ("claude".into(), None)], ..grouped() };
        w.pick = Some(Isolation { files: Files::Everything, network: Network::Providers(entries(&w.chosen)) });
        click_on(&mut w, " fake ");
        assert_eq!(saved(&w), ["claude"]);
        click_on(&mut w, " fake ");
        assert_eq!(saved(&w), ["claude", "fake"], "on again: all of it, as a chip says");
    }

    #[test]
    fn no_click_sequence_puts_an_agent_beside_its_own_providers() {
        let steps: [&[Click]; 6] = [
            &[Click::Provider("fake".into())],
            &[Click::Provider("claude".into())],
            &[Click::Choose("fake".into()), Click::All, Click::Done],
            &[Click::Choose("fake".into()), Click::Group("alpha".into()), Click::Done],
            &[Click::Choose("fake".into()), Click::Group("beta".into()), Click::Group("alpha".into()), Click::Done],
            &[Click::Network(Network::Open)],
        ];
        for n in 0..6usize.pow(4) {
            let mut w = grouped();
            let order: Vec<usize> = (0..4).map(|i| n / 6usize.pow(i) % 6).collect();
            for &i in &order {
                for c in steps[i] {
                    w.click(c.clone());
                }
                w.view = None;
            }
            w.click(Click::Network(Network::Providers(entries(&w.chosen))));
            let e = saved(&w);
            let (c, bad) = crate::policy::chosen(&e);
            assert!(bad.is_empty() && entries(&c) == e, "{order:?}: normal form {e:?}");
            assert!(!(e.contains(&"fake".to_string()) && e.iter().any(|x| x.starts_with("fake/"))), "{order:?}: {e:?}");
        }
    }

    fn click_on(w: &mut IsolationWin, label: &str) -> Out {
        let screen = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(screen);
        let d = w.draw(&mut buf, screen, &Theme::dark());
        let (x, y) = (0..screen.height)
            .find_map(|y| {
                let row: String = (0..screen.width).map(|x| buf[(x, y)].symbol()).collect();
                row.find(label).map(|b| (row[..b].chars().count() as u16, y))
            })
            .unwrap_or_else(|| panic!("no {label}"));
        w.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }, &d)
    }

    #[test]
    fn choices_by_mouse_then_save_and_restart() {
        let mut w = pane();
        click_on(&mut w, "This folder only");
        click_on(&mut w, " claude ");
        assert_eq!(w.pick, Some(Isolation { files: Files::Folder, network: Network::Providers(vec!["claude".into()]) }), "off, a chip chooses itself alone");
        click_on(&mut w, " codex ");
        click_on(&mut w, " claude ");
        assert_eq!(w.pick.as_ref().map(|i| &i.network), Some(&Network::Providers(vec!["codex".into()])), "on, a chip toggles");
        click_on(&mut w, " codex ");
        assert_eq!(w.pick.as_ref().map(|i| &i.network), Some(&Network::Providers(vec!["codex".into()])), "the last one stays");
        click_on(&mut w, " claude ");
        let shown = {
            let mut buf = Buffer::empty(Rect::new(0, 0, 100, 40));
            w.draw(&mut buf, Rect::new(0, 0, 100, 40), &Theme::dark());
            crate::app::snapshots::dump(&buf, Rect::new(0, 0, 100, 40))
        };
        assert!(FOLDER_COSTS.iter().all(|c| shown.contains(c.trim())) && shown.contains("Codex: run it with") && shown.contains(SETUID), "{shown}");
        assert!(!shown.contains(NO_AGENT), "only these providers: {shown}");
        click_on(&mut w, "Open");
        assert!(w.lines(&Theme::dark()).iter().flatten().any(|(t, _, _)| t == NO_AGENT), "this folder only with open network");
        click_on(&mut w, "Another account: scout");
        click_on(&mut w, "None");
        let want = Isolation { files: Files::Account("scout".into()), network: Network::None };
        assert_eq!(click_on(&mut w, "[r] save and restart"), Out::Save(Some(want.clone()), true));
        assert_eq!(w.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Out::Save(Some(want), false));
        assert_eq!(click_on(&mut w, "[Esc] cancel"), Out::Close);
    }

    #[test]
    fn this_folder_only_in_the_home_folder_warns_that_it_limits_little() {
        let shown = |w: &IsolationWin| w.lines(&Theme::dark()).iter().flatten().any(|(t, s, _)| t == HOME_FOLDER && s.fg == Some(Theme::dark().warn));
        let mut w = IsolationWin { home: true, ..pane() };
        assert!(!shown(&w), "files open: nothing to warn about");
        click_on(&mut w, "This folder only");
        assert!(shown(&w), "in warn, under the choices");
        w.home = false;
        assert!(!shown(&w), "a project folder is a real limit");
    }

    #[test]
    fn a_tab_can_go_back_to_its_space() {
        let mut w = IsolationWin { target: IsolationTarget::Tab(0, 1), inherit: Some("as the space"), pick: None, ..pane() };
        click_on(&mut w, "None");
        assert_eq!(w.pick, Some(Isolation { files: Files::Everything, network: Network::None }));
        click_on(&mut w, "as the space");
        assert_eq!(click_on(&mut w, "[Enter] save"), Out::Save(None, false));
        assert_eq!(w.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)), Out::None, "no program to restart");
    }

    #[test]
    fn the_accounts_window_writes_the_commands_for_the_name() {
        let w = Accounts { name: "scout".into(), owner: "ann".into(), scroll: Scroll::default() };
        let c = w.commands();
        assert_eq!(c[0], "sudo sysadminctl -addUser scout -fullName \"scout agent\" -shell /bin/zsh -home /Users/scout -password - interactive");
        assert!(!c.iter().any(|l| l.contains("sudoers") || l.contains("NOPASSWD")), "the commands to copy carry no sudoers line: {c:?}");
        let [line, check] = w.sudoers();
        assert_eq!(line, "printf 'ann ALL=(scout) NOPASSWD: ALL\\n' | sudo tee /etc/sudoers.d/nerd-scout");
        assert_eq!(check, "sudo visudo -c -f /etc/sudoers.d/nerd-scout");
        let shown: Vec<String> = w.lines(&[], &Theme::dark()).iter().map(|l| l.iter().map(|(t, _, _)| t.as_str()).collect()).collect();
        let at = |s: &str| shown.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("{s}: {shown:#?}"));
        assert!(at("asks for your password (sudo) each time it starts") < at("Optional, less safe") && at("Optional, less safe") < at("NOPASSWD"), "{shown:#?}");
        assert_eq!(shown[at("NOPASSWD") + 2], "with this line: any program running as you can act as scout at any time, without a password", "{shown:#?}");
        assert!(shown[at("NOPASSWD") + 3].starts_with("without it: sudo asks for your password each time a pane of scout starts"), "{shown:#?}");
        assert!(at("each time it starts") < at("only root can change") && at("only root can change") < at("sudo install -d -o root -g wheel -m 755 \"/Library/Application Support/nerd/bin\""), "{shown:#?}");
        assert!(shown[at("/nerd/bin/nerd")].ends_with(&format!(" {} \"/Library/Application Support/nerd/bin/nerd\"", std::env::current_exe().unwrap().display())), "{shown:#?}");
        assert!(shown.iter().any(|l| l == "Until that copy matches the running nerd, panes of the account refuse to start."), "{shown:#?}");
        assert!(w.pf().ends_with("user scout' | sudo tee /etc/pf.anchors/nerd-scout"));
        let mut w = w;
        let accounts = [Account { name: "scout".into(), uid: 502 }];
        assert_eq!(w.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &accounts), Out::Remove("scout".into()));
        assert_eq!(w.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &[]), Out::None, "nothing admitted under that name");
    }

    /// The frame and the rows inside it, as drawn on a 160 × 45 screen.
    fn inside<H: Clone>(draw: impl Fn(&mut Buffer, Rect) -> Drawn<H>) -> (Drawn<H>, Vec<String>) {
        let screen = Rect::new(0, 0, 160, 45);
        let mut buf = Buffer::empty(screen);
        let d = draw(&mut buf, screen);
        let rows = (d.frame.y..d.frame.bottom()).map(|y| (d.frame.x..d.frame.right()).map(|x| buf[(x, y)].symbol()).collect()).collect();
        (d, rows)
    }

    #[test]
    fn isolation_wraps_its_text_and_its_chips_and_every_chip_keeps_its_click() {
        let names = ["claude", "codex", "qwen", "gemini", "cursor", "copilot", "opencode", "pi", "another-agent", "and-one-more"];
        let w = IsolationWin { providers: names.map(String::from).to_vec(), home: true, pick: Some(Isolation { files: Files::Folder, network: Network::Open }), ..pane() };
        let (d, rows) = inside(|b, s| w.draw(b, s, &Theme::dark()));
        assert_eq!(d.frame.width, crate::chrome::WIDE_W);
        for p in names {
            assert!(d.hits.iter().any(|(_, c)| *c == Click::Provider(p.into())), "{p} has a click: {rows:#?}");
        }
        let chips: Vec<&String> = rows.iter().filter(|r| r.contains(" claude ") || r.contains(" and-one-more ")).collect();
        assert_eq!(chips.len(), 2, "the first and the last line of chips: {rows:#?}");
        let col = |r: &str, at: usize| r[..at].chars().count();
        let next = chips[1].char_indices().skip(1).find(|(_, c)| !c.is_whitespace()).map(|(i, _)| col(chips[1], i));
        assert_eq!(next, chips[0].find("claude").map(|i| col(chips[0], i)), "the second line of chips starts under the first chip: {rows:#?}");
        let at = rows.iter().position(|r| r.contains("though not dot files, ~/Library or ~/bin;")).unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(rows[at + 1].starts_with("│ limit. "), "the long note wraps, nothing cut: {rows:#?}");
    }

    #[test]
    fn accounts_wraps_a_command_under_its_indent() {
        let w = Accounts { name: "scout".into(), owner: "ann".into(), scroll: Scroll::default() };
        let (d, rows) = inside(|b, s| w.draw(b, s, &Theme::dark(), &[]));
        assert_eq!(d.frame.width, crate::chrome::WIDE_W);
        let at = rows.iter().position(|r| r.contains("sudo sysadminctl -addUser scout")).unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(rows[at + 1].starts_with("│   ") && rows[at + 1].contains("interactive"), "the rest of it under the two spaces: {rows:#?}");
        let at = rows.iter().position(|r| r.contains("act as scout at any time,")).unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(rows[at + 1].starts_with("│ without a password "), "prose wraps, nothing cut: {rows:#?}");
    }
}
