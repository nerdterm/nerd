//! Notifications for unfocused panes that went into `blocked` or `done`. All local:
//! a sound via `afplay`, a ring as a BEL byte. No system notification: a tray of them
//! goes unread, and the states are in the sidebar already.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::config::Notify;
use crate::term::AgentState;

const DEBOUNCE: Duration = Duration::from_secs(2);

/// A trait so tests can capture calls without touching the real afplay.
trait Sink {
    fn run(&mut self, cmd: &str, args: &[String]);
}

struct RealSink;

impl Sink for RealSink {
    fn run(&mut self, cmd: &str, args: &[String]) {
        // A launch error (missing binary, etc.) must not crash or hang the multiplexer.
        let _ = crate::spawn::detached(crate::spawn::cmd(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()));
    }
}

pub struct Notifier {
    last: HashMap<usize, AgentState>,
    fired_at: HashMap<usize, Instant>,
    sink: Box<dyn Sink + Send>,
}

impl Notifier {
    pub fn new() -> Self {
        Self { last: HashMap::new(), fired_at: HashMap::new(), sink: Box::new(RealSink) }
    }

    #[cfg(test)]
    fn with_sink(sink: impl Sink + Send + 'static) -> Self {
        Self { last: HashMap::new(), fired_at: HashMap::new(), sink: Box::new(sink) }
    }

    /// Fires only on a transition into `Blocked`/`Done` while unfocused; the pane's
    /// first-ever observation is never a transition.
    pub fn observe(&mut self, pane: usize, state: AgentState, focused: bool, cfg: &Notify) -> Vec<u8> {
        let prev = self.last.insert(pane, state);

        let is_transition = matches!(state, AgentState::Blocked | AgentState::Done | AgentState::Failed)
            && prev.is_some_and(|p| p != state);

        if !is_transition || focused {
            return Vec::new();
        }

        let now = Instant::now();
        let debounced = self.fired_at.get(&pane).is_some_and(|t| now.duration_since(*t) < DEBOUNCE);
        if debounced {
            return Vec::new();
        }
        self.fired_at.insert(pane, now);

        if cfg.sound {
            let sound = if state == AgentState::Done { "Glass" } else { "Basso" };
            self.sink.run("afplay", &[format!("/System/Library/Sounds/{sound}.aiff")]);
        }

        if cfg.bell { b"\x07".to_vec() } else { Vec::new() }
    }

    pub fn forget(&mut self, pane: usize) {
        self.last.remove(&pane);
        self.fired_at.remove(&pane);
    }

    /// Drops entries for panes that no longer exist, so the maps don't grow forever.
    pub fn prune(&mut self, live: &[usize]) {
        let gone: Vec<usize> = self.last.keys().copied().filter(|id| !live.contains(id)).collect();
        for id in gone {
            self.forget(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Call = (String, Vec<String>);

    #[derive(Clone, Default)]
    struct Calls(Arc<Mutex<Vec<Call>>>);

    impl Sink for Calls {
        fn run(&mut self, cmd: &str, args: &[String]) {
            self.0.lock().unwrap().push((cmd.to_string(), args.to_vec()));
        }
    }

    fn cfg_all() -> Notify {
        Notify { sound: true, bell: true }
    }

    #[test]
    fn first_observation_never_fires() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        let bytes = n.observe(1, AgentState::Blocked, false, &cfg_all());
        assert!(bytes.is_empty());
        assert!(calls.0.lock().unwrap().is_empty());
    }

    #[test]
    fn unfocused_pane_fires_once_on_transition_to_blocked() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, false, &cfg_all());
        let bytes = n.observe(1, AgentState::Blocked, false, &cfg_all());
        assert_eq!(bytes, b"\x07".to_vec());
        assert_eq!(calls.0.lock().unwrap().len(), 1); // afplay

        // repeating the same state doesn't re-fire the notification
        calls.0.lock().unwrap().clear();
        let bytes = n.observe(1, AgentState::Blocked, false, &cfg_all());
        assert!(bytes.is_empty());
        assert!(calls.0.lock().unwrap().is_empty());
    }

    #[test]
    fn focused_pane_never_fires() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, true, &cfg_all());
        let bytes = n.observe(1, AgentState::Blocked, true, &cfg_all());
        assert!(bytes.is_empty());
        assert!(calls.0.lock().unwrap().is_empty());
    }

    #[test]
    fn done_transition_fires() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, false, &cfg_all());
        n.observe(1, AgentState::Done, false, &cfg_all());
        assert_eq!(calls.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn bell_byte_only_when_configured() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls);
        n.observe(1, AgentState::Working, false, &Notify { sound: false, bell: false });
        let bytes = n.observe(1, AgentState::Blocked, false, &Notify { sound: false, bell: false });
        assert!(bytes.is_empty());
    }

    #[test]
    fn only_the_sound_leaves_the_process() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, false, &cfg_all());
        n.observe(1, AgentState::Blocked, false, &cfg_all());
        let calls = calls.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "afplay");
        assert_eq!(calls[0].1, vec!["/System/Library/Sounds/Basso.aiff".to_string()]);
    }

    #[test]
    fn debounce_suppresses_flapping_within_two_seconds() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, false, &cfg_all());
        n.observe(1, AgentState::Blocked, false, &cfg_all());
        // flapping back and forth within the debounce window shouldn't re-fire
        n.observe(1, AgentState::Working, false, &cfg_all());
        let bytes = n.observe(1, AgentState::Blocked, false, &cfg_all());
        assert!(bytes.is_empty());
        assert_eq!(calls.0.lock().unwrap().len(), 1); // only from the first trigger
    }

    #[test]
    fn forget_drops_pane_state() {
        let calls = Calls::default();
        let mut n = Notifier::with_sink(calls.clone());
        n.observe(1, AgentState::Working, false, &cfg_all());
        n.forget(1);
        // after forget, the pane is like freshly seen: the transition doesn't count
        let bytes = n.observe(1, AgentState::Blocked, false, &cfg_all());
        assert!(bytes.is_empty());
    }
}
