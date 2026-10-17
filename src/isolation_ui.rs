//! The Isolation window (files and network for a pane, or a tab's or space's new panes) and the Accounts window.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::IsolationTarget;
use crate::messages::{Drawn, Line, chip};
use crate::policy::{Account, Files, Isolation, Network};
use crate::render::Theme;

#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// No isolation of its own: as the space, or as nerd's default.
    Inherit,
    Files(Files),
    Network(Network),
    /// A provider's chip: on or off among the chosen.
    Provider(String),
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
    /// The ones "only these providers" means until others are chosen.
    pub chosen: Vec<String>,
    /// `<title>: <how to turn its own sandbox off>`, one per agent that says.
    pub notes: Vec<String>,
    /// The pane's folder (a tab's or space's: the space's) is the home folder itself.
    pub home: bool,
}

const CANCEL: &str = " [Esc] cancel ";
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
            let mut l = radio(provider, "Only these providers: ".into(), Some(Click::Network(Network::Providers(self.chosen.clone()))));
            for p in &self.providers {
                let lit = (provider && self.chosen.contains(p)).then(|| theme.cursor());
                chip(&mut l, p, lit, Some(Click::Provider(p.clone())), theme);
            }
            out.push(l);
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

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn<Click> {
        let lines = self.lines(theme);
        let buttons = self.buttons();
        let bottom = buttons.iter().map(|(b, _)| b.chars().count() + 1).sum::<usize>() + CANCEL.len();
        let r = crate::messages::fit(screen, &lines, bottom);
        crate::messages::draw(buf, r, self.title(), theme, &lines, 0, &buttons, CANCEL)
    }

    fn click(&mut self, c: Click) -> Out {
        match c {
            Click::Inherit => self.pick = None,
            Click::Files(f) => self.pick.get_or_insert_default().files = f,
            Click::Network(n) => self.pick.get_or_insert_default().network = n,
            Click::Provider(p) => {
                let on = matches!(self.pick.as_ref().map(|i| &i.network), Some(Network::Providers(_)));
                match self.chosen.iter().position(|c| *c == p) {
                    _ if !on => self.chosen = vec![p],
                    Some(i) if self.chosen.len() > 1 => _ = self.chosen.remove(i),
                    Some(_) => {}
                    None => self.chosen.push(p),
                }
                self.pick.get_or_insert_default().network = Network::Providers(self.chosen.clone());
            }
            Click::Save(restart) => return Out::Save(self.pick.clone(), restart),
            _ => {}
        }
        Out::None
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Out {
        use crossterm::event::KeyCode::*;
        match crate::keys::normalize(k).code {
            Enter => self.click(Click::Save(false)),
            Char('r') if self.inherit.is_none() => self.click(Click::Save(true)),
            Esc => Out::Close,
            _ => Out::None,
        }
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        match pressed(ev, d) {
            Err(out) => out,
            Ok(Some(c)) => self.click(c),
            Ok(None) => Out::None,
        }
    }
}

/// Accounts…: the admitted accounts, and the commands that make one.
#[derive(Clone, Debug)]
pub struct Accounts {
    /// The account the commands are written for.
    pub name: String,
    owner: String,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts { name: "agent".into(), owner: std::env::var("USER").unwrap_or_else(|_| "you".into()) }
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
        let lines = self.lines(accounts, theme);
        let buttons = Self::buttons();
        let bottom = buttons.iter().map(|(b, _)| b.chars().count() + 1).sum::<usize>() + crate::messages::CLOSE.len();
        let r = crate::messages::fit(screen, &lines, bottom);
        crate::messages::draw(buf, r, "Accounts", theme, &lines, 0, &buttons, crate::messages::CLOSE)
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
            _ => Out::None,
        }
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, d: &Drawn<Click>) -> Out {
        match pressed(ev, d) {
            Err(out) => out,
            Ok(Some(c)) => self.click(c),
            Ok(None) => Out::None,
        }
    }
}

#[cfg(test)]
mod tests {
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
            chosen: vec!["codex".into()],
            notes: vec!["Codex: run it with --sandbox danger-full-access".into()],
            home: false,
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
        let w = Accounts { name: "scout".into(), owner: "ann".into() };
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
}
