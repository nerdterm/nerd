//! The agent hook: an agent reports its own state, instead of us guessing from the screen.
//! Writes one word to `<state>/agents/<pane>`, the running subagents to `<pane>.subagents` and
//! tool calls to `<pane>.events` (dir 0700, files 0600); nothing leaves the machine.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::term::AgentState;

pub const SCRIPT_NAME: &str = "nerd-agent-state.sh";

/// Where the hook writes states; panes get the path via `NERD_STATE`.
pub fn state_dir() -> PathBuf {
    crate::session::state_dir().join("agents")
}

fn state_file(pane: usize) -> PathBuf {
    state_dir().join(pane.to_string())
}

/// State written by the hook. No file, or an unrecognized word, yields None,
/// and then ordinary output parsing takes over.
pub fn read_state(pane: usize) -> Option<AgentState> {
    let s = std::fs::read_to_string(state_file(pane)).ok()?;
    parse_state(&s)
}

fn parse_state(s: &str) -> Option<AgentState> {
    match s.trim() {
        "working" => Some(AgentState::Working),
        "idle" => Some(AgentState::Idle),
        "blocked" => Some(AgentState::Blocked),
        "failed" => Some(AgentState::Failed),
        _ => None,
    }
}

/// The pane's agent and its session id, only when the id has that agent's shape: it becomes an argument.
pub fn read_session(pane: usize, agent: &str) -> Option<(String, String)> {
    let s = std::fs::read_to_string(state_dir().join(format!("{pane}.session"))).ok()?;
    let s = s.trim();
    crate::agents::get(agent)?.is_id(s).then(|| (agent.to_string(), s.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Running,
    Ok,
    Failed,
    /// Never answered: denied, interrupted, or its agent moved on.
    Dropped,
}

/// One tool call as the hook saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub tool: String,
    pub target: String,
    /// Unix seconds.
    pub at: u64,
    pub outcome: Outcome,
    /// One line of what came back, when the hook caught one.
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sub {
    pub id: String,
    pub kind: String,
    pub task: String,
    pub started: u64,
    /// Its latest event, or its start.
    pub last: u64,
    pub stopped: Option<u64>,
    /// Oldest first.
    pub calls: Vec<Call>,
    /// The pane's agent has ended its turn: nothing it does waits on this one.
    pub alone: bool,
}

/// How a subagent's row reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubState {
    Running,
    Done,
    Failed,
    /// Silent with nothing in flight while its agent's turn is over: its stop was lost.
    Unknown,
}

/// A finished or failed subagent keeps its row this long.
pub const LINGER: u64 = 30;
/// A subagent silent this long reads as stuck.
pub const QUIET: u64 = 120;
/// Silent this long after its agent ended the turn: its stop was lost (a subagent stopped from outside sends none).
pub const ORPHAN: u64 = 60;
/// Quiet this long with nothing in flight after its agent's turn: taken for lost, off the sidebar.
pub const LOST: u64 = 600;

impl Sub {
    pub fn state(&self, now: u64) -> SubState {
        if self.stopped.is_some() {
            SubState::Done
        } else if self.calls.last().is_some_and(|c| c.outcome == Outcome::Failed) && now.saturating_sub(self.last) >= LINGER {
            SubState::Failed
        } else if self.silent(now, ORPHAN) || self.quiet(now, LOST) {
            SubState::Unknown
        } else {
            SubState::Running
        }
    }

    /// Still in the sidebar: running, or finished moments ago.
    pub fn shown(&self, now: u64) -> bool {
        self.stopped.is_none_or(|t| now.saturating_sub(t) < LINGER) && !self.quiet(now, LOST)
    }

    /// Not stopped, nothing in flight, and quiet `secs` since its agent ended the turn.
    fn silent(&self, now: u64, secs: u64) -> bool {
        self.alone && self.quiet(now, secs)
    }

    /// Not stopped, nothing in flight, and quiet `secs`, whatever its agent is doing.
    fn quiet(&self, now: u64, secs: u64) -> bool {
        self.stopped.is_none() && now.saturating_sub(self.last) >= secs && self.calls.last().is_none_or(|c| c.outcome != Outcome::Running)
    }

    pub fn stale(&self, now: u64) -> bool {
        self.stopped.is_none() && now.saturating_sub(self.last) >= QUIET
    }
}

/// What the pane's agent and its subagents are doing, from the hook's `.events` and `.subagents`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    /// The main thread's call in flight, else its last one.
    pub main: Option<Call>,
    /// Running ones and finished ones still in the log, by start.
    pub subs: Vec<Sub>,
    /// The log was cut: counts are no longer whole.
    pub cut: bool,
}

impl Activity {
    pub fn live(&self, now: u64) -> bool {
        self.subs.iter().any(|s| s.stopped.is_none() && !s.quiet(now, LOST) && !s.silent(now, ORPHAN))
    }

    /// A start the files do not give (a line an older hook wrote) is when nerd first saw the id, never epoch 0.
    pub fn carry_starts(&mut self, before: &Activity, now: u64) {
        for s in self.subs.iter_mut().filter(|s| s.started == 0) {
            s.started = before.subs.iter().find(|b| b.id == s.id && b.started > 0).map_or(now, |b| b.started);
            s.last = s.last.max(s.started);
        }
        self.subs.sort_by_key(|s| s.started);
    }

    /// The newest call in flight: what a permission prompt is most likely about.
    pub fn asking(&self) -> Option<&str> {
        let live = self.subs.iter().filter(|s| s.stopped.is_none()).filter_map(|s| s.calls.last());
        self.main.iter().chain(live).filter(|c| c.outcome == Outcome::Running).max_by_key(|c| c.at).map(|c| c.tool.as_str())
    }
}

/// Unix seconds, the hook's clock.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn read_activity(pane: usize) -> Activity {
    let read = |suffix: &str| {
        std::fs::read(state_dir().join(format!("{pane}{suffix}"))).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    };
    parse_activity(&read(".events"), &read(".subagents"))
}

/// Changes whenever either file does, so an unchanged pane is not parsed again.
pub fn activity_key(pane: usize) -> [Option<(std::time::SystemTime, u64)>; 2] {
    [".events", ".subagents"].map(|s| {
        let m = std::fs::metadata(state_dir().join(format!("{pane}{s}"))).ok()?;
        Some((m.modified().ok()?, m.len()))
    })
}

fn clean(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

/// `.subagents`: `id<TAB>type<TAB>task<TAB>start`. `.events`: `epoch<TAB>agent<TAB>what...`, where agent
/// `-` is the main thread. A line with the wrong field count is skipped.
pub(crate) fn parse_activity(events: &str, subagents: &str) -> Activity {
    use std::collections::HashMap;
    let num = |s: Option<&&str>| s.and_then(|s| s.parse::<u64>().ok());
    let mut subs: Vec<Sub> = subagents
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if !(2..=4).contains(&f.len()) || f[0].is_empty() {
                return None;
            }
            let started = num(f.get(3)).unwrap_or(0);
            Some(Sub { id: f[0].into(), kind: clean(f[1], 32), task: clean(f.get(2).unwrap_or(&""), 120), started, last: started, ..Sub::default() })
        })
        .take(16)
        .collect();
    let listed: Vec<String> = subs.iter().map(|s| s.id.clone()).collect();
    let mut typed: Vec<&str> = Vec::new();
    let mut calls: HashMap<&str, Vec<Call>> = HashMap::new();
    let mut last: HashMap<&str, u64> = HashMap::new();
    // a subagent's type from its calls, and which line last stopped it and last started a call
    let mut types: HashMap<&str, &str> = HashMap::new();
    let (mut stopped_at, mut heard_at): (HashMap<&str, usize>, HashMap<&str, usize>) = Default::default();
    // when each subagent last handed back its report
    let mut handed: HashMap<&str, u64> = HashMap::new();
    let (mut cut, mut idle) = (false, false);
    for (n, l) in events.lines().enumerate() {
        let f: Vec<&str> = l.split('\t').collect();
        let (Some(at), Some(&who)) = (num(f.first()), f.get(1)) else { continue };
        match (f.get(2).copied().unwrap_or(""), f.len()) {
            ("start", 6 | 7) => {
                if let Some(t) = f.get(6).filter(|t| !t.is_empty()) {
                    types.insert(who, t);
                }
                heard_at.insert(who, n);
                idle &= who != "-";
                let list = calls.entry(who).or_default();
                // only the latest start is in flight: an earlier one went unanswered
                for c in list.iter_mut().filter(|c| c.outcome == Outcome::Running) {
                    c.outcome = Outcome::Dropped;
                }
                // Claude Code's subagent hands back its report as its last call: no result or stop may follow
                let report = who != "-" && f[4] == "SubagentHandback";
                if report {
                    stopped_at.insert(who, n);
                    handed.insert(who, at);
                }
                let outcome = if report { Outcome::Ok } else { Outcome::Running };
                list.push(Call { id: f[3].into(), tool: clean(f[4], 32), target: clean(f[5], 120), at, outcome, summary: String::new() });
            }
            (end @ ("ok" | "fail"), 4 | 5) => {
                // an agent without call ids (Qwen's results, Gemini, Copilot) answers its latest call in flight
                let answers = |c: &Call| if f[3].is_empty() { c.outcome == Outcome::Running } else { c.id == f[3] };
                if let Some(c) = calls.entry(who).or_default().iter_mut().rev().find(|c| answers(c)) {
                    c.outcome = if end == "ok" { Outcome::Ok } else { Outcome::Failed };
                    // the shell cut bytes: a character split at the end is dropped, not shown as a replacement mark
                    c.summary = clean(f.get(4).unwrap_or(&""), 120).trim_end_matches(char::REPLACEMENT_CHARACTER).trim().to_string();
                }
            }
            ("stop", 3..=6) => {
                stopped_at.insert(who, n);
                for c in calls.entry(who).or_default().iter_mut().filter(|c| c.outcome == Outcome::Running) {
                    c.outcome = Outcome::Dropped;
                }
                match subs.iter_mut().find(|s| s.id == who) {
                    Some(s) => s.stopped = Some(at),
                    None => {
                        if f.get(3).is_some_and(|k| !k.is_empty()) {
                            typed.push(who);
                        }
                        subs.push(Sub {
                        id: who.into(),
                        kind: clean(f.get(3).filter(|k| !k.is_empty()).unwrap_or(&"agent"), 32),
                        task: clean(f.get(4).unwrap_or(&""), 120),
                        started: num(f.get(5)).unwrap_or(0),
                        stopped: Some(at),
                        ..Sub::default()
                    })
                    }
                }
            }
            // a new prompt or the end of a turn: nothing of the main thread is in flight any more
            // how the turn went, on a line of its own that an older nerd skips
            ("end", 4) if who == "-" => idle = matches!(f[3], "idle" | "failed"),
            ("turn", 3) if who == "-" => {
                for c in calls.entry(who).or_default().iter_mut().filter(|c| c.outcome == Outcome::Running) {
                    c.outcome = Outcome::Dropped;
                }
            }
            ("cut", 3) => cut = true,
            _ => continue,
        }
        last.insert(who, at);
    }
    // calls from a subagent nerd never saw start: it was resumed; it gets its row from them
    let mut strays: Vec<&str> = calls.keys().copied().filter(|w| *w != "-" && !subs.iter().any(|s| s.id == *w)).collect();
    strays.sort();
    for w in strays {
        subs.push(Sub { id: w.into(), kind: "agent".into(), ..Sub::default() });
    }
    for s in &mut subs {
        let id = s.id.as_str();
        // a call started after its last stop: resumed, and running again
        if heard_at.get(id) > stopped_at.get(id) && stopped_at.contains_key(id) {
            s.stopped = None;
        }
        if heard_at.get(id) == stopped_at.get(id) && let Some(&t) = handed.get(id) {
            s.stopped = Some(t);
        }
        s.alone = idle;
        if s.kind == "agent" && let Some(t) = types.get(id) {
            s.kind = clean(t, 32);
        }
        s.calls = calls.remove(s.id.as_str()).unwrap_or_default();
        if s.started == 0 {
            s.started = s.calls.first().map(|c| c.at).or(s.stopped).unwrap_or(0);
        }
        s.last = s.started.max(last.get(s.id.as_str()).copied().unwrap_or(0));
    }
    // a stop for an id that never started and never called a tool is Claude Code's own
    // housekeeping, which stops an agent every half minute: no row for it
    subs.retain(|s| !s.calls.is_empty() || listed.contains(&s.id) || typed.contains(&s.id.as_str()));
    subs.sort_by_key(|s| s.started);
    Activity { main: calls.remove("-").and_then(|mut v| v.pop()), subs, cut }
}

/// The agent is gone or the pane was set up again: the old word no longer applies to it.
pub fn write_state(pane: usize, word: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir())?;
    crate::paths::write_private(&state_file(pane), "plugin", word.as_bytes())
}

pub fn clear_state(pane: usize) {
    for suffix in ["", ".session", ".subagents", ".tasks", ".events", ".inbox"] {
        let _ = std::fs::remove_file(state_dir().join(format!("{pane}{suffix}")));
    }
    let _ = std::fs::remove_dir_all(crate::mail::mailbox(pane));
}

/// Hooks installed by an older nerd get this build's script and entries: the owner already chose to have them.
pub fn refresh() -> std::io::Result<()> {
    refresh_in(&home())
}

fn refresh_in(home: &Path) -> std::io::Result<()> {
    for a in crate::agents::all().iter() {
        if installed(a, home) && !current(a, home) {
            // one agent's broken config must not keep the others on an old script
            let _ = install_one(a, home);
        }
    }
    Ok(())
}

const SCRIPT: &str = r#"#!/bin/sh
# nerd: an agent reports its pane's state, its subagents and its tool calls. Nothing leaves the
# machine, just small files under nerd's state directory, and only within nerd. One script serves
# every agent: each names its fields its own way, and the first name that has a value is used.
# $1 is what happened, $2 the agent when it is not Claude Code.
case "$1" in working|idle|blocked|fail|stop|notify|mail|task|sub-start|sub-stop|sub-reset|tool-start|tool-ok|tool-fail) ;; *) exit 0 ;; esac
# A CLI that gives its hooks a clean environment (Gemini) still runs inside the pane: an ancestor carries it.
# ps prints the command, then KEY=value pairs: a value runs until the next word that starts a pair.
envof() { printf '%s\n' "$e" | head -n 1 | awk -v k="$1" '{ n = split($0, t, / /); on = 0; v = ""
  for (i = 1; i <= n; i++) {
    if (t[i] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) { if (on) break; if (index(t[i], k "=") == 1) { on = 1; v = substr(t[i], length(k) + 2) } }
    else if (on) v = v " " t[i]
  }
  if (on) print v }'; }
if [ -z "$NERD_PANE" ] || { [ -z "$NERD_STATE" ] && [ -z "$NERD_MSOCK" ]; }; then
  p=$PPID; n=0
  while [ "$n" -lt 8 ] && [ "${p:-1}" -gt 1 ]; do
    e=$(ps -wwE -o command= -p "$p" 2>/dev/null)
    case "$e" in *" NERD_PANE="*)
      NERD_PANE=$(envof NERD_PANE); NERD_STATE=$(envof NERD_STATE); NERD_HOOK_DUMP=${NERD_HOOK_DUMP:-$(envof NERD_HOOK_DUMP)}
      NERD_MSOCK=${NERD_MSOCK:-$(envof NERD_MSOCK)}; NERD_BIN=${NERD_BIN:-$(envof NERD_BIN)}
      break ;;
    esac
    p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' '); n=$((n + 1))
  done
fi
# An isolated pane cannot write nerd's state directory: nerd runs this script for it, over its message socket.
[ -z "$NERD_STATE" ] && [ -n "$NERD_MSOCK" ] && case "$NERD_BIN" in /*) true ;; *) false ;; esac && exec "$NERD_BIN" --hook-relay "$1" "$2"
[ -n "$NERD_PANE" ] && [ -n "$NERD_STATE" ] || exit 0
# Writes go only under nerd's own state directory: a pane number, and an absolute path below the
# root this script was installed for, with no `..` and no control characters.
root=@ROOT@
case "$NERD_PANE" in *[!0-9]*) exit 0 ;; esac
case "$NERD_STATE" in "$root"*) ;; *) exit 0 ;; esac
case "$NERD_STATE" in *..*) exit 0 ;; esac
[ "$(printf '%s' "$NERD_STATE" | tr -d '[:cntrl:]')" = "$NERD_STATE" ] || exit 0
# bytes from here on: a capped value may end in half a letter, which grep and sed reject in UTF-8
export LC_ALL=C
from=${2:-claude}
case "$from" in *[!a-z0-9-]*) exit 0 ;; esac
umask 077
mkdir -p "$NERD_STATE" 2>/dev/null || exit 0
base="$NERD_STATE/$NERD_PANE"
# The JSON stays in a file: /bin/sh is quadratic on patterns over a large string and slow to print one.
in="$base.in.$$"
trap 'rm -f "$in"' EXIT
trap 'exit 0' HUP INT TERM
# a hook killed outright leaves its payload behind: older than ten minutes it is no live hook's
find "$NERD_STATE" -maxdepth 1 -name '*.in.*' -mmin +10 -exec rm -f {} + 2>/dev/null
cat > "$in" || exit 0
now=$(date +%s)
# NERD_HOOK_DUMP=<absolute dir> keeps every payload as it came, to be replayed in nerd's tests.
case "$NERD_HOOK_DUMP" in /*) mkdir -p "$NERD_HOOK_DUMP/$from" && cp "$in" "$NERD_HOOK_DUMP/$from/$now-$$-$1.json" ;; esac
# The first string field of that name in the JSON on stdin (or in $2), without jq; a tab can't split a line of ours.
# The closing quote is optional: a capped $2 cuts a long value off before it.
field() { if [ $# -gt 1 ]; then printf '%s' "$2"; else cat "$in"; fi | grep -o "\"$1\" *: *\"[^\"]*\"\{0,1\}" | head -n 1 | sed "s/^\"$1\" *: *\"//; s/\"\$//" | tr '\t' ' ' | head -c 65536; }
first() { for k in "$@"; do v=$(field "$k"); [ -n "$v" ] && { printf '%s' "$v"; return; }; done; }
word() { first "$@" | tr -cd 'A-Za-z0-9_.:-'; }
safe() { first "$@" | tr -cd 'A-Za-z0-9_-'; }
# Parallel subagents and tool calls write at once: edits take turns. A lock older than 2 s is a dead hook's.
lock() {
  if [ -d "$base.lock" ] && [ $((now - $(stat -f %m "$base.lock" 2>/dev/null || echo "$now"))) -gt 2 ]; then mv "$base.lock" "$base.lock.$$" 2>/dev/null && rmdir "$base.lock.$$"; fi
  n=0; while ! mkdir "$base.lock" 2>/dev/null; do n=$((n + 1)); [ "$n" -gt 100 ] && return 1; sleep 0.02; done; }
unlock() { rmdir "$base.lock" 2>/dev/null; }
# One line to the tool log; past 800 lines it keeps the last 400 and says it was cut.
event() {
  lock || return 0
  printf '%s\n' "$1" >> "$base.events"
  if [ "$(wc -l < "$base.events")" -gt 800 ]; then
    { printf '%s\t-\tcut\n' "$now"; tail -n 400 "$base.events"; } > "$base.events.tmp" && mv -f "$base.events.tmp" "$base.events"
  fi
  unlock; }
state() {
  printf '%s\n' "$1" > "$base.tmp" && mv -f "$base.tmp" "$base"; }
# The pane resumes its agent's session after a restart: the id from the agent's own field, never from
# a tool event (its payload is the model's), and only when every match agrees on one id of the agent's kind.
# A built-in agent's field and kind are written here; a user's agent's are read from nerd's own file, so
# adding one leaves this script as it is.
session() {
  case "$from" in
@SESSIONS@    *) set -- $(grep "^$from " "${root}@SESSIONS_FILE@" 2>/dev/null | head -n 1); sf=$2 sk=$3 ;;
  esac
  case "$sf" in ""|*[!A-Za-z0-9_]*) return 0 ;; esac
  sid=$(grep -o "\"$sf\" *: *\"[^\"]*\"" "$in" | sed 's/.*"\([^"]*\)"$/\1/' | sort -u)
  x='[0-9a-fA-F]'
  case "$sk$sid" in
    uuid$x$x$x$x$x$x$x$x-$x$x$x$x-$x$x$x$x-$x$x$x$x-$x$x$x$x$x$x$x$x$x$x$x$x) ;;
    word|word-*|word*[!A-Za-z0-9_.-]*) return 0 ;;
    word*) ;;
    *) return 0 ;;
  esac
  [ ${#sid} -le 128 ] || return 0
  printf '%s\n' "$sid" > "$base.session.tmp" && mv -f "$base.session.tmp" "$base.session"; }
agent=$(safe agent_id)
# A session's own inbox socket, from a built-in agent that names one; never its token.
case "$1" in idle|sub-reset)
  if [ "$(field hook_event_name)" = SessionStart ]; then
    on=""
    case "$from" in
@INBOX@    esac
    if [ -n "$on" ]; then
      case "$ie" in /*.sock) ;; *) ie="" ;; esac
      [ "$(printf '%s' "$ie" | tr -d '[:cntrl:]')" = "$ie" ] || ie=""
      if [ -n "$ie" ]; then printf '%s\n' "$ie" > "$base.inbox.$$" && mv -f "$base.inbox.$$" "$base.inbox"; else rm -f "$base.inbox"; fi
    fi
  fi ;;
esac
# A subagent resumed after its stop starts no SubagentStart: its next call brings its row back. Not a
# result: one landing just after its stop would bring back a subagent that is gone. A subagent's call names
# its type; Claude Code's own helper calls with an id and no type, and never stops, so it gets no row.
adopt() {
  [ -n "$agent" ] || return 0
  k=$(first agent_type); [ -n "$k" ] || return 0
  lock || return 0
  grep -q "^$agent	" "$base.subagents" 2>/dev/null || printf '%s\t%s\t\t%s\n' "$agent" "$k" "$now" >> "$base.subagents"
  unlock; }
substop() {
  id=$(safe agent_id subagent_id)
  kind=$(first agent_type subagent_type agentName); task=$(field task)
  lock || return 0
  line=""
  if [ -f "$base.subagents" ]; then
    if [ -n "$id" ]; then
      line=$(grep "^$id	" "$base.subagents" | head -n 1)
    else
      # no id the start gave (Cursor, Copilot): the oldest running one of that type, and task when known
      line=$(K="$kind" T="$task" awk -F '\t' '$2 == ENVIRON["K"] && (ENVIRON["T"] == "" || $3 == ENVIRON["T"]) { print; exit }' "$base.subagents")
      [ -n "$line" ] || line=$(K="$kind" awk -F '\t' '$2 == ENVIRON["K"] { print; exit }' "$base.subagents")
      id=${line%%	*}
    fi
    if [ -n "$id" ]; then
      grep -v "^$id	" "$base.subagents" > "$base.subagents.tmp"
      mv -f "$base.subagents.tmp" "$base.subagents"
    fi
  fi
  unlock
  [ -n "$id" ] || return 0
  # its type, task and start go along: the row outlives the list's line
  event "$(printf '%s\t%s\tstop\t%s' "$now" "$id" "$(printf '%s' "$line" | cut -f 2-4)")"; }
# A call from an agent nerd has never seen, after the turn ended, is the agent's own housekeeping (Claude
# Code runs one after Stop): no subagent row for it. One seen before is a subagent resumed in the background.
# So is one that names no type at any time: Claude Code's own helper calls with an id and no type and never
# stops, while a subagent's calls name theirs; only its report may come untyped.
case "$1" in tool-*)
  if [ -n "$agent" ] && ! grep -q "^$agent	" "$base.subagents" 2>/dev/null && ! grep -q "	$agent	" "$base.events" 2>/dev/null; then
    case "$(cat "$base" 2>/dev/null)" in idle|failed) exit 0 ;; esac
    [ -n "$(first agent_type)" ] || [ "$(word tool_name toolName)" = SubagentHandback ] || exit 0
  fi ;;
esac
# The oldest message nerd left for this pane, printed as the hook's whole answer; false when there was none.
# One message per block chain: the agent stops again with stop_hook_active set.
deliver() {
  grep -q '"stop_hook_active" *: *true' "$in" && return 1
  f=$(ls "$base.mail" 2>/dev/null | grep '\.out$' | head -n 1)
  [ -n "$f" ] || return 1
  id=${f%.out}
  mv "$base.mail/$f" "$base.mail/$id.claim" 2>/dev/null || return 1
  state working
  cat "$base.mail/$id.claim"
  mv -f "$base.mail/$id.claim" "$base.mail/$id.sent"; }
case "$1" in
  task)
    # the Agent tool's short description and its time, waiting for the SubagentStart that follows it
    lock || exit 0
    printf '%s\t%s\n' "$now" "$(field description)" >> "$base.tasks"
    unlock ;;
  sub-start)
    # Copilot names the subagent it starts but gives it no id: its stop is paired by name
    id=$(safe agent_id subagent_id); [ -n "$id" ] || id="n$now$$"
    kind=$(first agent_type subagent_type agentName)
    task=$(field task)
    lock || exit 0
    if [ -z "$task" ] && [ -s "$base.tasks" ]; then
      # a start follows its call at once: a description older than a minute is a call's that started no
      # subagent (it failed, and no PostToolUse says so), and goes with any before it
      n=$(T=$((now - 60)) awk -F '\t' '$1 ~ /^[0-9]+$/ && $1 >= ENVIRON["T"] + 0 { print NR; exit }' "$base.tasks")
      [ -z "$n" ] || task=$(sed -n "${n}p" "$base.tasks" | cut -f 2-)
      tail -n +$((${n:-$(wc -l < "$base.tasks")} + 1)) "$base.tasks" > "$base.tasks.tmp" && mv -f "$base.tasks.tmp" "$base.tasks"
    fi
    printf '%s\t%s\t%s\t%s\n' "$id" "${kind:-agent}" "$task" "$now" >> "$base.subagents"
    unlock ;;
  sub-stop) substop ;;
  mail) deliver ;;
  sub-reset)
    lock || exit 0
    rm -f "$base.subagents" "$base.tasks" "$base.events"
    unlock ;;
  tool-start)
    tool=$(word tool_name toolName)
    adopt
    t=$(first file_path absolute_path command pattern description)
    # the first line of a command; a value cut at an escaped quote loses its dangling backslash
    t=$(printf '%s' "$t" | sed 's/\\n.*//; s/\\$//')
    # the subagent's type rides along, so a stop that lost its list line still knows it
    event "$(printf '%s\t%s\tstart\t%s\t%s\t%s\t%s' "$now" "${agent:--}" "$(word tool_use_id)" "$tool" "$t" "$(first agent_type)")"
    # Claude Code's subagent hands back its report as its last call: no result or stop may follow
    if [ "$tool" = SubagentHandback ] && [ -n "$agent" ]; then substop; fi ;;
  tool-ok|tool-fail)
    # one line of what came back: the error, the response if it is text, else its first text field
    if [ "$1" = tool-fail ]; then s=$(first error error_message); else
      s=$(first tool_response tool_output)
      for key in tool_response toolResult tool_result; do
        [ -z "$s" ] || break
        # what follows the key, capped; control characters are escaped in JSON, so \001 never splits it
        r=$(K="\"$key\"" awk 'BEGIN { RS = "\001" } { i = index($0, ENVIRON["K"]); if (i) printf "%s", substr($0, i + length(ENVIRON["K"]), 65536); exit }' "$in")
        for k in content stdout error message text textResultForLlm text_result_for_llm; do s=$(field "$k" "$r"); [ -n "$s" ] && break; done
      done
    fi
    # the first line that says something; sed, as a shell loop over leading \n is quadratic
    s=$(printf '%s' "$s" | sed 's/^\(\\n\)*//; s/\\n.*//; s/\\$//; s/\\t/ /g')
    # JSON packed into a string is not a sentence
    case "$s" in "{"*|"["*) s="" ;; esac
    event "$(printf '%s\t%s\t%s\t%s\t%.160s' "$now" "${agent:--}" "${1#tool-}" "$(word tool_use_id)" "$s")"
    # a permission prompt was answered: the pane is back at work
    state working ;;
  *)
    w=$1
    case "$w" in
      fail) w=failed ;;
      stop) if [ "$(field status)" = error ]; then w=failed; else w=idle; fi ;;
      notify) case "$(field notification_type)" in permission_prompt|elicitation_dialog|ToolPermission) w=blocked ;; *) exit 0 ;; esac ;;
    esac
    # a failed turn stays failed through a Stop that may follow it, until the next prompt
    [ "$w" = idle ] && [ "$(cat "$base" 2>/dev/null)" = failed ] && w=failed
    # The turn's end hands over a waiting message in the same call, so no idle can land after its working.
    if [ "$w" = idle ]; then
      case "$(field hook_event_name)" in [Ss]ession[Ss]tart) ;; *) deliver && exit 0 ;; esac
    fi
    state "$w"
    session
    [ "$w" = blocked ] || { event "$(printf '%s\t-\tturn' "$now")"; event "$(printf '%s\t-\tend\t%s' "$now" "$w")"; } ;;
esac
exit 0
"#;

/// OpenCode and pi take plugins, not shell hooks: each of these writes the same files from inside it.
/// The marker tells nerd's file from someone else's; OpenCode's must stay `NerdPlugin` for files older builds wrote.
fn plugin_src(a: &Manifest) -> (&'static str, &'static str) {
    match a.name.as_str() {
        "pi" => (include_str!("pi.js"), "NerdPiExtension"),
        _ => (include_str!("opencode.js"), "NerdPlugin"),
    }
}

/// The root under which the hooks may write: nerd's state base for this HOME.
fn state_root(home: &Path) -> String {
    format!("{}/", crate::paths::nerd_dir(home).display())
}

/// The script with the only state root it accepts, and each agent's session field and id kind, written into it.
fn script_for(root: &str) -> String {
    let sessions: String = sessions(crate::agents::builtins().iter()).lines().map(|l| {
        let f: Vec<&str> = l.split(' ').collect();
        format!("    {}) sf={} sk={} ;;\n", f[0], f[1], f[2])
    }).collect();
    let inbox: String = crate::agents::builtins()
        .iter()
        .filter_map(|m| m.agent.mail.as_ref()?.inbox_env.as_ref().map(|e| format!("    {}) on=1 ie=${e} ;;\n", m.from())))
        .collect();
    SCRIPT.replacen("@ROOT@", &format!("'{}'", root.replace('\'', "'\\''")), 1).replacen("@SESSIONS@", &sessions, 1).replacen("@SESSIONS_FILE@", SESSIONS_FILE, 1).replacen("@INBOX@", &inbox, 1)
}

/// Where the hook script finds a user's agent's session field and id kind, in nerd's state root.
pub const SESSIONS_FILE: &str = "hook-sessions";

/// One line per agent with a session and a hook script: `<from> <field> <uuid|word>`; every part is validated.
pub fn sessions<'a>(ms: impl Iterator<Item = &'a Manifest>) -> String {
    ms.filter(|m| m.shape().is_some_and(|s| s != Shape::Plugin))
        .filter_map(|m| m.agent.session.as_ref().map(|x| format!("{} {} {}\n", m.from(), x.field, if x.kind == crate::agents::IdKind::Uuid { "uuid" } else { "word" })))
        .collect()
}

fn plugin_for(a: &Manifest, root: &str) -> String {
    plugin_src(a).0.replacen("\"@ROOT@\"", &serde_json::to_string(root).unwrap_or_default(), 1)
}

use crate::agents::{Manifest, Shape};

fn home() -> PathBuf {
    crate::paths::home().unwrap_or_else(|| PathBuf::from("/"))
}

/// Only manifests with a config have one.
fn config(a: &Manifest, home: &Path) -> PathBuf {
    a.config(home).unwrap_or_default()
}

fn script(a: &Manifest, home: &Path) -> PathBuf {
    a.dir(home).unwrap_or_default().join("hooks").join(SCRIPT_NAME)
}

fn installed(a: &Manifest, home: &Path) -> bool {
    let config = std::fs::read_to_string(config(a, home));
    match a.shape() {
        None => false,
        Some(Shape::Plugin) => config.is_ok_and(|s| ours_file(a, &s)),
        _ => script(a, home).exists() && config.is_ok_and(|s| s.contains(SCRIPT_NAME)),
    }
}

/// A file nerd writes whole (its own hooks file, a plugin) carries this; one without it is someone else's.
fn ours_file(a: &Manifest, text: &str) -> bool {
    text.contains(if a.shape() == Some(Shape::Plugin) { plugin_src(a).1 } else { SCRIPT_NAME })
}

/// A file nerd would write whole that is there and not nerd's: never overwritten, never deleted.
fn foreign(a: &Manifest, path: &Path) -> bool {
    matches!(a.shape(), Some(Shape::Own | Shape::Plugin)) && path.exists() && !std::fs::read_to_string(path).is_ok_and(|s| ours_file(a, &s))
}

/// The config as this build wants it, from what is there now.
fn wanted(a: &Manifest, home: &Path) -> std::io::Result<Value> {
    let script = script(a, home).to_string_lossy().into_owned();
    let (events, arg) = (&a.agent.events, a.agent.arg.as_str());
    let cmd = |word: &str| command_for(&script, word, arg);
    Ok(match a.shape() {
        Some(Shape::Nested) => {
            let mut root = read_settings(&config(a, home))?;
            // ours go first: an entry an older nerd wrote and this one no longer has must not stay behind
            remove_entries(&mut root);
            add_entries(&mut root, &script, events, arg);
            root
        }
        Some(Shape::Flat) => {
            let mut root = read_settings(&config(a, home))?;
            remove_entries(&mut root);
            let obj = object(&mut root);
            obj.entry("version").or_insert(json!(1));
            let hooks = object(obj.entry("hooks").or_insert_with(|| json!({})));
            for e in events {
                list(hooks, &e.on).push(json!({ "command": cmd(&e.word) }));
            }
            root
        }
        Some(Shape::Own) => {
            let mut hooks = serde_json::Map::new();
            for e in events {
                list(&mut hooks, &e.on).push(json!({ "type": "command", "bash": cmd(&e.word) }));
            }
            json!({ "version": 1, "hooks": hooks })
        }
        Some(Shape::Plugin) | None => Value::Null,
    })
}

fn current(a: &Manifest, home: &Path) -> bool {
    let root = state_root(home);
    if a.shape() == Some(Shape::Plugin) {
        return std::fs::read_to_string(config(a, home)).ok() == Some(plugin_for(a, &root));
    }
    std::fs::read_to_string(script(a, home)).ok() == Some(script_for(&root))
        && read_settings(&config(a, home)).ok() == wanted(a, home).ok()
}

/// Edits the agent's config surgically: unrelated entries survive, a repeat call duplicates nothing.
fn install_one(a: &Manifest, home: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let config = config(a, home);
    if foreign(a, &config) {
        return Err(std::io::Error::other(format!("{} is there and is not nerd's; it is left alone", config.display())));
    }
    if let Some(dir) = config.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if a.shape() == Some(Shape::Plugin) {
        return std::fs::write(&config, plugin_for(a, &state_root(home)));
    }
    // a config that does not parse stops here, before anything of ours is written
    let root = wanted(a, home)?;
    let script = script(a, home);
    if let Some(dir) = script.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written private and replaced whole, so a link planted at its name is not followed.
    crate::paths::write_private(&script, "tmp", script_for(&state_root(home)).as_bytes())?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    // Not overwritten on a repeat install, or there'd be nothing to roll back to.
    let bak = config.with_extension("json.bak-nerd");
    if a.shape() != Some(Shape::Own) && config.exists() && !bak.exists() {
        std::fs::copy(&config, &bak)?;
    }
    write_settings(&config, &root)
}

/// Takes nerd's hooks out of one agent's config: its plugin removed, turned off, or moved to another config.
pub(crate) fn unhook(a: &Manifest) -> std::io::Result<()> {
    uninstall_one(a, &home())
}

/// Puts nerd's hooks back into one agent's config when its plugin is turned on, if the human once hooked it.
pub(crate) fn rehook(a: &Manifest) {
    if a.agent.config.is_some()
        && read_seen(&seen_path()).contains_key(&a.name)
        && let Err(e) = install_one(a, &home())
    {
        crate::log::warn(&format!("{}: hook not put back: {e}", a.name));
    }
}

/// Removes only nerd's entries, its own files and the script.
fn uninstall_one(a: &Manifest, home: &Path) -> std::io::Result<()> {
    let config = config(a, home);
    match a.shape() {
        Some(Shape::Plugin | Shape::Own) if !foreign(a, &config) => {
            let _ = std::fs::remove_file(&config);
        }
        Some(Shape::Nested | Shape::Flat) if config.exists() => {
            let mut root = read_settings(&config)?;
            remove_entries(&mut root);
            write_settings(&config, &root)?;
        }
        _ => {}
    }
    let _ = std::fs::remove_file(script(a, home));
    Ok(())
}

pub fn is_installed() -> bool {
    let home = home();
    crate::agents::all().iter().any(|a| installed(a, &home))
}

/// That agent's hook is installed and is this build's.
pub(crate) fn hook_current(agent: &str) -> bool {
    let home = home();
    crate::agents::get(agent).is_some_and(|a| installed(&a, &home) && current(&a, &home))
}

/// Runs the hook for `word` on behalf of pane `pane`, relayed from `from`.
pub fn run_for(pane: crate::layout::PaneId, word: &str, from: &str, payload: &[u8]) -> std::io::Result<String> {
    use std::io::{Read, Seek, Write};
    use std::os::unix::fs::OpenOptionsExt;
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    // Files opened then unlinked, not pipes: a child another thread forks meanwhile could hold a pipe's end open.
    let unlinked = |tag: &str, bytes: &[u8]| -> std::io::Result<std::fs::File> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = dir.join(format!("{pane}.relay-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let mut f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path)?;
        let _ = std::fs::remove_file(&path);
        f.write_all(bytes)?;
        f.rewind()?;
        Ok(f)
    };
    let (stdin, mut out) = (unlinked("in", payload)?, unlinked("out", b"")?);
    let mut child = crate::spawn::cmd("/bin/sh")
        .arg("-c")
        .arg(script_for(&format!("{}/", crate::paths::state_root().display())))
        .arg("hook")
        .arg(word)
        .arg(from)
        .env("NERD_PANE", pane.to_string())
        .env("NERD_STATE", &dir)
        .env_remove("CLAUDE_CODE_MESSAGING_SOCKET")
        .env_remove("NERD_MSOCK")
        .env_remove("NERD_BIN")
        .stdin(stdin)
        .stdout(out.try_clone()?)
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the hook script ran over 5 s"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut s = String::new();
    out.rewind()?;
    out.read_to_string(&mut s)?;
    Ok(s)
}

/// What installing into one agent came to: the file it got, or why not.
pub type Installed = (String, Result<PathBuf, String>);

/// Agents to ask about before installing: found here and not hooked yet, or hooked with a resume line
/// that changed since it was confirmed. Each row: the agent, the file it gets, the line that resumes it.
#[derive(Debug, Default, PartialEq)]
pub struct Ask {
    pub rows: Vec<[String; 3]>,
    /// Every one of them is hooked already, only its resume line is new.
    pub changed: bool,
}

impl Ask {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn intro(&self) -> &'static str {
        if self.changed {
            "The resume line of these agents changed since you confirmed it; nerd types it into a restored pane only once you confirm again."
        } else {
            "nerd adds its hook to these agents, so it sees when they work and when they wait for you."
        }
    }

    /// With its header row.
    pub fn table(&self) -> Vec<Vec<String>> {
        std::iter::once(["Agent", "Settings file", "Resumes with"].map(String::from)).chain(self.rows.iter().cloned()).map(Vec::from).collect()
    }

    /// The whole question on one line, for the log.
    pub fn summary(&self) -> String {
        self.rows.iter().map(|r| r.join(" ")).collect::<Vec<_>>().join("; ")
    }

    /// What `nerd --install-hook` prints before its y/N.
    pub fn prompt(&self) -> String {
        let table: String = crate::menu::table_lines(&self.table(), usize::MAX).iter().map(|l| format!("{l}\n")).collect();
        format!("{}\n\n{table}\nInstall? [y/N] ", self.intro())
    }
}

/// The removal dialog's table: each hooked agent and the file that loses nerd's entries, with a header row.
pub fn installed_table() -> Vec<Vec<String>> {
    let home = home();
    let all = crate::agents::all();
    let rows = all.iter().filter(|a| installed(a, &home)).map(|a| {
        vec![a.title.clone(), a.agent.config.as_ref().map_or("", |c| c.path.as_str()).to_string()]
    });
    std::iter::once(vec!["Agent".to_string(), "Settings file".to_string()]).chain(rows).collect()
}

pub fn first_installs() -> Ask {
    first_installs_in(&home(), &seen_path())
}

fn first_installs_in(home: &Path, seen: &Path) -> Ask {
    let all = crate::agents::all();
    let asked: Vec<&Manifest> = all.iter().filter(|a| a.dir(home).is_some_and(|d| d.is_dir()) && !(installed(a, home) && resume_seen(a, home, seen))).collect();
    Ask {
        changed: !asked.is_empty() && asked.iter().all(|a| installed(a, home)),
        rows: asked
            .iter()
            .map(|a| {
                let path = a.agent.config.as_ref().map_or("", |c| c.path.as_str()).to_string();
                let resume = if a.agent.resume.is_empty() { "no resume".to_string() } else { a.resume_line() };
                [a.title.clone(), path, resume]
            })
            .collect(),
    }
}

/// Every agent whose config directory exists; one that fails does not stop the others.
/// Called once the human has confirmed what `first_installs` showed, so their resume lines count as seen.
pub fn install() -> std::io::Result<Vec<Installed>> {
    let done = install_in(&home())?;
    record_seen(&home(), &seen_path());
    Ok(done)
}

/// Each agent's resume line as the human last confirmed it, by manifest name.
fn seen_path() -> PathBuf {
    crate::paths::state_root().join("hook-confirmed.json")
}

fn read_seen(path: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_seen(path: &Path, seen: &serde_json::Map<String, Value>) {
    if crate::paths::write_private(path, "tmp", &serde_json::to_vec_pretty(seen).unwrap_or_default()).is_err() {
        crate::log::warn(&format!("confirmed resume lines not written to {}", path.display()));
    }
}

fn record_seen(home: &Path, path: &Path) {
    let mut seen = read_seen(path);
    for a in crate::agents::all().iter().filter(|a| installed(a, home)) {
        seen.insert(a.name.clone(), json!(a.resume_line()));
    }
    write_seen(path, &seen);
}

/// Whether the human confirmed this agent's resume line as it is now. A hook an older nerd installed,
/// before lines were recorded, counts as confirmed and is recorded.
fn resume_seen(a: &Manifest, home: &Path, path: &Path) -> bool {
    let mut seen = read_seen(path);
    match seen.get(&a.name) {
        Some(line) => line.as_str() == Some(a.resume_line().as_str()),
        None if installed(a, home) => {
            seen.insert(a.name.clone(), json!(a.resume_line()));
            write_seen(path, &seen);
            true
        }
        None => false,
    }
}

/// A restored pane types an agent's resume line only when the human has confirmed it.
pub fn resume_confirmed(a: &Manifest) -> bool {
    resume_seen(a, &home(), &seen_path())
}

fn install_in(home: &Path) -> std::io::Result<Vec<Installed>> {
    let all = crate::agents::all();
    let mut done = Vec::new();
    for a in all.iter().filter(|a| a.dir(home).is_some_and(|d| d.is_dir())) {
        done.push((a.title.clone(), install_one(a, home).map(|()| config(a, home)).map_err(|e| e.to_string())));
    }
    if done.is_empty() {
        let dirs: Vec<String> = all.iter().filter_map(|a| a.dir(Path::new("~"))).map(|d| d.display().to_string()).collect();
        return Err(std::io::Error::other(format!("no supported agent found: none of {} exists", dirs.join(", "))));
    }
    Ok(done)
}

/// Every agent nerd installed into; returns their names.
pub fn uninstall() -> std::io::Result<Vec<String>> {
    uninstall_in(&home())
}

fn uninstall_in(home: &Path) -> std::io::Result<Vec<String>> {
    let mut done = Vec::new();
    for a in crate::agents::all().iter().filter(|a| installed(a, home)) {
        uninstall_one(a, home)?;
        done.push(a.title.clone());
    }
    Ok(done)
}

fn read_settings(path: &Path) -> std::io::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            // Never overwrite a file we couldn't parse.
            std::io::Error::other(format!("{} does not parse as JSON: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write_settings(path: &Path, root: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(root).map_err(std::io::Error::other)?;
    text.push('\n');
    // Temp file + rename: the old file stays intact if disk space runs out mid-write. A config that is a link (a
    // dotfiles manager's) is written where it points, so the link stays the live file.
    let path = if path.is_symlink() { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    let tmp = path.with_extension("json.nerd-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn command_for(script: &str, word: &str, arg: &str) -> String {
    // Single-quoted for the shell; an apostrophe is the only thing that needs escaping inside them.
    let cmd = format!("'{}' {word}", script.replace('\'', "'\\''"));
    if arg.is_empty() { cmd } else { format!("{cmd} {arg}") }
}

/// Ours whether nested (`hooks: [{command}]`) or flat (`command`).
fn is_ours(entry: &Value) -> bool {
    let mine = |h: &Value| ["command", "bash"].iter().any(|k| h[*k].as_str().is_some_and(|c| c.contains(SCRIPT_NAME)));
    mine(entry) || entry["hooks"].as_array().is_some_and(|hs| hs.iter().any(mine))
}

/// The value as an object, replaced by an empty one when it is something else.
fn object(v: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().unwrap()
}

fn list<'a>(hooks: &'a mut serde_json::Map<String, Value>, event: &str) -> &'a mut Vec<Value> {
    let l = hooks.entry(event).or_insert_with(|| json!([]));
    if !l.is_array() {
        *l = json!([]);
    }
    l.as_array_mut().unwrap()
}

fn add_entries(root: &mut Value, script: &str, events: &[crate::agents::Event], arg: &str) {
    let hooks = object(object(root).entry("hooks").or_insert_with(|| json!({})));
    for crate::agents::Event { on, matcher, word } in events {
        let list = list(hooks, on);
        let cmd = command_for(script, word, arg);
        let present = list.iter().any(|e| {
            e["matcher"].as_str() == matcher.as_deref()
                && e["hooks"]
                    .as_array()
                    .is_some_and(|hs| hs.iter().any(|h| h["command"].as_str() == Some(cmd.as_str())))
        });
        if present {
            continue;
        }
        let mut entry = json!({ "hooks": [{ "type": "command", "command": cmd }] });
        if let Some(m) = matcher {
            entry.as_object_mut().unwrap().insert("matcher".into(), json!(m));
        }
        list.push(entry);
    }
}

fn remove_entries(root: &mut Value) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else { return };
    let mut empty = Vec::new();
    for (event, list) in hooks.iter_mut() {
        if let Some(arr) = list.as_array_mut() {
            arr.retain(|e| !is_ours(e));
            if arr.is_empty() {
                empty.push(event.clone());
            }
        }
    }
    for event in empty {
        hooks.remove(&event);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_config_that_is_a_link_stays_a_link_and_its_target_is_written() {
        let dir = std::env::temp_dir().join(format!("nerd-hook-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        let (target, link) = (dir.join("dotfiles/settings.json"), dir.join("settings.json"));
        std::fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_settings(&link, &serde_json::json!({ "hooks": {} })).unwrap();
        assert!(link.is_symlink(), "the link is still a link");
        assert!(std::fs::read_to_string(&target).unwrap().contains("hooks"), "its target got the change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn claude() -> Vec<crate::agents::Event> {
        builtin("claude").agent.events
    }

    fn builtin(name: &str) -> Manifest {
        crate::agents::builtins().into_iter().find(|m| m.name == name).unwrap()
    }

    fn count(root: &Value) -> usize {
        root["hooks"]
            .as_object()
            .map(|h| h.values().filter_map(Value::as_array).map(Vec::len).sum())
            .unwrap_or(0)
    }

    #[test]
    fn install_is_idempotent_and_keeps_foreign_entries() {
        let mut root = json!({
            "model": "fable",
            "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] }] }
        });
        add_entries(&mut root, "/h/nerd-agent-state.sh", &claude(), "");
        let after_first = root.clone();
        add_entries(&mut root, "/h/nerd-agent-state.sh", &claude(), "");
        assert_eq!(root, after_first, "a repeat install added something");
        assert_eq!(count(&root), 1 + claude().len());
        assert_eq!(root["model"], "fable");
        assert_eq!(root["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "rtk hook claude");
        assert_eq!(root["hooks"]["Notification"][0]["matcher"], "permission_prompt");
        assert!(root["hooks"]["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn an_upgrade_drops_entries_this_build_no_longer_has() {
        let mut root = json!({
            "hooks": { "PostToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] },
                { "hooks": [{ "type": "command", "command": "'/h/nerd-agent-state.sh' working" }] }
            ] }
        });
        remove_entries(&mut root);
        add_entries(&mut root, "/h/nerd-agent-state.sh", &claude(), "");
        let post = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "{post:?}");
        assert_eq!(post[0]["hooks"][0]["command"], "rtk hook claude");
        assert_eq!(post[1]["hooks"][0]["command"], "'/h/nerd-agent-state.sh' tool-ok");
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let mut root = json!({
            "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] }] }
        });
        let original = root.clone();
        add_entries(&mut root, "/h/nerd-agent-state.sh", &claude(), "");
        remove_entries(&mut root);
        assert_eq!(root, original);
    }

    #[test]
    fn empty_or_broken_settings_still_get_hooks() {
        let mut root = json!({});
        add_entries(&mut root, "/h/nerd-agent-state.sh", &claude(), "");
        assert_eq!(count(&root), claude().len());
        let mut odd = json!({ "hooks": "nope" });
        add_entries(&mut odd, "/h/nerd-agent-state.sh", &claude(), "");
        assert_eq!(count(&odd), claude().len());
    }

    #[test]
    fn apostrophe_in_the_path_is_escaped_for_the_shell() {
        assert_eq!(command_for("/Users/o'neil/.claude/h.sh", "idle", ""), r"'/Users/o'\''neil/.claude/h.sh' idle");
    }

    #[test]
    fn state_words_parse_and_junk_does_not() {
        assert_eq!(parse_state("working\n"), Some(AgentState::Working));
        assert_eq!(parse_state(" blocked "), Some(AgentState::Blocked));
        assert_eq!(parse_state("failed"), Some(AgentState::Failed));
        assert_eq!(parse_state("done"), None);
        assert_eq!(parse_state(""), None);
    }

    #[test]
    fn the_session_id_comes_only_from_a_turn_event_and_only_as_one_uuid() {
        let id = "0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c";
        let h = Hook::new("sid");
        h.run("idle", &format!(r#"{{"session_id":"{id}","hook_event_name":"SessionStart","source":"startup"}}"#));
        assert_eq!(h.file(".session").trim(), id, "a SessionStart with a UUID writes it");
        let other = "11111111-2222-3333-4444-555555555555";
        for word in ["tool-start", "tool-ok", "tool-fail"] {
            h.run(word, &format!(r#"{{"session_id":"{other}","tool_name":"Bash","tool_input":{{"session_id":"--dangerously-skip-permissions"}},"tool_response":{{"session_id":"{other}"}}}}"#));
            h.run(word, &format!(r#"{{"session_id":"{other}","tool_name":"Bash","tool_input":{{"command":"ls"}}}}"#));
        }
        assert_eq!(h.file(".session").trim(), id, "a tool event writes nothing, whatever it nests");
        h.run_as("nosuch", "idle", &format!(r#"{{"session_id":"{other}"}}"#));
        assert_eq!(h.file(".session").trim(), id, "no id from an agent nerd does not resume");
        h.run_as("cursor", "idle", &format!(r#"{{"session_id":"{other}"}}"#));
        assert_eq!(h.file(".session").trim(), id, "Cursor does not resume");
        h.run("working", &format!(r#"{{"session_id":"{other}","prompt":"x","extra":{{"session_id":"--dangerously-skip-permissions"}}}}"#));
        assert_eq!(h.file(".session").trim(), id, "two disagreeing matches write nothing");
        h.run("working", r#"{"session_id":"abc-123","prompt":"x"}"#);
        h.run("working", r#"{"session_id":"------------------------------------","prompt":"x"}"#);
        assert_eq!(h.file(".session").trim(), id, "only the exact UUID shape");
        h.run("working", &format!(r#"{{"session_id":"{other}","prompt":"x","extra":{{"session_id":"{other}"}}}}"#));
        assert_eq!(h.file(".session").trim(), other, "matches that agree are one id");
    }

    /// Runs the script as Claude Code would, in its own state directory.
    /// A payload as the script's stdin, from an unlinked file: a pipe's write end, leaked into a child another test
    /// thread spawns before the end is marked close-on-exec (not atomic on macOS), would hold back its EOF.
    fn stdin_of(bytes: &[u8]) -> std::process::Stdio {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("nerd-hook-stdin-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::write(&path, bytes).unwrap();
        let f = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        f.into()
    }

    struct Hook(PathBuf);

    impl Hook {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("nerd-hook-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn spawn(&self, word: &str, json: &str) -> std::process::Child {
            self.spawn_as(word, "", json)
        }

        fn spawn_as(&self, word: &str, agent: &str, json: &str) -> std::process::Child {
            std::process::Command::new("/bin/sh")
                .args(["-c", &script_for(&test_root()), "hook", word])
                .args((!agent.is_empty()).then_some(agent))
                .env("NERD_PANE", "7")
                .env("NERD_STATE", &self.0)
                .stdin(stdin_of(json.as_bytes()))
                .spawn()
                .unwrap()
        }

        fn run(&self, word: &str, json: &str) {
            assert!(self.spawn(word, json).wait().unwrap().success());
        }

        /// As another agent's hook entry calls it: the agent's name after the word.
        fn run_as(&self, agent: &str, word: &str, json: &str) {
            assert!(self.spawn_as(word, agent, json).wait().unwrap().success());
        }

        fn file_in(&self, dir: &str, suffix: &str) -> String {
            std::fs::read_to_string(self.0.join(dir).join(format!("7{suffix}"))).unwrap_or_default()
        }

        fn word(&self) -> String {
            self.file("").trim().to_string()
        }

        fn file(&self, suffix: &str) -> String {
            std::fs::read(self.0.join(format!("7{suffix}"))).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
        }

        fn activity(&self) -> Activity {
            parse_activity(&self.file(".events"), &self.file(".subagents"))
        }
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn kinds(a: &Activity) -> Vec<(String, String, bool)> {
        a.subs.iter().map(|s| (s.kind.clone(), s.task.clone(), s.stopped.is_none())).collect()
    }

    #[test]
    fn subagents_come_and_go_with_their_task() {
        let h = Hook::new("sub");
        h.run("task", r#"{"tool_name":"Agent","tool_input":{"description":"Find the callers","prompt":"a "description": trap"}}"#);
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore","session_id":"s"}"#);
        h.run("sub-start", r#"{"agent_id":"a2","agent_type":"critic"}"#);
        assert_eq!(kinds(&h.activity()), vec![("Explore".into(), "Find the callers".into(), true), ("critic".into(), String::new(), true)]);
        assert!(h.activity().subs[0].started > 1_700_000_000, "the start is an epoch");

        h.run("sub-stop", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        let a = h.activity();
        let mut rows = kinds(&a);
        rows.sort();
        assert_eq!(rows, vec![("Explore".into(), "Find the callers".into(), false), ("critic".into(), String::new(), true)], "the stopped one keeps its row");
        assert_eq!(h.file(".subagents").lines().count(), 1, "and leaves the live list");
        assert!(a.live(0));

        h.run("sub-reset", "{}");
        assert!(h.activity().subs.is_empty(), "a new session starts with none");
        assert!(!h.0.join("7.lock").exists(), "no lock left behind");
    }

    #[test]
    fn a_failed_agent_call_leaves_no_name_for_the_next_subagent() {
        let h = Hook::new("old-tasks");
        // two calls that started nothing a while ago, one from before descriptions carried their time
        let old = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 120;
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("7.tasks"), format!("Before the fix\n{old}\tFailed call\n")).unwrap();
        h.run("task", r#"{"tool_name":"Agent","tool_input":{"description":"Review the plan"}}"#);
        h.run("task", r#"{"tool_name":"Agent","tool_input":{"description":"Check the build"}}"#);
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"critic"}"#);
        h.run("sub-start", r#"{"agent_id":"a2","agent_type":"general-purpose"}"#);
        assert_eq!(kinds(&h.activity()), vec![("critic".into(), "Review the plan".into(), true), ("general-purpose".into(), "Check the build".into(), true)]);
        assert!(h.file(".tasks").is_empty(), "the stale ones went with the first start");

        // only stale ones queued: the start gets no name rather than a wrong one
        std::fs::write(h.0.join("7.tasks"), format!("{old}\tFailed call\n")).unwrap();
        h.run("sub-start", r#"{"agent_id":"a3","agent_type":"Explore"}"#);
        assert_eq!(h.activity().subs.iter().find(|s| s.id == "a3").unwrap().task, "");
        assert!(h.file(".tasks").is_empty());
    }

    #[test]
    fn an_untyped_call_from_an_unseen_agent_gets_no_row_mid_turn() {
        let h = Hook::new("helper");
        h.run("working", "{}");
        // Claude Code's own helper: an id, no type, no start, no stop (tests/fixtures/agents/claude)
        for t in ["t1", "t2"] {
            h.run("tool-start", &format!(r#"{{"agent_id":"x9","tool_name":"ReadNotifications","tool_use_id":"{t}","tool_input":{{}}}}"#));
        }
        assert!(h.file(".subagents").is_empty(), "no row that nothing would ever stop");
        assert!(h.activity().subs.iter().all(|s| s.id != "x9"));
    }

    #[test]
    fn tool_calls_are_logged_with_their_target_and_outcome() {
        let h = Hook::new("tools");
        h.run("tool-start", r#"{"tool_name":"Read","tool_use_id":"t1","tool_input":{"file_path":"src/client.rs","limit":20}}"#);
        h.run("tool-ok", r#"{"tool_name":"Read","tool_use_id":"t1","tool_response":{"file_path":"other"}}"#);
        assert_eq!(h.file("").trim(), "working", "a finished call means working");
        h.run("tool-start", r#"{"tool_name":"Bash","tool_use_id":"t2","tool_input":{"command":"cargo test\nls\tx","description":"Run tests"}}"#);
        let main = h.activity().main.unwrap();
        assert_eq!((main.tool.as_str(), main.target.as_str(), main.outcome), ("Bash", "cargo test", Outcome::Running));

        h.run("blocked", "{}");
        assert_eq!(h.activity().asking(), Some("Bash"), "the prompt is about the call in flight");
        h.run("tool-fail", r#"{"tool_name":"Bash","tool_use_id":"t2"}"#);
        assert_eq!(h.activity().main.unwrap().outcome, Outcome::Failed);
        assert_eq!(h.file("").trim(), "working", "an answered prompt clears blocked whatever the result");

        // inside a subagent: the same hooks, with its agent_id
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Grep","tool_use_id":"t3","tool_input":{"pattern":"fn main","path":"src"}}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Edit","tool_use_id":"t4","tool_input":{"file_path":"say \"hi\" there"}}"#);
        let a = h.activity();
        let calls: Vec<_> = a.subs[0].calls.iter().map(|c| (c.tool.as_str(), c.target.as_str(), c.outcome)).collect();
        assert_eq!(calls, vec![("Grep", "fn main", Outcome::Dropped), ("Edit", "say ", Outcome::Running)], "an escaped quote cuts the target, without its backslash");
        assert_eq!(a.main.unwrap().outcome, Outcome::Failed, "a subagent's calls are not the main thread's");
        let long = "й".repeat(200);
        h.run("tool-start", &format!(r#"{{"tool_name":"Read","tool_use_id":"t5","tool_input":{{"file_path":"{long}"}}}}"#));
        assert_eq!(h.activity().main.unwrap().target, "й".repeat(120), "cut by characters, never inside one");
        for line in h.file(".events").lines() {
            assert!(matches!(line.split('\t').count(), 3..=7), "{line:?}");
        }
    }

    #[test]
    fn a_result_leaves_one_line_under_its_call() {
        let h = Hook::new("result");
        let summary = |id: &str| h.activity().main.filter(|c| c.id == id).map(|c| c.summary);
        h.run("tool-start", r#"{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"cargo test","description":"x"}}"#);
        h.run("tool-ok", r#"{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"cargo test"},"tool_response":{"stdout":"\n\nrunning 4 tests\ntest a ... ok","stderr":"","interrupted":false}}"#);
        assert_eq!(summary("t1").as_deref(), Some("running 4 tests"), "the first line that says something");
        h.run("tool-start", r#"{"tool_name":"WebFetch","tool_use_id":"t2","tool_input":{"url":"u"}}"#);
        h.run("tool-ok", r#"{"tool_name":"WebFetch","tool_use_id":"t2","tool_response":"Received 485KB (200 OK)\nbody"}"#);
        assert_eq!(summary("t2").as_deref(), Some("Received 485KB (200 OK)"), "a plain string is its own summary");
        h.run("tool-start", r#"{"tool_name":"Write","tool_use_id":"t3","tool_input":{"file_path":"a","content":"not this"}}"#);
        h.run("tool-fail", r#"{"tool_name":"Write","tool_use_id":"t3","tool_input":{"content":"not this"},"error":"Permission denied\tfor a"}"#);
        assert_eq!(summary("t3").as_deref(), Some("Permission denied for a"), "a failure says its error");
        h.run("tool-start", r#"{"tool_name":"Read","tool_use_id":"t4","tool_input":{"file_path":"a","content":"not this"}}"#);
        let long = format!("a{}", "й".repeat(150));
        h.run("tool-ok", &format!(r#"{{"tool_name":"Read","tool_use_id":"t4","tool_input":{{"content":"not this"}},"tool_response":{{"type":"text","file":{{"content":"{long}"}}}}}}"#));
        let s = summary("t4").unwrap();
        assert_eq!(s, format!("a{}", "й".repeat(79)), "the response's own field; the shell's 160 bytes split a letter, and the half is dropped");
    }

    #[test]
    fn a_large_output_is_as_quick_and_as_short_as_a_small_one() {
        let h = Hook::new("large");
        let big = "x".repeat(5 << 20);
        let main = || h.activity().main.unwrap();
        // best of three within five seconds each: a loaded machine stalls runs; the old script took 13 s for 100 KB
        let quick = |word: &str, json: &str| {
            (0..3).any(|_| {
                let mut child = h.spawn(word, json);
                let t = std::time::Instant::now();
                while t.elapsed() < std::time::Duration::from_secs(5) {
                    if child.try_wait().unwrap().is_some() {
                        return true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let _ = child.kill();
                let _ = child.wait();
                false
            })
        };
        // tool_use_id last, as Claude Code sends it: the id is found past the output
        for (word, json) in [
            ("tool-ok", r#"{"tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"\n\nfirst line\nBODY","stderr":""},"tool_use_id":"ID"}"#),
            ("tool-ok", r#"{"tool_name":"Read","tool_input":{"file_path":"a"},"tool_response":"first line\nBODY","tool_use_id":"ID"}"#),
            ("tool-ok", r#"{"tool_name":"Read","tool_input":{"file_path":"a"},"tool_response":{"type":"text","file":{"content":"first line\nBODY"}},"tool_use_id":"ID"}"#),
            ("tool-fail", r#"{"tool_name":"Bash","tool_input":{"command":"ls"},"error":"first line\nBODY","tool_use_id":"ID"}"#),
            ("tool-start", r#"{"tool_name":"Bash","tool_input":{"command":"first line\nBODY"},"tool_use_id":"ID"}"#),
        ] {
            let mut seen = Vec::new();
            for (id, body) in [("small", "tail"), ("large", big.as_str())] {
                if word != "tool-start" {
                    h.run("tool-start", &format!(r#"{{"tool_name":"Bash","tool_use_id":"{id}"}}"#));
                }
                assert!(quick(word, &json.replace("ID", id).replace("BODY", body)), "{word} on {id} took over five seconds");
                let c = main();
                assert_eq!(c.id, id);
                seen.push(if word == "tool-start" { c.target } else { c.summary });
            }
            assert_eq!(seen, ["first line", "first line"], "{word}: {json}");
        }
        let id = "0b5e7c3a-1d2f-4e6a-9b8c-7d6e5f4a3b2c";
        assert!(quick("idle", &format!(r#"{{"session_id":"{id}","last_assistant_message":"{big}"}}"#)), "a large turn took over five seconds");
        assert_eq!(h.file(".session").trim(), id);
        let left: Vec<_> = std::fs::read_dir(&h.0).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".in.")).collect();
        assert!(left.is_empty(), "a finished hook leaves no payload file: {left:?}");
    }

    #[test]
    fn a_payload_left_by_a_killed_hook_is_swept_and_a_live_one_kept() {
        let h = Hook::new("sweep");
        h.run("working", "{}");
        let old = h.0.join("7.in.99999");
        std::fs::write(&old, "secret").unwrap();
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 60)).unwrap();
        let live = h.0.join("7.in.1");
        std::fs::write(&live, "in use").unwrap();
        h.run("idle", "{}");
        assert!(!old.exists(), "a payload twenty minutes old is a dead hook's");
        assert!(live.exists(), "a fresh one may be a parallel hook's");
    }

    #[test]
    fn a_resumed_subagent_gets_its_row_back_from_its_first_call() {
        let h = Hook::new("resume");
        h.run("sub-start", r#"{"agent_id":"r1","agent_type":"general-purpose"}"#);
        h.run("sub-stop", r#"{"agent_id":"r1","agent_type":"general-purpose"}"#);
        assert_eq!(kinds(&h.activity()), vec![("general-purpose".into(), String::new(), false)]);
        // sent a message after its stop: no SubagentStart, just its calls
        h.run("tool-start", r#"{"agent_id":"r1","agent_type":"general-purpose","tool_name":"Read","tool_use_id":"t1","tool_input":{"file_path":"a.rs"}}"#);
        let a = h.activity();
        assert_eq!(kinds(&a), vec![("general-purpose".into(), String::new(), true)], "running again");
        assert_eq!(a.subs[0].calls.len(), 1);
        assert_eq!(h.file(".subagents").lines().count(), 1, "one line, not two");
        h.run("tool-ok", r#"{"agent_id":"r1","agent_type":"general-purpose","tool_name":"Read","tool_use_id":"t1","tool_response":"fn a"}"#);
        h.run("sub-stop", r#"{"agent_id":"r1","agent_type":"general-purpose"}"#);
        let a = h.activity();
        assert_eq!(kinds(&a), vec![("general-purpose".into(), String::new(), false)], "the stop carries its type");
        assert_eq!(a.subs[0].calls[0].summary, "fn a");

        // one nerd never saw start or call a tool: Claude Code's own, stopped every half minute
        h.run("sub-stop", r#"{"agent_id":"r2","agent_type":"Explore"}"#);
        assert_eq!(h.activity().subs.len(), 1, "no row for a bare stop");
    }

    #[test]
    fn a_subagent_resumed_after_the_turn_ended_comes_back_and_a_stranger_does_not() {
        let h = Hook::new("late");
        h.run("sub-start", r#"{"agent_id":"r1","agent_type":"general-purpose"}"#);
        h.run("sub-stop", r#"{"agent_id":"r1","agent_type":"general-purpose"}"#);
        h.run("idle", "{}");
        h.run("tool-start", r#"{"agent_id":"r1","agent_type":"general-purpose","tool_name":"Read","tool_use_id":"t1","tool_input":{"file_path":"a.rs"}}"#);
        assert_eq!(kinds(&h.activity()), vec![("general-purpose".into(), String::new(), true)], "resumed in the background: running again");
        for word in ["idle", "fail"] {
            h.run(word, "{}");
            h.run("tool-start", r#"{"agent_id":"x9","tool_name":"Skill","tool_use_id":"t2","tool_input":{}}"#);
            assert!(h.activity().subs.iter().all(|s| s.id != "x9"), "after a turn that ended {word}: a stranger's call is the agent's own");
        }
    }

    #[test]
    fn logs_written_before_the_fix_still_read_right() {
        // an older script: a resumed subagent's calls with no list line, and a stop without its type
        let events = "10\tq1\tstart\tt1\tRead\ta.rs\n11\tq1\tok\tt1\n12\tq1\tstop\t\n20\tq1\tstart\tt2\tBash\tls\tgeneral-purpose\n";
        let a = parse_activity(events, "");
        assert_eq!(kinds(&a), vec![("general-purpose".into(), String::new(), true)], "running again, typed by its call");
        assert_eq!(a.subs[0].calls.len(), 2);
        let summary = "\u{250c}\u{2500} claude \u{fffd}";
        let a = parse_activity(&format!("1\t-\tstart\tt\tBash\tx\n2\t-\tok\tt\t{summary}\n"), "");
        assert_eq!(a.main.unwrap().summary, "\u{250c}\u{2500} claude", "box drawing kept, the half character a byte cut left dropped");
    }

    #[test]
    fn a_denied_call_is_not_in_flight_forever() {
        let h = Hook::new("deny");
        h.run("tool-start", r#"{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"rm -rf x"}}"#);
        h.run("blocked", "{}");
        // denied: no PostToolUse; the turn goes on and ends
        h.run("idle", "{}");
        let a = h.activity();
        assert_eq!(a.main.as_ref().unwrap().outcome, Outcome::Dropped);
        assert_eq!(a.asking(), None);

        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Bash","tool_use_id":"t2","tool_input":{"command":"x"}}"#);
        h.run("sub-stop", r#"{"agent_id":"a1"}"#);
        assert_eq!(h.activity().subs[0].calls[0].outcome, Outcome::Dropped, "a stopped subagent has nothing in flight");
    }

    #[test]
    fn a_subagent_that_hands_back_its_report_is_done() {
        let h = Hook::new("handback");
        // resumed through SendMessage: no SubagentStart, and nothing after its report
        h.run("tool-start", r#"{"agent_id":"r1","tool_name":"SubagentHandback","tool_use_id":"t1","tool_input":{"message":"all done"}}"#);
        let a = h.activity();
        let r1 = &a.subs[0];
        assert_eq!((r1.id.as_str(), r1.state(r1.last), r1.calls[0].outcome), ("r1", SubState::Done, Outcome::Ok));
        assert!(r1.shown(r1.last) && !r1.shown(r1.last + LINGER), "it lingers like any finished one");
        assert!(!a.live(r1.last) && a.asking().is_none());
        assert!(h.file(".subagents").is_empty(), "no row adopted for its report");

        // resumed earlier, so a call already brought its row back: the report takes it off the list
        h.run("tool-start", r#"{"agent_id":"r2","agent_type":"Explore","tool_name":"Read","tool_use_id":"t2","tool_input":{"file_path":"a.rs"}}"#);
        h.run("tool-ok", r#"{"agent_id":"r2","tool_use_id":"t2"}"#);
        h.run("tool-start", r#"{"agent_id":"r2","agent_type":"Explore","tool_name":"SubagentHandback","tool_use_id":"t3","tool_input":{"message":"x"}}"#);
        let r2 = h.activity().subs.into_iter().find(|s| s.id == "r2").unwrap();
        assert_eq!((r2.kind.as_str(), r2.state(r2.last), r2.calls.len()), ("Explore", SubState::Done, 2));
        assert!(h.file(".subagents").is_empty());

        // a log from before the fix: the report and nothing after it
        let a = parse_activity("1790388204\taf38\tstart\ttoolu_01\tSubagentHandback\t\t\n", "af38\tagent\t\t1790388204\n");
        assert_eq!((a.subs[0].state(1790388204 + 6 * 3600), a.subs[0].calls[0].outcome), (SubState::Done, Outcome::Ok));
    }

    #[test]
    fn a_subagent_still_runs_from_its_start_to_its_stop() {
        let h = Hook::new("normal");
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"ls"}}"#);
        let s = h.activity().subs[0].clone();
        assert_eq!((s.state(s.last), s.calls[0].outcome), (SubState::Running, Outcome::Running));
        h.run("tool-ok", r#"{"agent_id":"a1","tool_use_id":"t1"}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"SubagentHandback","tool_use_id":"t2","tool_input":{"message":"x"}}"#);
        h.run("tool-ok", r#"{"agent_id":"a1","tool_use_id":"t2","tool_response":"Report delivered"}"#);
        h.run("sub-stop", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        let a = h.activity();
        assert_eq!(kinds(&a), vec![("Explore".into(), String::new(), false)]);
        assert_eq!(a.subs[0].calls.iter().map(|c| c.outcome).collect::<Vec<_>>(), vec![Outcome::Ok, Outcome::Ok]);
    }

    #[test]
    fn a_report_after_a_resume_is_when_it_ended() {
        // an older script: stopped, resumed, then its report, with no stop after it
        let events = "10\tq\tstart\tt1\tRead\ta.rs\t\n12\tq\tstop\t\n20\tq\tstart\tt2\tRead\tb.rs\t\n30\tq\tstart\tt3\tSubagentHandback\t\t\n";
        assert_eq!(parse_activity(events, "").subs[0].stopped, Some(30));
        // a call after the report: running again
        let a = parse_activity(&format!("{events}40\tq\tstart\tt4\tRead\tc.rs\t\n"), "");
        assert_eq!(a.subs[0].stopped, None);
    }

    #[test]
    fn the_turn_line_stays_as_an_older_nerd_reads_it() {
        let h = Hook::new("turnline");
        h.run("idle", "{}");
        let lines: Vec<Vec<String>> = h.file(".events").lines().map(|l| l.split('\t').map(String::from).collect()).collect();
        assert_eq!(lines.iter().map(|f| (f.len(), f[2].as_str())).collect::<Vec<_>>(), vec![(3, "turn"), (4, "end")]);
        assert_eq!(lines[1][3], "idle");
    }

    #[test]
    fn a_silent_subagent_after_its_agent_ended_the_turn_is_not_running() {
        let h = Hook::new("lost");
        h.run("working", "{}");
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"ls"}}"#);
        let s = h.activity().subs[0].clone();
        assert_eq!(s.state(s.last + QUIET), SubState::Running, "the agent waits on it: stuck, but running");
        h.run("idle", "{}");
        let s = h.activity().subs[0].clone();
        assert_eq!(s.state(s.last + 10 * QUIET), SubState::Running, "a long build in flight after the turn ended is work");
        // its call answered, its stop lost
        h.run("tool-ok", r#"{"agent_id":"a1","tool_use_id":"t1"}"#);
        let a = h.activity();
        let s = &a.subs[0];
        assert_eq!(s.state(s.last + ORPHAN - 1), SubState::Running, "not quiet long enough yet");
        assert!(a.live(s.last + ORPHAN - 1), "a minute's grace: it may still be thinking");
        assert_eq!(s.state(s.last + ORPHAN), SubState::Unknown);
        assert!(!a.live(s.last + ORPHAN) && s.shown(s.last + ORPHAN), "its stop was lost: the pane rests, the row stays marked unknown");
        assert!(s.shown(s.last + LOST - 1));
        assert!(!a.live(s.last + LOST) && !s.shown(s.last + LOST), "taken for lost: the row goes");
        assert_eq!(s.state(s.last + LOST), SubState::Unknown, "its run still says so");

        // a background subagent still at work after the turn ended keeps running
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Read","tool_use_id":"t2","tool_input":{"file_path":"a.rs"}}"#);
        let a = h.activity();
        let s = a.subs[0].clone();
        assert_eq!(s.state(s.last + QUIET - 1), SubState::Running);
        assert!(a.live(s.last + ORPHAN), "resumed: the pane is at work again");
        // woken without a prompt (a background subagent's result), the agent is at work again
        h.run("tool-start", r#"{"tool_name":"Agent","tool_use_id":"t3","tool_input":{"description":"more"}}"#);
        assert!(!h.activity().subs[0].alone);
    }

    #[test]
    fn a_subagent_quiet_for_long_goes_while_its_agent_keeps_working() {
        let h = Hook::new("aged");
        h.run("working", "{}");
        // a row adopted before its events were cut away, and an agent that never ends its turn
        std::fs::write(h.0.join("7.subagents"), "a1\tagent\t\t100\n").unwrap();
        h.run("tool-start", r#"{"tool_name":"Bash","tool_use_id":"m1","tool_input":{"command":"ls"}}"#);
        let a = h.activity();
        let s = &a.subs[0];
        assert!(!s.alone);
        assert_eq!(s.state(s.last + QUIET), SubState::Running, "the agent is at work: too early to doubt it");
        assert!(s.shown(s.last + LOST - 1) && a.live(s.last + LOST - 1));
        assert!(!s.shown(s.last + LOST) && !a.live(s.last + LOST), "ten quiet minutes: the row goes");
        assert_eq!(s.state(s.last + LOST), SubState::Unknown);
        // a long call in flight is work, however long
        h.run("tool-start", r#"{"agent_id":"a1","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"make"}}"#);
        let a = h.activity();
        let s = &a.subs[0];
        assert!(s.shown(s.last + 10 * LOST) && a.live(s.last + 10 * LOST));
        assert_eq!(s.state(s.last + 10 * LOST), SubState::Running);
    }

    #[test]
    fn a_failed_turn_stays_failed_until_the_next_prompt() {
        let h = Hook::new("fail");
        h.run("working", "{}");
        h.run("fail", r#"{"error":"rate_limit"}"#);
        assert_eq!(parse_state(&h.file("")), Some(AgentState::Failed));
        h.run("idle", "{}");
        assert_eq!(parse_state(&h.file("")), Some(AgentState::Failed), "a Stop after the failure does not hide it");
        h.run("working", "{}");
        assert_eq!(parse_state(&h.file("")), Some(AgentState::Working), "UserPromptSubmit clears it");
    }

    #[test]
    fn the_log_is_cut_to_its_last_lines_under_the_lock() {
        let h = Hook::new("cut");
        std::fs::create_dir_all(&h.0).unwrap();
        let old: String = (0..800).map(|i| format!("1\ta1\tok\tt{i}\n")).collect();
        std::fs::write(h.0.join("7.events"), old).unwrap();
        h.run("tool-start", r#"{"tool_name":"Read","tool_use_id":"new","tool_input":{"file_path":"a"}}"#);
        let log = h.file(".events");
        assert_eq!(log.lines().count(), 401, "{}", log.lines().count());
        assert!(log.lines().next().unwrap().ends_with("\t-\tcut"));
        assert!(log.lines().last().unwrap().contains("\tstart\tnew\tRead\ta\t"));
        assert!(h.activity().cut);
        assert!(!h.0.join("7.lock").exists());
    }

    #[test]
    fn parallel_appends_keep_every_line_whole() {
        let h = Hook::new("par");
        h.run("sub-start", r#"{"agent_id":"a1","agent_type":"Explore"}"#);
        let kids: Vec<_> = (0..12)
            .map(|i| h.spawn("tool-start", &format!(r#"{{"agent_id":"a1","tool_name":"Read","tool_use_id":"t{i}","tool_input":{{"file_path":"f{i}"}}}}"#)))
            .collect();
        for mut k in kids {
            assert!(k.wait().unwrap().success());
        }
        let log = h.file(".events");
        assert_eq!(log.lines().count(), 12);
        assert!(log.lines().all(|l| l.split('\t').count() == 7), "{log}");
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let h = Hook::new("stale");
        std::fs::create_dir_all(h.0.join("7.lock")).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        std::fs::File::open(h.0.join("7.lock")).unwrap().set_modified(old).unwrap();
        h.run("tool-start", r#"{"tool_name":"Read","tool_use_id":"t1","tool_input":{"file_path":"a"}}"#);
        assert_eq!(h.file(".events").lines().count(), 1, "the event was not dropped");
        assert!(!h.0.join("7.lock").exists());
    }

    #[test]
    fn a_subagent_listed_by_an_older_hook_counts_from_when_nerd_first_saw_it() {
        let mut a = parse_activity("", "a1\tExplore\tfind\n");
        assert_eq!(a.subs[0].started, 0, "the old three-field line has no start");
        a.carry_starts(&Activity::default(), 500);
        assert_eq!((a.subs[0].started, a.subs[0].last), (500, 500));
        let mut again = parse_activity("", "a1\tExplore\tfind\n");
        again.carry_starts(&a, 900);
        assert_eq!(again.subs[0].started, 500, "the first sight stays, the age keeps counting from it");
        assert_eq!(crate::sidebar::ago(std::time::Duration::from_secs(900 - 500)), "6m");
    }

    #[test]
    fn activity_skips_malformed_lines_and_derives_the_rows() {
        let events = "100\t-\tstart\tt1\tRead\ta.rs\n\
                      junk\n\
                      101\t-\tok\tt1\n\
                      102\ta1\tstart\tt2\tBash\tcargo test\n\
                      103\ta1\tfail\tt2\n\
                      104\ta1\tstart\tt3\n\
                      105\ta2\tstop\tcritic\treview\t90\n";
        let a = parse_activity(events, "a1\tExplore\tfind\t95\n");
        assert_eq!(a.main.as_ref().map(|c| (c.tool.as_str(), c.outcome)), Some(("Read", Outcome::Ok)));
        assert_eq!(kinds(&a), vec![("critic".into(), "review".into(), false), ("Explore".into(), "find".into(), true)], "by start");
        let explore = &a.subs[1];
        assert_eq!((explore.started, explore.last, explore.calls.len()), (95, 103, 1), "the short start line is skipped");
        assert_eq!(explore.state(120), SubState::Running, "a failure, but not quiet for 30 s yet");
        assert_eq!(explore.state(133), SubState::Failed);
        assert!(!explore.stale(222) && explore.stale(223), "stuck after 120 s of silence");
        let critic = &a.subs[0];
        assert_eq!(critic.state(106), SubState::Done);
        assert!(critic.shown(134) && !critic.shown(135), "a finished row stays 30 s");
        assert!(!a.cut);
    }

    fn calls(a: &Activity) -> Vec<(String, String, Outcome, String)> {
        a.main.iter().map(|c| (c.tool.clone(), c.target.clone(), c.outcome, c.summary.clone())).collect()
    }

    #[test]
    fn codex_reports_state_tools_subagents_and_its_session() {
        let h = Hook::new("codex");
        h.run_as("codex", "idle", r#"{"session_id":"s1","hook_event_name":"SessionStart","source":"startup"}"#);
        h.run_as("codex", "working", r#"{"session_id":"s1","turn_id":"t","prompt":"go"}"#);
        assert_eq!(h.word(), "working");
        assert_eq!(h.file(".session").trim(), "s1", "Codex's id is a word");
        h.run_as("codex", "working", r#"{"session_id":"-rf","prompt":"go"}"#);
        assert_eq!(h.file(".session").trim(), "s1", "never one that reads as a flag");
        h.run_as("codex", "tool-start", r#"{"session_id":"s1","turn_id":"t","tool_name":"Bash","tool_use_id":"c1","tool_input":{"command":"cargo test"}}"#);
        h.run_as("codex", "blocked", r#"{"session_id":"s1","turn_id":"t","tool_name":"Bash","tool_input":{"command":"cargo test","description":"run"}}"#);
        assert_eq!((h.word(), h.activity().asking().map(str::to_string)), ("blocked".into(), Some("Bash".into())));
        h.run_as("codex", "tool-ok", r#"{"session_id":"s1","turn_id":"t","tool_name":"Bash","tool_use_id":"c1","tool_input":{"command":"cargo test"},"tool_response":"test result: ok\nmore"}"#);
        assert_eq!(calls(&h.activity()), vec![("Bash".into(), "cargo test".into(), Outcome::Ok, "test result: ok".into())]);
        h.run_as("codex", "sub-start", r#"{"session_id":"s1","turn_id":"t","agent_id":"x1","agent_type":"worker"}"#);
        assert_eq!(kinds(&h.activity()), vec![("worker".into(), String::new(), true)], "no task given, none shown");
        h.run_as("codex", "sub-stop", r#"{"session_id":"s1","turn_id":"t","agent_id":"x1","agent_type":"worker","last_assistant_message":"done"}"#);
        assert_eq!(kinds(&h.activity()), vec![("worker".into(), String::new(), false)]);
        h.run_as("codex", "idle", r#"{"session_id":"s1","turn_id":"t","stop_hook_active":false}"#);
        assert_eq!(h.word(), "idle");
    }

    #[test]
    fn qwen_reports_failures_permission_notices_and_subagent_tools() {
        let h = Hook::new("qwen");
        h.run_as("qwen", "sub-start", r#"{"session_id":"s","agent_type":"general-purpose","agent_id":"q1","permission_mode":"default"}"#);
        h.run_as("qwen", "tool-start", r#"{"session_id":"s","agent_id":"q1","tool_name":"run_shell_command","tool_use_id":"u1","tool_input":{"command":"ls -la"}}"#);
        h.run_as("qwen", "tool-fail", r#"{"session_id":"s","agent_id":"q1","tool_name":"run_shell_command","tool_input":{"command":"ls -la"},"error":"exit 2","is_interrupt":false}"#);
        let a = h.activity();
        let c = &a.subs[0].calls[0];
        assert_eq!((c.tool.as_str(), c.target.as_str(), c.outcome, c.summary.as_str()), ("run_shell_command", "ls -la", Outcome::Failed, "exit 2"));
        assert!(a.main.is_none(), "a subagent's call is its own");
        h.run_as("qwen", "notify", r#"{"session_id":"s","message":"done","title":"t","notification_type":"idle_prompt"}"#);
        assert_eq!(h.word(), "working", "a notice that is not a question changes nothing");
        h.run_as("qwen", "notify", r#"{"session_id":"s","message":"Allow?","notification_type":"permission_prompt"}"#);
        assert_eq!(h.word(), "blocked");
    }

    #[test]
    fn cursor_pairs_a_stop_without_id_by_type_and_task() {
        let h = Hook::new("cursor");
        h.run_as("cursor", "working", r#"{"conversation_id":"c","generation_id":"g","hook_event_name":"beforeSubmitPrompt","prompt":"go"}"#);
        h.run_as("cursor", "tool-start", r#"{"conversation_id":"c","tool_name":"Shell","tool_input":{"command":"npm test"},"tool_use_id":"abc","cwd":"/p"}"#);
        h.run_as("cursor", "tool-ok", r#"{"conversation_id":"c","tool_name":"Shell","tool_input":{"command":"npm test"},"tool_output":"{\"exitCode\":0}","tool_use_id":"abc"}"#);
        assert_eq!(calls(&h.activity()), vec![("Shell".into(), "npm test".into(), Outcome::Ok, String::new())], "JSON inside a string is not a summary");
        h.run_as("cursor", "tool-start", r#"{"tool_name":"Shell","tool_input":{"command":"npm run e2e"},"tool_use_id":"abd"}"#);
        h.run_as("cursor", "tool-fail", r#"{"tool_name":"Shell","tool_input":{"command":"npm run e2e"},"tool_use_id":"abd","error_message":"Command timed out","failure_type":"timeout"}"#);
        assert_eq!(calls(&h.activity())[0].3, "Command timed out");

        let start = |task: &str, id: &str| format!(r#"{{"subagent_id":"{id}","subagent_type":"generalPurpose","task":"{task}","parent_conversation_id":"c"}}"#);
        h.run_as("cursor", "sub-start", &start("Explore auth", "s1"));
        h.run_as("cursor", "sub-start", &start("Explore billing", "s2"));
        h.run_as("cursor", "sub-stop", r#"{"subagent_type":"generalPurpose","status":"completed","task":"Explore billing","summary":"x"}"#);
        let a = h.activity();
        assert_eq!(kinds(&a), vec![("generalPurpose".into(), "Explore auth".into(), true), ("generalPurpose".into(), "Explore billing".into(), false)]);
        assert_eq!(a.subs.iter().find(|s| s.stopped.is_some()).unwrap().id, "s2", "the one with that task stopped");

        h.run_as("cursor", "stop", r#"{"status":"error","loop_count":0}"#);
        assert_eq!(h.word(), "failed", "a turn that ended on an error");
        h.run_as("cursor", "stop", r#"{"status":"completed","loop_count":0}"#);
        assert_eq!(h.word(), "failed");
        h.run_as("cursor", "working", r#"{"prompt":"again"}"#);
        h.run_as("cursor", "stop", r#"{"status":"completed","loop_count":0}"#);
        assert_eq!(h.word(), "idle");
    }

    #[test]
    fn copilot_pairs_a_nameless_start_with_its_stop_by_name() {
        let h = Hook::new("copilot");
        h.run_as("copilot", "tool-start", r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"bash","tool_input":{"command":"git status"}}"#);
        h.run_as("copilot", "notify", r#"{"sessionId":"s","hook_event_name":"Notification","message":"m","notification_type":"permission_prompt"}"#);
        assert_eq!((h.word(), h.activity().asking().map(str::to_string)), ("blocked".into(), Some("bash".into())));
        h.run_as("copilot", "tool-ok", r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"bash","tool_input":{"command":"git status"},"tool_result":{"result_type":"success","text_result_for_llm":"On branch master"}}"#);
        assert_eq!(calls(&h.activity()), vec![("bash".into(), "git status".into(), Outcome::Ok, "On branch master".into())], "no call id: the latest start is the one");

        let start = r#"{"sessionId":"s","timestamp":1,"cwd":"/p","agentName":"explore","agentDisplayName":"Explore","agentDescription":"Reads code"}"#;
        h.run_as("copilot", "sub-start", start);
        h.run_as("copilot", "sub-start", start);
        let a = h.activity();
        assert_eq!(kinds(&a), vec![("explore".into(), String::new(), true); 2], "no task is invented from the description");
        let first = a.subs.iter().map(|s| s.id.clone()).min_by_key(|id| h.file(".subagents").find(id.as_str())).unwrap();
        h.run_as("copilot", "sub-stop", r#"{"sessionId":"s","agentId":"cp-9","agentType":"explore","agentName":"explore","stopReason":"end_turn"}"#);
        let a = h.activity();
        assert_eq!(a.subs.iter().filter(|s| s.stopped.is_some()).map(|s| s.id.clone()).collect::<Vec<_>>(), vec![first], "the oldest of that name");
        assert_eq!(h.file(".subagents").lines().count(), 1);
    }

    #[test]
    fn gemini_reports_the_state_and_the_call_in_flight() {
        let h = Hook::new("gemini");
        h.run_as("gemini", "working", r#"{"session_id":"s","hook_event_name":"BeforeAgent","timestamp":"t","prompt":"go"}"#);
        h.run_as("gemini", "tool-start", r#"{"session_id":"s","hook_event_name":"BeforeTool","tool_name":"read_file","tool_input":{"absolute_path":"/r/src/main.rs"}}"#);
        assert_eq!(calls(&h.activity()), vec![("read_file".into(), "/r/src/main.rs".into(), Outcome::Running, String::new())]);
        h.run_as("gemini", "notify", r#"{"session_id":"s","hook_event_name":"Notification","notification_type":"ToolPermission","message":"Allow?","details":{}}"#);
        assert_eq!(h.word(), "blocked");
        h.run_as("gemini", "tool-ok", r#"{"session_id":"s","hook_event_name":"AfterTool","tool_name":"read_file","tool_input":{"absolute_path":"/r/src/main.rs"},"tool_response":{"llmContent":"fn main"}}"#);
        assert_eq!(calls(&h.activity())[0].2, Outcome::Ok);
        h.run_as("gemini", "idle", r#"{"session_id":"s","hook_event_name":"AfterAgent","prompt":"go","prompt_response":"ok","stop_hook_active":false}"#);
        assert_eq!(h.word(), "idle");
        assert!(h.file(".session").is_empty());
    }

    #[test]
    fn a_hook_given_a_clean_environment_finds_its_pane_in_an_ancestor() {
        let h = Hook::new("ancestor");
        // macOS hides a system binary's environment from ps, so the agent here is node, as Gemini's is
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the ancestor walk test is skipped");
            return;
        };
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("hook.sh"), script_for(&test_root())).unwrap();
        let run = r#"require("child_process").spawnSync("/usr/bin/env", ["-u", "NERD_PANE", "-u", "NERD_STATE", "/bin/sh", process.argv[1], "working", "gemini"], { input: "{}" })"#;
        let out = std::process::Command::new(node).args(["-e", run]).arg(h.0.join("hook.sh")).env("NERD_PANE", "7").env("NERD_STATE", h.0.join("with space")).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let word = std::fs::read_to_string(h.0.join("with space").join("7")).unwrap_or_default();
        assert_eq!(word.trim(), "working", "a state directory with a space in it survives the walk");
    }

    /// Every agent's config directory in a scratch HOME, each with an entry of someone else's.
    fn scratch_home(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("nerd-home-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let nested = |dir: &str, file: &str, event: &str| {
            let path = home.join(dir).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let v = json!({ "model": "keep", "hooks": { event: [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "foreign.sh" }] }] } });
            std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        };
        nested(".claude", "settings.json", "PreToolUse");
        nested(".codex", "hooks.json", "Stop");
        nested(".qwen", "settings.json", "PreToolUse");
        nested(".gemini", "settings.json", "BeforeTool");
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::write(home.join(".cursor/hooks.json"), r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"./format.sh"}],"stop":[{"command":"./bell.sh"}]}}"#).unwrap();
        std::fs::create_dir_all(home.join(".copilot/hooks")).unwrap();
        std::fs::write(home.join(".copilot/hooks/theirs.json"), r#"{"version":1,"hooks":{}}"#).unwrap();
        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        std::fs::create_dir_all(home.join(".pi")).unwrap();
        home
    }

    fn snapshot(home: &Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut dirs = vec![home.to_path_buf()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else {
                    out.push((p.strip_prefix(home).unwrap().display().to_string(), std::fs::read_to_string(&p).unwrap_or_default()));
                }
            }
        }
        out.sort();
        out
    }

    fn ours(v: &Value) -> usize {
        v["hooks"].as_object().map_or(0, |h| h.values().filter_map(Value::as_array).flatten().filter(|e| is_ours(e)).count())
    }

    #[test]
    fn every_agent_found_gets_its_hook_and_loses_only_that() {
        let home = scratch_home("all");
        let before = snapshot(&home);
        let read = |p: &str| serde_json::from_str::<Value>(&std::fs::read_to_string(home.join(p)).unwrap()).unwrap();
        let foreign: Vec<Value> = [".claude/settings.json", ".codex/hooks.json", ".qwen/settings.json", ".gemini/settings.json", ".cursor/hooks.json"].iter().map(|p| read(p)).collect();

        let done = install_in(&home).unwrap();
        let agents = crate::agents::builtins();
        assert_eq!(done.iter().map(|(n, r)| (n.clone(), r.is_ok())).collect::<Vec<_>>(), agents.iter().map(|a| (a.title.clone(), true)).collect::<Vec<_>>());
        for a in &agents {
            assert!(installed(a, &home) && current(a, &home), "{}", a.name);
            if a.shape() != Some(Shape::Plugin) {
                assert_eq!(std::fs::read_to_string(script(a, &home)).unwrap(), script_for(&state_root(&home)), "{}: its own copy of the script", a.name);
                assert_eq!(ours(&read(&config(a, &home).strip_prefix(&home).unwrap().to_string_lossy())), a.agent.events.len(), "{}", a.name);
            }
        }
        let claude = read(".claude/settings.json");
        assert_eq!((claude["model"].as_str(), claude["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str()), (Some("keep"), Some("foreign.sh")));
        let codex = read(".codex/hooks.json");
        assert!(codex["hooks"]["Stop"].as_array().unwrap().iter().any(|e| e["hooks"][0]["command"].as_str().is_some_and(|c| c.ends_with("nerd-agent-state.sh' idle codex"))));
        let cursor = read(".cursor/hooks.json");
        assert_eq!((cursor["version"].as_i64(), cursor["hooks"]["stop"][0]["command"].as_str()), (Some(1), Some("./bell.sh")));
        assert!(cursor["hooks"]["stop"][1]["command"].as_str().unwrap().ends_with("' stop cursor"), "flat: {cursor}");
        let copilot = read(".copilot/hooks/nerd.json");
        assert!(copilot["hooks"]["subagentStart"][0]["bash"].as_str().unwrap().ends_with("' sub-start copilot"));
        assert_eq!(std::fs::read_to_string(home.join(".config/opencode/plugins/nerd.js")).unwrap(), plugin_for(&builtin("opencode"), &state_root(&home)));
        assert_eq!(std::fs::read_to_string(home.join(".pi/agent/extensions/nerd.js")).unwrap(), plugin_for(&builtin("pi"), &state_root(&home)));
        assert!(std::fs::read_to_string(script(&builtin("claude"), &home)).unwrap().contains(&format!("root='{}/Library/Application Support/nerd/'", home.display())));

        let once = snapshot(&home);
        install_in(&home).unwrap();
        refresh_in(&home).unwrap();
        assert_eq!(snapshot(&home), once, "a repeat install and a refresh change nothing");

        // an older build's script is replaced on the next start
        std::fs::write(script(&builtin("codex"), &home), "#!/bin/sh\n# old\n").unwrap();
        assert!(!current(&builtin("codex"), &home));
        refresh_in(&home).unwrap();
        assert_eq!(snapshot(&home), once);

        assert_eq!(uninstall_in(&home).unwrap().len(), agents.len());
        let back: Vec<Value> = [".claude/settings.json", ".codex/hooks.json", ".qwen/settings.json", ".gemini/settings.json", ".cursor/hooks.json"].iter().map(|p| read(p)).collect();
        assert_eq!(back, foreign, "each file holds exactly what it held");
        let left: Vec<String> = snapshot(&home).into_iter().map(|(p, _)| p).filter(|p| !p.contains(".bak-nerd")).collect();
        assert_eq!(left, before.into_iter().map(|(p, _)| p).collect::<Vec<_>>(), "no script, plugin or file of ours left");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn only_agents_that_are_there_get_a_hook() {
        let home = std::env::temp_dir().join(format!("nerd-home-some-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(install_in(&home).is_err(), "nothing to install into");
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let done = install_in(&home).unwrap();
        assert_eq!(done.iter().map(|(n, r)| (n.as_str(), r.is_ok())).collect::<Vec<_>>(), vec![("Cursor", true)]);
        assert!(!home.join(".claude").exists(), "no config directory is made for an agent that is not there");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".cursor/hooks.json")).unwrap()).unwrap();
        assert_eq!(v["version"], 1, "a new Cursor file gets the version it requires");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_opencode_plugin_writes_the_same_lines() {
        let h = Hook::new("opencode");
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the OpenCode plugin test is skipped");
            return;
        };
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin("opencode"), &test_root())).unwrap();
        std::fs::write(
            h.0.join("run.mjs"),
            r#"
import { copyFileSync } from "node:fs"
import { NerdPlugin } from "./nerd.mjs"
const p = await NerdPlugin()
const ev = (type, properties) => p.event({ event: { type, properties } })
await ev("session.created", { sessionID: "m", info: { id: "m" } })
await p["chat.message"]({ sessionID: "m" })
await p["tool.execute.before"]({ tool: "bash", sessionID: "m", callID: "c1" }, { args: { command: "cargo test\nls" } })
await p["tool.execute.after"]({ tool: "bash", sessionID: "m", callID: "c1", args: {} }, { title: "", output: "\nrunning 4 tests\nok", metadata: {} })
await ev("session.created", { sessionID: "ses_k", info: { id: "ses_k", parentID: "m", agent: "explore", title: "Find the callers" } })
await p["tool.execute.before"]({ tool: "read", sessionID: "ses_k", callID: "c2" }, { args: { filePath: "src/a.rs" } })
await ev("permission.asked", { id: "p", sessionID: "ses_k", permission: "edit", patterns: [], metadata: {}, always: [] })
copyFileSync(process.env.NERD_STATE + "/7", process.env.NERD_STATE + "/asked")
await ev("session.idle", { sessionID: "ses_k" })
await ev("session.error", { sessionID: "m" })
await ev("session.idle", { sessionID: "m" })
"#,
        )
        .unwrap();
        let out = std::process::Command::new(node).arg(h.0.join("run.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &h.0).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(std::fs::read_to_string(h.0.join("asked")).unwrap().trim(), "blocked");
        assert_eq!(h.word(), "failed", "an error, then idle: still failed");
        let a = h.activity();
        assert_eq!(calls(&a), vec![("bash".into(), "cargo test".into(), Outcome::Ok, "running 4 tests".into())]);
        assert_eq!(kinds(&a), vec![("explore".into(), "Find the callers".into(), false)]);
        let c = &a.subs[0].calls[0];
        assert_eq!((c.tool.as_str(), c.target.as_str(), c.outcome), ("read", "src/a.rs", Outcome::Dropped), "the child went idle with it unanswered");
        assert_eq!(h.file(".session").trim(), "m", "the main session, not its child");
        assert!(h.file(".events").contains("\t-\tturn\n") && h.file(".events").ends_with("\t-\tend\tfailed\n"), "the turn's word on a line of its own");
        assert!(a.subs[0].alone);

        // started with a state directory outside its root, it hooks nothing
        std::fs::write(h.0.join("stray.mjs"), "import { NerdPlugin } from \"./nerd.mjs\"\nconsole.log(Object.keys(await NerdPlugin()).length)\n").unwrap();
        let node = which("node").unwrap();
        for (pane, dir) in [("7", "/tmp/elsewhere".to_string()), ("7", format!("{}/../x", h.0.display())), ("x", h.0.display().to_string())] {
            let out = std::process::Command::new(&node).arg(h.0.join("stray.mjs")).env("NERD_PANE", pane).env("NERD_STATE", &dir).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0", "{pane} {dir}");
        }
    }

    #[test]
    fn the_pi_extension_writes_the_same_lines() {
        let h = Hook::new("pi");
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the pi extension test is skipped");
            return;
        };
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin("pi"), &test_root())).unwrap();
        std::fs::write(
            h.0.join("run.mjs"),
            r#"
import { copyFileSync, writeFileSync } from "node:fs"
import ext from "./nerd.mjs"
const on = {}
ext({ on: (name, fn) => (on[name] = fn) })
const dir = process.env.NERD_STATE, file = dir + "/s.jsonl", id = "019abc12-0000-7000-8000-000000000001"
let idle = true
const copy = (a, b) => { try { copyFileSync(a, b) } catch {} }
const ctx = { mode: process.argv[2], isIdle: () => idle, sessionManager: { getSessionFile: () => file, getSessionId: () => id } }
const ev = (name, e = {}) => on[name]?.({ type: name, ...e }, ctx)
writeFileSync(dir + "/7.session", "old\n")
ev("session_start", { reason: "startup" })
copy(dir + "/7.session", dir + "/fresh")
writeFileSync(file, "{}\n")
idle = false
ev("agent_start")
ev("tool_execution_start", { toolCallId: "c1", toolName: "bash", args: { command: "cargo test\nls" } })
ev("tool_execution_end", { toolCallId: "c1", result: { content: [{ type: "text", text: "\nrunning 4 tests\nok" }] }, isError: false })
ev("ui_prompt_start", { kind: "select", title: "sudo?" })
copy(dir + "/7", dir + "/asked")
ev("ui_prompt_end", { kind: "select" })
copy(dir + "/7", dir + "/answered")
ev("tool_execution_start", { toolCallId: "c2", toolName: "read", args: { path: "src/a.rs" } })
ev("tool_execution_end", { toolCallId: "c2", result: { content: [{ type: "text", text: "ENOENT: src/a.rs" }] }, isError: true })
ev("agent_before_settle", { outcome: "error" })
idle = true
ev("agent_settled")
console.log(process.title)
"#,
        )
        .unwrap();
        let run = |mode: &str| std::process::Command::new(&node).arg(h.0.join("run.mjs")).arg(mode).env("NERD_PANE", "7").env("NERD_STATE", &h.0).env_remove("NERD_HOOK_DUMP").output().unwrap();

        // a child `pi -p` in the pane writes nothing at all
        let before = snapshot(&h.0);
        let out = run("print");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let left: Vec<String> = snapshot(&h.0).into_iter().map(|(p, _)| p).filter(|p| !["s.jsonl", "7.session", "fresh"].contains(&p.as_str())).collect();
        assert_eq!(left, before.into_iter().map(|(p, _)| p).collect::<Vec<_>>());
        assert!(!h.0.join("7").exists());
        let _ = std::fs::remove_file(h.0.join("fresh"));
        let _ = std::fs::remove_file(h.0.join("s.jsonl"));

        let out = run("tui");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!h.0.join("fresh").exists(), "no session file yet: the stale id is gone and no new one is written");
        assert_eq!(std::fs::read_to_string(h.0.join("asked")).unwrap().trim(), "blocked");
        assert_eq!(std::fs::read_to_string(h.0.join("answered")).unwrap().trim(), "working", "a prompt answered mid-turn: back at work");
        assert_eq!(h.word(), "failed", "an error, then settled: still failed");
        assert_eq!(h.file(".session").trim(), "019abc12-0000-7000-8000-000000000001");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "pi --session 019abc12-0000-7000-8000-000000000001", "the id is in ps for the live check");
        let a = h.activity();
        assert_eq!(calls(&a), vec![("read".into(), "src/a.rs".into(), Outcome::Failed, "ENOENT: src/a.rs".into())], "the latest call");
        let ev = h.file(".events");
        assert!(ev.contains("\t-\tstart\tc1\tbash\tcargo test\n") && ev.contains("\t-\tok\tc1\trunning 4 tests\n"), "{ev}");
        assert!(h.file(".events").ends_with("\t-\tend\tfailed\n"), "the turn's word on a line of its own");

        // a prompt answered while pi waits for you leaves it idle
        std::fs::write(
            h.0.join("idle.mjs"),
            "import ext from \"./nerd.mjs\"\nconst on = {}\next({ on: (n, f) => (on[n] = f) })\nconst ctx = { mode: \"tui\", isIdle: () => true, sessionManager: {} }\non.ui_prompt_start({}, ctx)\non.ui_prompt_end({}, ctx)\n",
        )
        .unwrap();
        let out = std::process::Command::new(&node).arg(h.0.join("idle.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &h.0).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(h.word(), "idle");

        // started with a state directory outside its root, it hooks nothing
        std::fs::write(h.0.join("stray.mjs"), "import ext from \"./nerd.mjs\"\nlet n = 0\next({ on: () => n++ })\nconsole.log(n)\n").unwrap();
        let sibling = format!("{}-x/agents", test_root().trim_end_matches('/'));
        for (pane, dir) in [("7", "/elsewhere".to_string()), ("7", format!("{}/../x", h.0.display())), ("x", h.0.display().to_string()), ("7", sibling)] {
            let out = std::process::Command::new(&node).arg(h.0.join("stray.mjs")).env("NERD_PANE", pane).env("NERD_STATE", &dir).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0", "{pane} {dir}");
        }
    }

    /// The root the test scripts accept: everything under the temporary directory.
    fn test_root() -> String {
        let t = std::env::temp_dir().display().to_string();
        if t.ends_with('/') { t } else { format!("{t}/") }
    }

    /// Runs the script accepting only `root`, with the pane variables given (or none, to walk up).
    fn run_guarded(root: &Path, pane: &str, state: &str, cwd: &Path) {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &script_for(&format!("{}/", root.display())), "hook", "working"])
            .env("NERD_PANE", pane)
            .env("NERD_STATE", state)
            .current_dir(cwd)
            .stdin(stdin_of(b"{}"))
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn the_script_writes_only_under_its_own_root() {
        let h = Hook::new("guard");
        let root = h.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let everything = || snapshot(&h.0).into_iter().map(|(p, _)| p).collect::<Vec<_>>();
        let before = everything();
        let dotdot = format!("{}/../evil", root.display());
        for (pane, state) in [
            ("7", dotdot.as_str()),
            ("7", "relative/agents"),
            ("7", &format!("{}/a\nb", root.display())),
            ("7;x", &format!("{}/agents", root.display())),
        ] {
            run_guarded(&root, pane, state, &h.0);
            assert_eq!(everything(), before, "pane {pane:?}, state {state:?}: nothing may be written");
        }
        run_guarded(&root, "7", &format!("{}/agents", root.display()), &h.0);
        assert_eq!(std::fs::read_to_string(root.join("agents/7")).unwrap().trim(), "working", "the right place still works");
    }

    #[test]
    fn a_sibling_folder_that_shares_the_roots_name_is_outside_it() {
        let h = Hook::new("sibling");
        std::fs::create_dir_all(&h.0).unwrap();
        let root = state_root(&h.0);
        let sibling = format!("{}-x/agents", root.trim_end_matches('/'));
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &script_for(&root), "hook", "working"])
            .env("NERD_PANE", "7")
            .env("NERD_STATE", &sibling)
            .stdin(stdin_of(b"{}"))
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert!(!Path::new(&sibling).exists(), "the hook wrote under {sibling}");
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the OpenCode half is skipped");
            return;
        };
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin("opencode"), &root)).unwrap();
        std::fs::write(h.0.join("stray.mjs"), "import { NerdPlugin } from \"./nerd.mjs\"\nconsole.log(Object.keys(await NerdPlugin()).length)\n").unwrap();
        let out = std::process::Command::new(node).arg(h.0.join("stray.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &sibling).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0", "the plugin hooks {sibling}");
    }

    #[test]
    fn a_variable_hidden_inside_another_ones_value_leads_nowhere() {
        let h = Hook::new("junk");
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the ancestor walk test is skipped");
            return;
        };
        let root = h.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(h.0.join("hook.sh"), script_for(&format!("{}/", root.display()))).unwrap();
        let run = r#"require("child_process").spawnSync("/usr/bin/env", ["-u", "NERD_PANE", "-u", "NERD_STATE", "/bin/sh", process.argv[1], "working"], { input: "{}" })"#;
        let junk = format!("x NERD_STATE={}/evil NERD_PANE=7", h.0.display());
        let out = std::process::Command::new(node).args(["-e", run]).arg(h.0.join("hook.sh")).env_remove("NERD_PANE").env_remove("NERD_STATE").env("JUNK", junk).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!h.0.join("evil").exists(), "a path outside nerd's root is never written");
    }

    #[test]
    fn an_agent_whose_config_does_not_parse_is_skipped_and_said() {
        let home = scratch_home("broken");
        std::fs::write(home.join(".codex/hooks.json"), "{ not json").unwrap();
        let done = install_in(&home).unwrap();
        let codex = done.iter().find(|(n, _)| *n == "Codex").unwrap();
        assert!(codex.1.as_ref().is_err_and(|e| e.contains("does not parse")), "{codex:?}");
        assert!(!script(&builtin("codex"), &home).exists(), "nothing of ours is left beside a file we could not read");
        assert_eq!(std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap(), "{ not json");
        assert!(done.iter().filter(|(n, _)| *n != "Codex").all(|(_, r)| r.is_ok()), "the others still got theirs");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// What each agent's config gets, with HOME written as `/HOME`: the entries nerd writes, byte for byte.
    fn golden_texts(home: &Path) -> Vec<(String, String)> {
        let h = home.display().to_string();
        let mut out: Vec<(String, String)> = crate::agents::builtins()
            .iter()
            .filter(|a| a.shape() != Some(Shape::Plugin))
            .map(|a| (format!("{}.json", a.name), serde_json::to_string_pretty(&wanted(a, home).unwrap()).unwrap().replace(&h, "/HOME")))
            .collect();
        for name in ["opencode", "pi"] {
            out.push((format!("{name}.js"), plugin_for(&builtin(name), &state_root(home)).replace(&h, "/HOME")));
        }
        out
    }

    /// `NERD_BLESS=1 cargo test golden` rewrites them; a change to one must be deliberate.
    pub(crate) fn golden(name: &str, text: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(name);
        if std::env::var_os("NERD_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap_or_default(), text, "{} changed", path.display());
    }

    #[test]
    fn golden_hook_entries_stay_as_they_were() {
        let home = scratch_home("golden");
        for (name, text) in golden_texts(&home) {
            golden(&format!("hooks/{name}"), &text);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A fixture folder's payloads in order, each with the hook word (or plugin hook) its file is named after.
    fn payloads(dir: &Path) -> Vec<(String, String)> {
        let mut files: Vec<(Vec<u64>, String, PathBuf)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json") && p.file_stem().is_some_and(|s| s != "expect"))
            .map(|p| {
                let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
                let parts: Vec<&str> = stem.split('-').collect();
                let n = parts.iter().take_while(|x| x.parse::<u64>().is_ok()).count();
                (parts[..n].iter().map(|x| x.parse().unwrap()).collect(), parts[n..].join("-"), p)
            })
            .collect();
        files.sort();
        files.into_iter().map(|(_, word, p)| (word, std::fs::read_to_string(p).unwrap())).collect()
    }

    /// What a replay must leave: the state word, the session id, and each subagent row as (type, task, running).
    /// A recording holds one file per hook call, so an event several words answer comes once per word:
    /// each event once, in order.
    fn events_of(files: &[(String, String)]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, json) in files {
            if out.last() != Some(json) {
                out.push(json.clone());
            }
        }
        out
    }

    /// The words this build's manifest runs for a payload: its event by name (either case), and its matcher on the tool or notice.
    fn words_for(m: &Manifest, json: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(json).unwrap();
        let name = v["hook_event_name"].as_str().or(v["hookName"].as_str()).unwrap_or_default();
        let subject = [&v["tool_name"], &v["notification_type"]].into_iter().filter_map(Value::as_str).collect::<Vec<_>>();
        let words: Vec<String> = m
            .agent
            .events
            .iter()
            .filter(|e| e.on.eq_ignore_ascii_case(name) && e.matcher.as_deref().is_none_or(|x| subject.contains(&x)))
            .map(|e| e.word.clone())
            .collect();
        assert!(!words.is_empty() || name.is_empty(), "{}: {name} runs no word", m.name);
        words
    }

    /// What a replay must leave, and when `after` is given, the state word after each payload in turn.
    /// The state word, the session id, and each subagent row as (type, task, running).
    type Left = (String, String, Vec<(String, String, bool)>);

    fn expected(dir: &Path) -> (Left, Option<Vec<String>>) {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap()).unwrap();
        let mut subs: Vec<(String, String, bool)> = v["subagents"].as_array().unwrap().iter().map(|r| (r[0].as_str().unwrap().into(), r[1].as_str().unwrap().into(), r[2].as_bool().unwrap())).collect();
        subs.sort();
        let after = v["after"].as_array().map(|a| a.iter().map(|w| w.as_str().unwrap().to_string()).collect());
        ((v["state"].as_str().unwrap().into(), v["session"].as_str().unwrap().into(), subs), after)
    }

    fn replayed(h: &Hook) -> Left {
        let mut subs = kinds(&h.activity());
        subs.sort();
        (h.word(), h.file(".session").trim().to_string(), subs)
    }

    #[test]
    fn recorded_payloads_replay_through_the_real_script() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents");
        for m in crate::agents::builtins().iter().filter(|m| m.shape() != Some(Shape::Plugin)) {
            let dir = root.join(&m.name);
            let h = Hook::new(&format!("replay-{}", m.name));
            let mut after = Vec::new();
            for json in events_of(&payloads(&dir)) {
                for word in words_for(m, &json) {
                    h.run_as(&m.agent.arg, &word, &json);
                }
                after.push(h.word());
            }
            let (want, want_after) = expected(&dir);
            assert_eq!(replayed(&h), want, "{}", m.name);
            if let Some(w) = want_after {
                assert_eq!(after, w, "{}: the state after each payload", m.name);
            }
        }
    }

    #[test]
    fn recorded_opencode_calls_replay_through_the_plugin() {
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the OpenCode replay is skipped");
            return;
        };
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents/opencode");
        let h = Hook::new("replay-opencode");
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin("opencode"), &test_root())).unwrap();
        let calls: Vec<Value> = payloads(&dir).into_iter().map(|(hook, json)| json!([hook, serde_json::from_str::<Value>(&json).unwrap()])).collect();
        std::fs::write(h.0.join("calls.json"), Value::Array(calls).to_string()).unwrap();
        std::fs::write(
            h.0.join("run.mjs"),
            r#"
import { readFileSync } from "node:fs"
import { NerdPlugin } from "./nerd.mjs"
const p = await NerdPlugin()
const after = []
for (const [hook, x] of JSON.parse(readFileSync(new URL("./calls.json", import.meta.url), "utf8"))) {
  if (hook === "event") await p.event(x)
  else await p[hook](x.input, x.output)
  let word = ""
  try { word = readFileSync(process.env.NERD_STATE + "/7", "utf8").trim() } catch {}
  after.push(word)
}
console.log(JSON.stringify(after))
"#,
        )
        .unwrap();
        let out = std::process::Command::new(node).arg(h.0.join("run.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &h.0).env_remove("NERD_HOOK_DUMP").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let (want, want_after) = expected(&dir);
        assert_eq!(replayed(&h), want);
        let after: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        if let Some(w) = want_after {
            assert_eq!(after, w, "the state after each call");
        }
    }

    #[test]
    fn recorded_pi_calls_replay_through_the_extension() {
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the pi replay is skipped");
            return;
        };
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents/pi");
        let h = Hook::new("replay-pi");
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin("pi"), &test_root())).unwrap();
        let calls: Vec<Value> = payloads(&dir).into_iter().map(|(_, json)| serde_json::from_str::<Value>(&json).unwrap()).collect();
        std::fs::write(h.0.join("calls.json"), Value::Array(calls).to_string()).unwrap();
        // each record's ctx comes back as a fake one: the session file exists exactly when it did
        std::fs::write(
            h.0.join("run.mjs"),
            r#"
import { readFileSync, writeFileSync, rmSync } from "node:fs"
import ext from "./nerd.mjs"
const on = {}
ext({ on: (name, fn) => (on[name] = fn) })
const dir = process.env.NERD_STATE, file = dir + "/session.jsonl", after = []
for (const { event, ctx: c } of JSON.parse(readFileSync(new URL("./calls.json", import.meta.url), "utf8"))) {
  if (c.fileExists) writeFileSync(file, "{}\n")
  else rmSync(file, { force: true })
  const ctx = { mode: c.mode, isIdle: () => c.idle, sessionManager: { getSessionFile: () => file, getSessionId: () => c.sessionId } }
  on[event.type](event, ctx)
  let word = ""
  try { word = readFileSync(dir + "/7", "utf8").trim() } catch {}
  after.push(word)
}
console.log(JSON.stringify(after))
"#,
        )
        .unwrap();
        let out = std::process::Command::new(node).arg(h.0.join("run.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &h.0).env_remove("NERD_HOOK_DUMP").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let (want, want_after) = expected(&dir);
        assert_eq!(replayed(&h), want);
        let after: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(Some(after), want_after, "the state after each call");
    }

    /// Runs `script` as run.mjs beside the built extension; `env` replaces the pane variables.
    fn run_ext(h: &Hook, agent: &str, script: &str, env: &[(&str, &str)]) -> Option<Value> {
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the {agent} mailbox test is skipped");
            return None;
        };
        std::fs::create_dir_all(&h.0).unwrap();
        std::fs::write(h.0.join("nerd.mjs"), plugin_for(&builtin(agent), &test_root())).unwrap();
        std::fs::write(h.0.join("run.mjs"), script).unwrap();
        let out = std::process::Command::new(node).arg(h.0.join("run.mjs")).env("NERD_PANE", "7").env("NERD_STATE", &h.0).env_remove("NERD_HOOK_DUMP").envs(env.iter().copied()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout))))
    }

    const MAIL_HEAD: &str = r#"
import { writeFileSync, mkdirSync, readdirSync, existsSync } from "node:fs"
import * as mod from "./nerd.mjs"
const dir = process.env.NERD_STATE + "/7.mail"
mkdirSync(dir, { recursive: true })
const put = (id) => writeFileSync(`${dir}/${id}.json`, JSON.stringify({ id, frame: `[frame ${id}]`, text: `text ${id}` }))
const wait = (ms) => new Promise((r) => setTimeout(r, ms))
// A loaded machine is slow: wait for the outcome, not for a time.
const until = async (done, ms = 10000) => { for (const end = Date.now() + ms; !done() && Date.now() < end; ) await wait(20) }
const files = () => readdirSync(dir).sort()
"#;

    #[test]
    fn pi_delivers_a_mailbox_message_with_trigger_turn_when_idle_and_follow_up_when_busy() {
        let h = Hook::new("pi-mail");
        let script = format!(
            r#"{MAIL_HEAD}
const sent = [], on = {{}}
let idle = true
mod.default({{ on: (n, f) => (on[n] = f), sendMessage: (m, o) => (sent.push([m, o]), (idle = false)) }})
const ctx = {{ mode: "tui", isIdle: () => idle, sessionManager: {{}} }}
put("b2"); put("a1")
on.session_start({{ reason: "startup" }}, ctx)
await until(() => files().join() === "a1.sent,b2.sent")
console.log(JSON.stringify({{ sent, files: files() }}))
"#
        );
        let Some(r) = run_ext(&h, "pi", &script, &[]) else { return };
        let opts = |i: usize| r["sent"][i][1].clone();
        assert_eq!(r["sent"][0][0], json!({"customType": "nerd_message", "content": "[frame a1]\ntext a1", "display": true}), "the oldest first");
        assert_eq!(opts(0), json!({"triggerTurn": true}));
        assert_eq!(r["sent"][1][0]["content"], "[frame b2]\ntext b2");
        assert_eq!(opts(1), json!({"triggerTurn": false, "deliverAs": "followUp"}));
        assert_eq!(r["files"], json!(["a1.sent", "b2.sent"]));
    }

    #[test]
    fn pi_defers_while_a_prompt_is_open() {
        let h = Hook::new("pi-defer");
        let script = format!(
            r#"{MAIL_HEAD}
const sent = [], on = {{}}
mod.default({{ on: (n, f) => (on[n] = f), sendMessage: (m, o) => sent.push([m, o]) }})
const ctx = {{ mode: "tui", isIdle: () => true, sessionManager: {{}} }}
on.session_start({{ reason: "startup" }}, ctx)
on.ui_prompt_start({{}}, ctx)
put("a1")
await wait(1500)
const during = {{ sent: sent.length, files: files() }}
on.ui_prompt_end({{}}, ctx)
await until(() => files().includes("a1.sent"))
console.log(JSON.stringify({{ during, sent: sent.length, files: files() }}))
"#
        );
        let Some(r) = run_ext(&h, "pi", &script, &[]) else { return };
        assert_eq!(r["during"], json!({"sent": 0, "files": ["a1.json"]}));
        assert_eq!((r["sent"].clone(), r["files"].clone()), (json!(1), json!(["a1.sent"])));
    }

    #[test]
    fn opencode_prompts_the_main_session_and_renames_after_acceptance() {
        let h = Hook::new("oc-mail");
        let script = format!(
            r#"{MAIL_HEAD}
const calls = []
const client = {{ session: {{ prompt: async (x) => (calls.push([x, files()]), {{ data: {{}} }}) }} }}
const p = await mod.NerdPlugin({{ client }})
await p.event({{ event: {{ type: "session.created", properties: {{ info: {{ id: "m" }} }} }} }})
await p.event({{ event: {{ type: "session.created", properties: {{ info: {{ id: "c", parentID: "m", title: "t" }} }} }} }})
put("a1")
await until(() => files().includes("a1.sent"))
console.log(JSON.stringify({{ calls, files: files() }}))
"#
        );
        let Some(r) = run_ext(&h, "opencode", &script, &[]) else { return };
        assert_eq!(r["calls"].as_array().unwrap().len(), 1);
        assert_eq!(r["calls"][0][0], json!({"path": {"id": "m"}, "body": {"parts": [{"type": "text", "text": "[frame a1]\ntext a1"}]}}), "the main session, not its child");
        assert_eq!(r["calls"][0][1], json!(["a1.json"]), "still waiting while the call runs");
        assert_eq!(r["files"], json!(["a1.sent"]));
    }

    #[test]
    fn opencode_defers_while_a_permission_is_asked() {
        let h = Hook::new("oc-defer");
        let script = format!(
            r#"{MAIL_HEAD}
let calls = 0
const client = {{ session: {{ prompt: async () => (calls++, {{ data: {{}} }}) }} }}
const p = await mod.NerdPlugin({{ client }})
const ev = (type, properties) => p.event({{ event: {{ type, properties }} }})
await ev("session.created", {{ info: {{ id: "m" }} }})
await ev("permission.asked", {{ id: "q", sessionID: "m" }})
put("a1")
await wait(1500)
const during = {{ calls, files: files() }}
await ev("permission.replied", {{ requestID: "q" }})
await until(() => files().includes("a1.sent"))
console.log(JSON.stringify({{ during, calls, files: files() }}))
"#
        );
        let Some(r) = run_ext(&h, "opencode", &script, &[]) else { return };
        assert_eq!(r["during"], json!({"calls": 0, "files": ["a1.json"]}));
        assert_eq!((r["calls"].clone(), r["files"].clone()), (json!(1), json!(["a1.sent"])));
    }

    /// A message server at `dir/m.sock`: answers Hello, `{"ok":{}}` to writes, the first Take with one message.
    fn fake_msock(dir: &Path) -> (PathBuf, std::sync::Arc<std::sync::Mutex<Vec<String>>>, std::os::unix::net::UnixListener) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("m.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (path, seen, l)
    }

    fn serve_msock(l: std::os::unix::net::UnixListener, seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        std::thread::spawn(move || {
            let Ok((c, _)) = l.accept() else { return };
            let mut w = c.try_clone().unwrap();
            let mut taken = false;
            for line in BufReader::new(c).lines().map_while(Result::ok) {
                seen.lock().unwrap().push(line.clone());
                let reply = if line.starts_with(r#"{"Hello""#) {
                    json!({"ok": {"protocol": 1, "pane": 7, "name": "p"}})
                } else if line.starts_with(r#"{"Take""#) && !std::mem::replace(&mut taken, true) {
                    json!({"mail": {"id": "m1", "frame": "[frame m1]", "text": "hello"}})
                } else if line.starts_with(r#"{"Take""#) {
                    json!({"none": {}})
                } else {
                    json!({"ok": {}})
                };
                if writeln!(w, "{reply}").is_err() {
                    return;
                }
            }
        });
    }

    fn own_uid(dir: &Path) -> String {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).unwrap().uid().to_string()
    }

    fn socket_run(agent: &str, tag: &str, body: &str) {
        let dir = PathBuf::from(format!("/tmp/nt-s3b-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (path, seen, l) = fake_msock(&dir);
        serve_msock(l, seen.clone());
        let h = Hook::new(tag);
        let script = format!(
            r#"{MAIL_HEAD_SOCK}
{body}
"#
        );
        let uid = own_uid(&dir);
        let env = [("NERD_STATE", ""), ("NERD_MSOCK", path.to_str().unwrap()), ("NERD_MSOCK_UID", uid.as_str())];
        let Some(r) = run_ext(&h, agent, &script, &env) else { return };
        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines[0], r#"{"Hello":{"protocol":1}}"#);
        let has = |s: &str| lines.iter().any(|l| l == s);
        assert!(has(r#"{"Put":{"file":"","data":"working\n"}}"#), "{lines:?}");
        assert!(has(r#"{"Put":{"file":"session","data":"ses_1\n"}}"#) || agent == "pi", "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(r#"{"Append":{"file":"events","line":""#) && l.contains(r"\t-\tturn")), "{lines:?}");
        assert!(has(r#"{"Drop":{"file":"events"}}"#), "{lines:?}");
        assert!(lines.iter().filter(|l| l.starts_with(r#"{"Take""#)).count() >= 2, "{lines:?}");
        let took = lines.iter().position(|l| l == r#"{"Took":{"id":"m1"}}"#);
        assert!(took.is_some_and(|i| lines[..i].iter().any(|l| l.starts_with(r#"{"Take""#))), "the delivered message is confirmed after its Take: {lines:?}");
        assert_eq!(r["delivered"], json!(["[frame m1]\nhello"]), "the taken message, once");
        assert!(!h.0.join("7").exists() && !h.0.join("7.events").exists(), "nothing written as files");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const MAIL_HEAD_SOCK: &str = r#"
import * as mod from "./nerd.mjs"
const wait = (ms) => new Promise((r) => setTimeout(r, ms))
// A loaded machine is slow: wait for the outcome, not for a time.
const until = async (done, ms = 10000) => { for (const end = Date.now() + ms; !done() && Date.now() < end; ) await wait(20) }
"#;

    #[test]
    fn pi_socket_mode_reports_state_and_takes_mail() {
        socket_run(
            "pi",
            "pisock",
            r#"
const delivered = [], on = {}
mod.default({ on: (n, f) => (on[n] = f), sendMessage: (m) => delivered.push(m.content) })
const ctx = { mode: "tui", isIdle: () => true, sessionManager: {} }
on.session_start({ reason: "startup" }, ctx)
on.agent_start({}, ctx)
await until(() => delivered.length > 0)
// The next Take, a second after the first, says the mailbox is empty.
await wait(1500)
console.log(JSON.stringify({ delivered }))
"#,
        );
    }

    #[test]
    fn opencode_socket_mode_reports_state_and_takes_mail() {
        socket_run(
            "opencode",
            "ocsock",
            r#"
const delivered = []
// prompt answers when the turn ends, here never; promptAsync once it is accepted
const client = { session: { prompt: () => new Promise(() => {}), async promptAsync(x) { if (this !== client.session) throw 0; delivered.push(x.body.parts[0].text); return { data: {} } } } }
const p = await mod.NerdPlugin({ client })
await p.event({ event: { type: "session.created", properties: { info: { id: "ses_1" } } } })
await p["chat.message"]({ sessionID: "ses_1" })
await until(() => delivered.length > 0)
// The next Take, a second after the first, says the mailbox is empty.
await wait(1500)
console.log(JSON.stringify({ delivered }))
"#,
        );
    }

    #[test]
    fn socket_mode_refuses_a_tmp_directory_that_is_not_nerds() {
        use std::os::unix::fs::PermissionsExt;
        let base = PathBuf::from(format!("/tmp/nt-s3b-{}-bad", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let uid = own_uid(&base);
        let dir = base.join(format!("nerd-{uid}"));
        let (path, _seen, l) = fake_msock(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        l.set_nonblocking(true).unwrap();
        let h = Hook::new("badsock");
        let script = r#"
import ext from "./nerd.mjs"
const on = {}
ext({ on: (n, f) => (on[n] = f), sendMessage() {} })
on.session_start({ reason: "startup" }, { mode: "tui", isIdle: () => true, sessionManager: {} })
await new Promise((r) => setTimeout(r, 1500))
console.log("{}")
"#;
        let env = [("NERD_STATE", ""), ("NERD_MSOCK", path.to_str().unwrap()), ("NERD_MSOCK_UID", uid.as_str())];
        if run_ext(&h, "pi", script, &env).is_none() {
            return;
        }
        assert_eq!(l.accept().err().map(|e| e.kind()), Some(std::io::ErrorKind::WouldBlock), "no connection was made");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn nerd_hook_dump_keeps_each_payload_as_it_came() {
        use std::os::unix::fs::PermissionsExt;
        let h = Hook::new("dump");
        let dump = h.0.join("dump");
        let payload = r#"{"session_id":"s1","hook_event_name":"Stop"}"#;
        for (word, agent) in [("idle", Some("codex")), ("working", None)] {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", &script_for(&test_root()), "hook", word])
                .args(agent)
                .env("NERD_PANE", "7")
                .env("NERD_STATE", h.0.join("agents"))
                .env("NERD_HOOK_DUMP", &dump)
                .stdin(stdin_of(payload.as_bytes()))
                .spawn()
                .unwrap();
            assert!(child.wait().unwrap().success());
        }
        for (agent, word) in [("codex", "idle"), ("claude", "working")] {
            let files: Vec<PathBuf> = std::fs::read_dir(dump.join(agent)).unwrap().flatten().map(|e| e.path()).collect();
            assert_eq!(files.len(), 1, "{agent}: {files:?}");
            let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.ends_with(&format!("-{word}.json")) && name.split('-').next().unwrap().parse::<u64>().unwrap() > 1_700_000_000, "{name}");
            assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), payload);
            assert_eq!(std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(payloads(&dump.join(agent)), vec![(word.to_string(), payload.to_string())], "a recording replays as a fixture");
        }
        assert_eq!(h.file_in("agents", ""), "working\n", "the hook still did its work");
    }

    impl Hook {
        fn put_mail(&self, id: &str, line: &str) {
            std::fs::create_dir_all(self.0.join("7.mail")).unwrap();
            std::fs::write(self.0.join(format!("7.mail/{id}.out")), line).unwrap();
        }

        fn mail_as(&self, agent: &str, json: &str) -> String {
            self.say("mail", agent, json)
        }

        /// What the script prints for `word`.
        fn say(&self, word: &str, agent: &str, json: &str) -> String {
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", &script_for(&test_root()), "hook", word, agent])
                .env("NERD_PANE", "7")
                .env("NERD_STATE", &self.0)
                .stdin(stdin_of(json.as_bytes()))
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        }

        fn mail_files(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.0.join("7.mail")).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            v.sort();
            v
        }
    }

    #[test]
    fn the_turns_end_prints_the_line_for_each_form() {
        let h = Hook::new("mailforms");
        let framed = "[message from planner via nerd, id 01A; reply: nerd send --reply 01A \"…\"]\nfind \"X\"";
        for (agent, line) in [
            ("claude", json!({ "decision": "block", "reason": framed })),
            ("codex", json!({ "decision": "block", "reason": framed })),
            ("qwen", json!({ "decision": "block", "reason": framed })),
            ("copilot", json!({ "decision": "block", "reason": framed })),
            ("gemini", json!({ "decision": "deny", "reason": framed })),
            ("cursor", json!({ "followup_message": framed })),
        ] {
            let m = builtin(agent);
            assert!(m.agent.events.iter().all(|e| e.word != "mail"), "{agent}: the turn's end is one entry, so no idle lands after the hand-over");
            let end = m.agent.events.iter().find(|e| matches!(e.on.as_str(), "Stop" | "AfterAgent" | "stop")).unwrap();
            let line = format!("{line}\n");
            h.put_mail("01A", &line);
            assert_eq!(h.say(&end.word, m.from(), &json!({ "hook_event_name": end.on }).to_string()), line, "{agent}");
            assert_eq!(h.mail_files(), vec!["01A.sent"], "{agent}");
            assert_eq!(h.word(), "working", "{agent}: the agent goes on with the message");
            std::fs::remove_file(h.0.join("7.mail/01A.sent")).unwrap();
        }
    }

    #[test]
    fn a_session_start_or_a_block_chain_hands_nothing_over() {
        let h = Hook::new("mailstart");
        h.put_mail("01A", "x\n");
        assert_eq!(h.say("idle", "codex", r#"{"hook_event_name":"SessionStart"}"#), "");
        assert_eq!(h.say("idle", "cursor", r#"{"hook_event_name":"sessionStart"}"#), "");
        assert_eq!(h.say("idle", "codex", r#"{"hook_event_name":"Stop","stop_hook_active":true}"#), "");
        assert_eq!((h.word(), h.mail_files()), ("idle".to_string(), vec!["01A.out".to_string()]), "the pane is idle and the message waits");
    }

    #[test]
    fn stop_hook_active_prints_nothing() {
        let h = Hook::new("mailactive");
        h.put_mail("01A", "{\"decision\":\"block\",\"reason\":\"x\"}\n");
        assert_eq!(h.mail_as("codex", r#"{"hook_event_name":"Stop","stop_hook_active" :  true}"#), "");
        assert_eq!(h.mail_files(), vec!["01A.out"], "the message waits for the next turn's end");
        assert_eq!(h.mail_as("codex", r#"{"hook_event_name":"Stop","stop_hook_active":false}"#), "{\"decision\":\"block\",\"reason\":\"x\"}\n");
    }

    #[test]
    fn the_mail_branch_writes_working() {
        let h = Hook::new("mailworking");
        h.run_as("codex", "idle", "{}");
        assert_eq!(h.word(), "idle");
        assert_eq!(h.mail_as("codex", "{}"), "", "no message: nothing printed");
        assert_eq!(h.word(), "idle", "and the state stays");
        h.put_mail("01A", "{}\n");
        h.mail_as("codex", "{}");
        assert_eq!(h.word(), "working", "the agent goes on with the message");
    }

    #[test]
    fn one_message_per_call_oldest_first() {
        let h = Hook::new("mailorder");
        h.put_mail("01B", "second\n");
        h.put_mail("01A", "first\n");
        assert_eq!(h.mail_as("codex", "{}"), "first\n");
        assert_eq!(h.mail_files(), vec!["01A.sent", "01B.out"]);
        assert_eq!(h.mail_as("codex", "{}"), "second\n");
        assert_eq!(h.mail_as("codex", "{}"), "");
    }

    #[test]
    fn session_start_records_the_inbox_and_never_a_token() {
        let h = Hook::new("inbox");
        let run = |agent: &str, sock: &str, event: &str| {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", &script_for(&test_root()), "hook", "idle"])
                .args((!agent.is_empty()).then_some(agent))
                .env("NERD_PANE", "7")
                .env("NERD_STATE", &h.0)
                .env("CLAUDE_CODE_MESSAGING_SOCKET", sock)
                .env("CLAUDE_CODE_MESSAGING_TOKEN", "s3cr3t-token")
                .stdin(stdin_of(format!(r#"{{"hook_event_name":"{event}"}}"#).as_bytes()))
                .spawn()
                .unwrap();
            assert!(child.wait().unwrap().success());
        };
        run("", "/tmp/x.sock", "SessionStart");
        assert_eq!(h.file(".inbox"), "/tmp/x.sock\n");
        assert!(snapshot(&h.0).iter().all(|(_, body)| !body.contains("s3cr3t")), "no token anywhere");
        run("", "/tmp/y.sock", "Stop");
        assert_eq!(h.file(".inbox"), "/tmp/x.sock\n", "only a session's start records it");
        run("codex", "/tmp/y.sock", "SessionStart");
        assert_eq!(h.file(".inbox"), "/tmp/x.sock\n", "only an agent whose manifest names the variable");
        for bad in ["relative.sock", "/tmp/x.txt", ""] {
            run("", "/tmp/x.sock", "SessionStart");
            run("", bad, "SessionStart");
            assert!(!h.0.join("7.inbox").exists(), "{bad:?} removes it");
        }
    }

    #[test]
    fn socket_mode_execs_the_relay() {
        let h = Hook::new("relay");
        std::fs::create_dir_all(&h.0).unwrap();
        let bin = h.0.join("fake-nerd");
        std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s|' \"$@\" > '{0}/args'\ncat > '{0}/stdin'\n", h.0.display())).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let script = h.0.join("hook.sh");
        std::fs::write(&script, script_for(&test_root())).unwrap();
        let relayed = || {
            let got = (std::fs::read_to_string(h.0.join("args")).unwrap_or_default(), std::fs::read_to_string(h.0.join("stdin")).unwrap_or_default());
            let _ = std::fs::remove_file(h.0.join("args"));
            got
        };
        let mut child = std::process::Command::new("/bin/sh")
            .arg(&script)
            .args(["idle", "codex"])
            .env("NERD_PANE", "7")
            .env_remove("NERD_STATE")
            .env("NERD_MSOCK", "/tmp/nerd-x.msock")
            .env("NERD_BIN", &bin)
            .stdin(stdin_of(br#"{"hook_event_name":"Stop"}"#))
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(relayed(), ("--hook-relay|idle|codex|".to_string(), r#"{"hook_event_name":"Stop"}"#.to_string()));
        // a clean environment (Gemini's) finds the variables in an ancestor; macOS hides a system binary's from ps
        let Ok(node) = which("node") else {
            eprintln!("node is not on PATH: the ancestor half of the relay test is skipped");
            return;
        };
        let run = r#"require("child_process").spawnSync("/usr/bin/env", ["-u", "NERD_PANE", "-u", "NERD_MSOCK", "-u", "NERD_BIN", "/bin/sh", process.argv[1], "working", "gemini"], { input: "{}" })"#;
        let out = std::process::Command::new(node).args(["-e", run]).arg(&script).env("NERD_PANE", "7").env_remove("NERD_STATE").env("NERD_MSOCK", "/tmp/nerd-x.msock").env("NERD_BIN", &bin).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(relayed(), ("--hook-relay|working|gemini|".to_string(), "{}".to_string()));
    }

    #[test]
    fn run_for_runs_the_script_as_the_pane() {
        crate::session::tests::with_temp_state_dir("runfor", || {
            assert_eq!(run_for(9, "working", "codex", b"{}").unwrap(), "");
            assert_eq!(std::fs::read_to_string(state_dir().join("9")).unwrap(), "working\n");
            crate::mail::mailbox_put(9, "01A", "out", b"{\"decision\":\"block\",\"reason\":\"x\"}\n").unwrap();
            assert_eq!(run_for(9, "mail", "codex", b"{}").unwrap(), "{\"decision\":\"block\",\"reason\":\"x\"}\n");
            assert_eq!(crate::mail::mailbox_sent(9), vec!["01A".to_string()]);
            assert!(std::fs::read_dir(state_dir()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("relay")), "no payload left behind");
        });
    }

    #[test]
    fn clear_state_removes_the_mailbox_and_the_inbox() {
        crate::session::tests::with_temp_state_dir("clearmail", || {
            write_state(9, "idle").unwrap();
            std::fs::write(state_dir().join("9.inbox"), "/tmp/x.sock\n").unwrap();
            crate::mail::mailbox_put(9, "01A", "out", b"x").unwrap();
            clear_state(9);
            assert!(!state_dir().join("9").exists() && !state_dir().join("9.inbox").exists() && !crate::mail::mailbox(9).exists());
        });
    }

    #[test]
    fn a_file_nerd_writes_whole_is_never_someone_elses() {
        let home = scratch_home("foreign");
        let theirs = home.join(".copilot/hooks/nerd.json");
        std::fs::write(&theirs, r#"{"version":1,"hooks":{"x":[]}}"#).unwrap();
        let plugin = home.join(".config/opencode/plugins/nerd.js");
        std::fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        std::fs::write(&plugin, "export const Mine = 1\n").unwrap();
        let extension = home.join(".pi/agent/extensions/nerd.js");
        std::fs::create_dir_all(extension.parent().unwrap()).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();
        let done = install_in(&home).unwrap();
        for (title, name) in [("Copilot CLI", "copilot"), ("OpenCode", "opencode"), ("pi", "pi")] {
            let r = &done.iter().find(|(n, _)| n == title).unwrap().1;
            assert!(r.as_ref().is_err_and(|e| e.contains("not nerd's")), "{title}: {r:?}");
            assert!(!installed(&builtin(name), &home), "{title}");
        }
        uninstall_in(&home).unwrap();
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), r#"{"version":1,"hooks":{"x":[]}}"#, "an uninstall leaves it too");
        assert_eq!(std::fs::read_to_string(&plugin).unwrap(), "export const Mine = 1\n");
        assert_eq!(std::fs::read_to_string(&extension).unwrap(), "export default function () {}\n");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_user_manifest_aimed_at_a_file_that_is_not_json_writes_nothing() {
        let home = scratch_home("vim");
        std::fs::create_dir_all(home.join(".vim")).unwrap();
        std::fs::write(home.join(".vim/vimrc"), "set nu\n").unwrap();
        let m: Manifest = serde_json::from_value(json!({ "name": "vim", "title": "V", "agent": { "names": ["vim"], "config": { "path": "~/.vim/vimrc", "shape": "nested" }, "arg": "vim", "events": [{ "on": "x", "do": "idle" }] } })).unwrap();
        assert!(crate::agents::validate(&m, &crate::agents::builtins()).is_ok(), "the path rule alone lets it through");
        assert!(install_one(&m, &home).is_err_and(|e| e.to_string().contains("does not parse")));
        assert_eq!(std::fs::read_to_string(home.join(".vim/vimrc")).unwrap(), "set nu\n");
        assert!(!script(&m, &home).exists() && !home.join(".vim/vimrc.json.bak-nerd").exists(), "nothing of ours beside it");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_command_line_asks_with_the_same_table() {
        let row = |r: [&str; 3]| r.map(String::from);
        let ask = Ask { rows: vec![row(["Qwen Code", "~/.qwen/settings.json", "qwen --resume <id>"]), row(["Cursor", "~/.cursor/hooks.json", "no resume"])], changed: false };
        assert_eq!(
            ask.prompt(),
            "nerd adds its hook to these agents, so it sees when they work and when they wait for you.\n\n\
             Agent      Settings file          Resumes with\n\
             Qwen Code  ~/.qwen/settings.json  qwen --resume <id>\n\
             Cursor     ~/.cursor/hooks.json   no resume\n\
             \nInstall? [y/N] "
        );
        assert!(Ask { changed: true, ..ask }.prompt().starts_with("The resume line of these agents changed since you confirmed it;"));
    }

    #[test]
    fn a_changed_resume_line_is_asked_about_again() {
        let home = scratch_home("seen");
        let seen = home.join("confirmed.json");
        assert_eq!(first_installs_in(&home, &seen).rows.len(), crate::agents::builtins().len(), "every agent found is asked about");
        assert!(!first_installs_in(&home, &seen).changed);
        install_in(&home).unwrap();
        assert!(first_installs_in(&home, &seen).is_empty(), "hooks an older nerd installed count as confirmed");
        assert_eq!(read_seen(&seen)["codex"], "codex resume <id>", "and their lines are recorded");
        let mut changed = read_seen(&seen);
        changed.insert("codex".into(), json!("codex resume --yolo <id>"));
        write_seen(&seen, &changed);
        let ask = first_installs_in(&home, &seen);
        assert_eq!(ask.rows, vec![["Codex", "~/.codex/hooks.json", "codex resume <id>"].map(String::from)]);
        assert!(ask.changed && ask.intro().starts_with("The resume line of these agents changed"), "the re-ask says why");
        assert!(!resume_seen(&builtin("codex"), &home, &seen), "not typed until confirmed");
        record_seen(&home, &seen);
        assert!(first_installs_in(&home, &seen).is_empty() && resume_seen(&builtin("codex"), &home, &seen), "confirming records it");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_user_agents_session_comes_from_nerds_file_and_the_script_stays_as_it_is() {
        let h = Hook::new("usersession");
        let root = h.0.join("root");
        std::fs::create_dir_all(root.join("agents")).unwrap();
        let before = script_for(&format!("{}/", root.display()));
        std::fs::write(root.join(SESSIONS_FILE), "fake sid word\nbad a.b word\n").unwrap();
        assert_eq!(script_for(&format!("{}/", root.display())), before, "a user's agent changes no installed script");
        let run = |agent: &str, json: &str| {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", &before, "hook", "idle", agent])
                .env("NERD_PANE", "7")
                .env("NERD_STATE", root.join("agents"))
                .stdin(stdin_of(json.as_bytes()))
                .spawn()
                .unwrap();
            assert!(child.wait().unwrap().success());
            std::fs::read_to_string(root.join("agents/7.session")).unwrap_or_default()
        };
        assert_eq!(run("bad", r#"{"aXb":"x1"}"#), "", "a field that is not a plain name is never used: a.b would match aXb");
        assert_eq!(run("fake", r#"{"sid":"-rf"}"#), "", "an id that reads as a flag");
        assert_eq!(run("fake", r#"{"sid":"fk-1"}"#).trim(), "fk-1");
        assert_eq!(run("f.*", r#"{"sid":"fk-2"}"#).trim(), "fk-1", "an agent name that is not a plain token stops the script");
    }

    fn which(name: &str) -> Result<PathBuf, ()> {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|p| p.is_file())).ok_or(())
    }

    #[test]
    fn script_ignores_calls_outside_nerd() {
        assert!(SCRIPT.contains(r#"[ -n "$NERD_PANE" ] && [ -n "$NERD_STATE" ] || exit 0"#));
        assert!(SCRIPT.contains("umask 077"));
    }
}
