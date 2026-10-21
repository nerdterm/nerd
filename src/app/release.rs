use super::*;
use crate::release::{Found, Said};

/// How often the server looks whether the daily release check is due.
pub const RELEASE_EVERY: Duration = Duration::from_secs(600);

const TITLE: &str = "Check for a new release";
const ASKING: &str = "asking GitHub for the newest release…";
const DOWNLOADING: &str = "downloading the release…";

/// Who asked for the release run: each is answered its own way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReleaseAsk {
    /// Check for a new release…: its window shows the answer.
    Menu,
    /// Once a day, after the person allowed it: a card, only for a newer release they did not skip.
    Daily,
    /// The window's install button.
    Install,
}

impl App {
    /// Menu > Check for a new release…: GitHub is asked through the broker, outside the server's sandbox.
    pub(super) fn check_release(&mut self) -> Overlay {
        self.release_start(ReleaseAsk::Menu);
        Overlay::None
    }

    fn release_start(&mut self, ask: ReleaseAsk) {
        self.dirty = true;
        if self.release_job.is_some() {
            if ask != ReleaseAsk::Daily {
                self.notice = Some("nerd is already asking GitHub; the answer comes here".into());
            }
            return;
        }
        match self.release_exec(ask == ReleaseAsk::Install) {
            Ok(id) => {
                self.release_job = Some((id, ask));
                match ask {
                    ReleaseAsk::Menu => self.toast = Some(ASKING.into()),
                    ReleaseAsk::Install => self.toast = Some(DOWNLOADING.into()),
                    ReleaseAsk::Daily => {}
                }
            }
            Err(e) if ask == ReleaseAsk::Daily => crate::log::warn(&format!("daily release check not started: {e}")),
            Err(e) => self.notice = Some(e),
        }
    }

    /// `nerd --check-update --window`, run by the broker outside the sandbox with no tab and no terminal; it leaves
    /// its answer in a file.
    fn release_exec(&mut self, install: bool) -> Result<usize, String> {
        let exec = self.plugins.take_id();
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "nerd".into());
        let mut argv = vec![exe, "--check-update".to_string()];
        if install {
            argv.push("--install".into());
        }
        argv.push("--window".into());
        match (crate::broker::handle(), crate::broker::exec_handle()) {
            (Some(_), _) if crate::broker::exec_coming() => Err("nerd's helper is starting; try again in a moment".to_string()),
            (Some(_), None) => Err("this server's helper is from an older build: restart the server to check for releases".into()),
            (Some(_), Some(b)) => b.exec(exec, argv, vec![("NERD_INSTANCE".into(), crate::session::instance_name())], crate::paths::home().unwrap_or_default()).map_err(|e| format!("the broker did not take it: {e}")),
            // Tests run with no broker: the job stays as started.
            (None, _) if cfg!(test) => Ok(()),
            (None, _) => Err("no broker to ask GitHub through".into()),
        }?;
        Ok(exec)
    }

    /// The broker's word on a process it ran; true when it was the release run.
    pub(super) fn release_note(&mut self, n: &crate::broker::ExecNote) -> bool {
        use crate::broker::ExecNote;
        let (ExecNote::Started { id, .. } | ExecNote::Failed { id, .. } | ExecNote::Exited { id, .. }) = n;
        let Some((_, ask)) = self.release_job.filter(|(j, _)| j == id) else { return false };
        let said = match n {
            ExecNote::Started { .. } => return true,
            ExecNote::Failed { message, .. } => Said::Failed(format!("the check did not start: {message}")),
            ExecNote::Exited { .. } => crate::release::take_said(),
        };
        self.release_job = None;
        self.release_said(said, ask);
        true
    }

    fn release_said(&mut self, said: Said, ask: ReleaseAsk) {
        self.dirty = true;
        // The run's own card is answered now; anything said since stays.
        if self.toast.as_deref().is_some_and(|t| t == ASKING || t == DOWNLOADING) {
            self.toast = None;
        }
        match said {
            // The run logged it; a check nobody asked for just now does not interrupt.
            Said::Failed(_) if ask == ReleaseAsk::Daily => {}
            Said::Failed(e) => self.notice = Some(e),
            Said::Installed(v) => {
                self.release_found = None;
                self.toast = Some(format!("{v} installed in ~/.local/bin"));
            }
            Said::Found(f) => {
                // Kept while it is news: the menu item and the menu button stay marked until it is installed or skipped.
                let news = f.newer && crate::release::skipped().as_deref() != Some(f.tag.as_str());
                self.release_found = news.then(|| f.clone());
                let version = f.tag.trim_start_matches('v');
                match ask {
                    ReleaseAsk::Daily if news => self.toast_with_more(format!("nerd {version} is out"), More::Release),
                    ReleaseAsk::Daily => {}
                    _ if self.overlay.is_open() => self.toast_with_more(format!("newest release: nerd {version}"), More::Release),
                    _ => self.overlay = self.release_window(f),
                }
            }
        }
    }

    /// The running build, the newest release and its changelog; install only when this nerd may replace itself.
    pub(super) fn release_window(&mut self, f: Found) -> Overlay {
        use crate::menu::Confirm;
        self.dirty = true;
        let (running, newest) = (crate::version_line(), format!("nerd {}", f.tag.trim_start_matches('v')));
        if !f.newer {
            let body = format!("Nothing newer: the newest release is {newest}, and {running} runs here.");
            return Overlay::Confirm(Confirm::new(TITLE, body).info(), Pending::Dismiss);
        }
        let rows = f.notes.iter().map(|l| vec![l.clone()]).collect();
        let said = format!("{newest} is out; {running} runs here.");
        if let Some(b) = &f.blocked {
            let c = Confirm::new(TITLE, format!("{said} {b}.")).rows(rows).info().other(" [s] skip this release ").click_yes().select(0);
            return Overlay::Confirm(c, Pending::ReleaseInstall(f.tag));
        }
        let body = format!("{said} Install downloads it from GitHub, checks it against the checksum published with it and puts it in ~/.local/bin; Update available then swaps the running servers for it.");
        let c = Confirm::new(TITLE, body).rows(rows).labels(" [Enter] install ", " [Esc] later ").other(" [s] skip this release ").click_yes();
        Overlay::Confirm(c, Pending::ReleaseInstall(f.tag))
    }

    /// The window's install button: only from nerd's own window on the server's Mac.
    pub(super) fn install_release(&mut self, tag: String) {
        if self.client_inside {
            self.notice = Some(super::mail::INSIDE_REFUSED.into());
            return;
        }
        if self.caps.bye {
            self.notice = Some("a release installs on the Mac the server runs on: run nerd --check-update in a terminal there".into());
            return;
        }
        if self.release_job.is_some() {
            self.notice = Some("nerd is already asking GitHub; the answer comes here".into());
            return;
        }
        if let Err(e) = crate::release::allow_install(&tag) {
            self.notice = Some(format!("the release was not installed: {e}"));
            return;
        }
        crate::log::info(&format!("release {tag}: install asked from the window"));
        self.release_start(ReleaseAsk::Install);
    }

    pub(super) fn skip_release(&mut self, tag: &str) {
        self.release_found = None;
        match crate::release::skip(tag) {
            Ok(()) => self.toast = Some(format!("nerd {} skipped: the daily check says nothing more about it", tag.trim_start_matches('v'))),
            Err(e) => self.notice = Some(format!("not skipped: {e}")),
        }
    }

    /// The first-start question and Settings write the per-Mac answer, only from nerd's own window on that Mac.
    pub(super) fn set_release_check(&mut self, daily: bool) -> bool {
        if self.client_inside {
            self.notice = Some(super::mail::INSIDE_REFUSED.into());
            return false;
        }
        if self.caps.bye {
            self.notice = Some("this answer belongs to the Mac the server runs on: change it in a nerd window there".into());
            return false;
        }
        match crate::release::set_daily(daily) {
            Ok(()) => {
                crate::log::info(&format!("release check set to {}", if daily { "daily" } else { "off" }));
                // A yes is answered by a check at the next tick, not ten minutes on.
                self.release_at = Instant::now() - RELEASE_EVERY;
                self.dirty = true;
                true
            }
            Err(e) => {
                self.notice = Some(format!("the answer was not saved ({e}); nerd asks again next time"));
                false
            }
        }
    }

    /// Asked on a window's attach, once what that window showed first is closed, until it is answered once on this
    /// Mac; Esc leaves it for the next window.
    pub fn offer_release_check(&mut self) {
        self.release_ask = self.overlay.is_open();
        if self.release_ask || self.client_inside || self.caps.bye || self.cfg_broken || crate::release::daily().is_some() {
            return;
        }
        let body = "Should nerd ask GitHub once a day whether a new release is out? It only tells you: installing waits for your click. Every check is logged, and Settings changes this answer.";
        let c = crate::menu::Confirm::new("Check for new releases", body).labels(" [Enter] yes, daily ", " [Esc] ask later ").other(" [d] don't check ").click_yes();
        self.overlay = Overlay::Confirm(c, Pending::ReleaseDaily);
        self.dirty = true;
    }

    /// The server's tick: a check once a day on this Mac, if the person allowed it.
    pub fn tick_release(&mut self) {
        if self.release_ask && !self.overlay.is_open() {
            self.offer_release_check();
        }
        // Not under an open release window: a run in flight would refuse its install button.
        let window = matches!(self.overlay, Overlay::Confirm(_, Pending::ReleaseInstall(_)));
        if self.release_at.elapsed() < RELEASE_EVERY || self.release_job.is_some() || window {
            return;
        }
        self.release_at = Instant::now();
        if crate::release::daily_due(crate::clients::now()) {
            self.release_start(ReleaseAsk::Daily);
        }
    }
}
