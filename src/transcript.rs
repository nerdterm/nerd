//! A Claude Code subagent's transcript, `~/.claude/projects/*/<session>/subagents/agent-<id>.jsonl`,
//! read on a worker by byte offset, only the lines added since, as display lines for its run.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::layout::PaneId;

/// Read no further into one transcript; the view says so when it stops here.
pub const CAP: u64 = 32 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Bold,
    Muted,
    Ok,
    Bad,
}

pub type Row = Vec<(String, Tone)>;

/// What the run holds: the rows read so far and where the next read starts.
#[derive(Default)]
pub struct Transcript {
    pub offset: u64,
    pub rows: Vec<Row>,
    pub widest: usize,
    pub calls: usize,
    pub full: bool,
}

impl Transcript {
    /// Takes a read; false when it starts anywhere but where this one ended, or at 0 for a file that shrank.
    pub fn take(&mut self, f: Found) -> bool {
        if f.from == 0 {
            *self = Self::default();
        } else if f.from != self.offset {
            return false;
        }
        self.offset = f.to;
        self.widest = self.widest.max(f.rows.iter().map(width).max().unwrap_or(0));
        self.calls += f.calls;
        self.full = f.full;
        self.rows.extend(f.rows);
        true
    }
}

fn width(r: &Row) -> usize {
    r.iter().map(|(t, _)| t.chars().count()).sum()
}

pub struct Ask {
    pub pane: PaneId,
    pub id: String,
    pub session: String,
    pub from: u64,
}

pub struct Found {
    pub pane: PaneId,
    pub id: String,
    pub from: u64,
    pub to: u64,
    pub rows: Vec<Row>,
    pub calls: usize,
    pub full: bool,
}

/// Runs on the transcript worker: only the newest ask counts.
pub fn work(asks: Vec<Ask>) -> Vec<Found> {
    let root = crate::paths::home().map(|h| h.join(".claude/projects"));
    asks.into_iter().last().zip(root).and_then(|(a, root)| read(&root, a)).into_iter().collect()
}

pub fn read(root: &Path, a: Ask) -> Option<Found> {
    let path = find(root, &a.session, &a.id)?;
    let len = std::fs::metadata(&path).ok()?.len();
    let from = if len < a.from { 0 } else { a.from };
    let end = len.min(CAP);
    let full = len > CAP;
    if end <= from && from == a.from {
        return None;
    }
    let mut bytes = Vec::new();
    let mut f = std::fs::File::open(&path).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    f.take(end.saturating_sub(from)).read_to_end(&mut bytes).ok()?;
    // a line still being written waits for its newline
    bytes.truncate(bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
    let to = from + bytes.len() as u64;
    if to == from && from == a.from {
        return None;
    }
    let (rows, calls) = parse(&bytes, from == 0);
    Some(Found { pane: a.pane, id: a.id, from, to, rows, calls, full })
}

/// The transcript of subagent `id` of `session`, only as a regular `.jsonl` file under `root`.
pub fn find(root: &Path, session: &str, id: &str) -> Option<PathBuf> {
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !word(session) || !word(id) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    std::fs::read_dir(&root).ok()?.flatten().find_map(|e| {
        let p = e.path().join(session).join("subagents").join(format!("agent-{id}.jsonl")).canonicalize().ok()?;
        (p.starts_with(&root) && p.extension().is_some_and(|x| x == "jsonl") && p.metadata().ok()?.is_file()).then_some(p)
    })
}

/// Rows from complete JSONL lines; `first`: these start the file, so its first plain user message is the prompt.
pub fn parse(bytes: &[u8], first: bool) -> (Vec<Row>, usize) {
    let mut rows = Vec::new();
    let mut calls = 0;
    let mut prompt = first;
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let content = &v["message"]["content"];
        match (v["type"].as_str(), content) {
            (Some("user"), Value::String(s)) => {
                if std::mem::take(&mut prompt) {
                    block(&mut rows, "> ", s, Tone::Text);
                }
            }
            (Some("user"), Value::Array(items)) => {
                for r in items.iter().filter(|i| i["type"] == "tool_result") {
                    result(&mut rows, r, &v["toolUseResult"]);
                }
            }
            (Some("assistant"), Value::Array(items)) => {
                for i in items {
                    match i["type"].as_str() {
                        Some("text") => block(&mut rows, "", i["text"].as_str().unwrap_or(""), Tone::Text),
                        Some("thinking") => block(&mut rows, "\u{273b} ", i["thinking"].as_str().unwrap_or(""), Tone::Muted),
                        Some("tool_use") => {
                            calls += 1;
                            let mut row = vec![("\u{25cf} ".to_string(), Tone::Muted), (clean(i["name"].as_str().unwrap_or("?")), Tone::Bold)];
                            if let Some(s) = summary(&i["input"]) {
                                row.push((format!("({s})"), Tone::Text));
                            }
                            rows.push(row);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (rows, calls)
}

/// A message, its first line after `mark` and the rest under it; blank ends dropped.
fn block(rows: &mut Vec<Row>, mark: &str, text: &str, tone: Tone) {
    let text = text.trim_matches('\n');
    if text.trim().is_empty() {
        return;
    }
    let pad = " ".repeat(mark.chars().count());
    for (n, l) in text.lines().enumerate() {
        rows.push(vec![(format!("{}{}", if n == 0 { mark } else { &pad }, clean(l)), tone)]);
    }
}

/// What a call gave back: a diff as its hunks, a new file as added lines, a command's output, else the result's text.
fn result(rows: &mut Vec<Row>, r: &Value, tur: &Value) {
    let mut out: Vec<(String, Tone)> = Vec::new();
    let lines = |s: &str, tone| s.trim_end_matches('\n').lines().map(|l| (clean(l), tone)).collect::<Vec<_>>();
    let patch = tur["structuredPatch"].as_array().filter(|p| !p.is_empty());
    if r["is_error"] == true {
        out = lines(&text_of(&r["content"]), Tone::Bad);
    } else if let Some(hunks) = patch {
        for h in hunks {
            let n = |k: &str| h[k].as_u64().unwrap_or(0);
            out.push((format!("@@ -{},{} +{},{} @@", n("oldStart"), n("oldLines"), n("newStart"), n("newLines")), Tone::Muted));
            for l in h["lines"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let tone = match l.chars().next() {
                    Some('+') => Tone::Ok,
                    Some('-') => Tone::Bad,
                    _ => Tone::Muted,
                };
                out.push((clean(l), tone));
            }
        }
    } else if let (Some("create"), Some(c)) = (tur["type"].as_str(), tur["content"].as_str()) {
        out = c.trim_end_matches('\n').lines().map(|l| (format!("+{}", clean(l)), Tone::Ok)).collect();
    } else if tur["stdout"].is_string() || tur["stderr"].is_string() {
        out = lines(tur["stdout"].as_str().unwrap_or(""), Tone::Muted);
        out.extend(lines(tur["stderr"].as_str().unwrap_or(""), Tone::Bad));
    } else {
        out = lines(&text_of(&r["content"]), Tone::Muted);
    }
    if out.is_empty() {
        out.push(("(No output)".into(), Tone::Muted));
    }
    for (n, (t, tone)) in out.into_iter().enumerate() {
        rows.push(vec![(format!("{}{t}", if n == 0 { "  \u{23bf} " } else { "    " }), tone)]);
    }
}

/// A result's content: a string, or its text blocks.
fn text_of(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The call's target, as the hook picks it, then what other tools name; its first line.
fn summary(input: &Value) -> Option<String> {
    let s = ["file_path", "absolute_path", "command", "pattern", "description", "url", "query", "path", "prompt"].iter().find_map(|k| input[*k].as_str().filter(|s| !s.trim().is_empty()))?;
    let mut lines = s.trim().lines();
    let head = clean(lines.next().unwrap_or(""));
    Some(if lines.next().is_some() { format!("{head}\u{2026}") } else { head })
}

fn clean(s: &str) -> String {
    crate::render::unspoofed(&s.replace('\t', "    "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/transcript/agent.jsonl");

    fn plain(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.iter().map(|(t, _)| t.as_str()).collect()).collect()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nerd-transcript-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_real_transcript_reads_as_claude_code_shows_it() {
        let (rows, calls) = parse(FIXTURE.as_bytes(), true);
        let text = plain(&rows);
        let at = |s: &str| text.iter().position(|l| l.starts_with(s)).unwrap_or_else(|| panic!("{s:?} not in {text:#?}"));
        assert_eq!(calls, 5);
        assert_eq!(at("> Make the agent hook fast"), 0, "the prompt first");
        assert!(!text.iter().any(|l| l.contains("system-reminder") || l.contains("2026-09-26")), "later plain user lines and attachments are not the run's");
        let refused = at("\u{25cf} Bash(git fetch");
        assert!(text[refused + 1].starts_with("  \u{23bf} Permission to use Bash") && rows[refused + 1][0].1 == Tone::Bad, "an error in its tone");
        assert!(at("Plain commands, one at a time.") > refused);
        let think = at("\u{273b} The fake directory");
        assert_eq!(rows[think][0].1, Tone::Muted);
        let log = at("\u{25cf} Bash(/usr/bin/git log --oneline -2)");
        assert_eq!(text[log + 1], "  \u{23bf} a1758ab fix: the agent hook stays fast on a large tool output");
        assert!(text[log + 2].starts_with("    9c04e2b fix:"), "the rest of the output under it");
        let read = at("\u{25cf} Read(/Users/user/dev/projects/nerd/");
        assert_eq!(text[read + 1], "  \u{23bf} 330            if installed(a, home) && !current(a, home) {", "a tab as spaces");
        let write = at("\u{25cf} Write(");
        assert_eq!((text[write + 1].as_str(), rows[write + 1][0].1), ("  \u{23bf} +#!/bin/bash", Tone::Ok), "a new file as added lines");
        let edit = at("\u{25cf} Edit(");
        assert_eq!(text[edit + 1], "  \u{23bf} @@ -2,7 +2,7 @@");
        let minus = at("    -awk");
        assert_eq!((rows[minus][0].1, rows[minus + 1][0].1), (Tone::Bad, Tone::Ok));
        assert!(text[minus + 1].starts_with("    +awk") && rows[minus - 1][0].1 == Tone::Muted);
        assert!(refused < log && log < read && read < write && write < edit, "in the order they ran");
        assert!(text.iter().all(|l| !l.contains("/Users/") || l.contains("/Users/user/")), "home paths are a placeholder");
    }

    #[test]
    fn broken_lines_are_skipped_and_only_the_first_plain_message_is_the_prompt() {
        let bytes = b"{\"type\":\"user\",\"message\":{\"content\":\"one\\ntwo\"}}\nnot json\n{\"type\":\"user\",\"message\":{\"content\":\"later\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"thinking\",\"thinking\":\"\"},{\"type\":\"tool_use\",\"name\":\"Grep\",\"input\":{}}]}}\n{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":[{\"type\":\"text\",\"text\":\"\"}]}]}}\n";
        assert_eq!(plain(&parse(bytes, true).0), ["> one", "  two", "\u{25cf} Grep", "  \u{23bf} (No output)"]);
        assert_eq!(plain(&parse(bytes, false).0)[0], "\u{25cf} Grep", "a tail is past the prompt");
    }

    #[test]
    fn the_tail_reads_only_complete_new_lines_and_starts_over_when_the_file_shrinks() {
        let root = scratch("tail");
        let dir = root.join("-proj/s1/subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("agent-a1.jsonl");
        let ask = |from| Ask { pane: 1, id: "a1".into(), session: "s1".into(), from };
        let lines: Vec<&str> = FIXTURE.lines().collect();
        let head = format!("{}\n", lines[..8].join("\n"));
        std::fs::write(&file, &head).unwrap();
        let mut t = Transcript::default();
        let f = read(&root, ask(0)).unwrap();
        assert_eq!(f.to, head.len() as u64);
        assert!(t.take(f));
        let before = t.rows.len();
        assert!(read(&root, ask(t.offset)).is_none(), "nothing new, nothing read");

        let rest = format!("{}\n", lines[8..].join("\n"));
        let cut = rest[..rest.len() - 1].rfind('\n').unwrap();
        std::fs::write(&file, format!("{head}{}", &rest[..cut + 5])).unwrap();
        let f = read(&root, ask(t.offset)).unwrap();
        assert_eq!(f.to, (head.len() + cut + 1) as u64, "the half-written line waits");
        let stale = Found { rows: vec![vec![("x".into(), Tone::Text)]], ..read(&root, ask(t.offset)).unwrap() };
        assert!(t.take(f));
        assert!(!t.take(stale), "an answer to an older ask is dropped");
        assert!(read(&root, ask(t.offset)).is_none(), "only a half-written line: nothing to redraw");
        std::fs::write(&file, format!("{head}{rest}")).unwrap();
        assert!(t.take(read(&root, ask(t.offset)).unwrap()));
        assert_eq!(plain(&t.rows), plain(&parse(FIXTURE.as_bytes(), true).0), "read in three goes, as in one");
        assert!(t.rows.len() > before);

        std::fs::write(&file, &head).unwrap();
        let f = read(&root, ask(t.offset)).unwrap();
        assert_eq!((f.from, f.to), (0, head.len() as u64), "a shorter file is read again from its start");
        assert!(t.take(f));
        assert_eq!(t.rows.len(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_path_is_derived_and_only_a_jsonl_file_under_the_root_counts() {
        let root = scratch("path");
        let outside = scratch("path-out");
        let sub = root.join("-proj/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(find(&root, "s1", "a1"), None, "not there yet");
        std::fs::write(sub.join("agent-a1.jsonl"), "").unwrap();
        assert_eq!(find(&root, "s1", "a1"), Some(sub.join("agent-a1.jsonl").canonicalize().unwrap()), "there now");
        std::fs::create_dir_all(sub.join("agent-d1.jsonl")).unwrap();
        assert_eq!(find(&root, "s1", "d1"), None, "a directory");
        std::fs::write(outside.join("x.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(outside.join("x.jsonl"), sub.join("agent-l1.jsonl")).unwrap();
        assert_eq!(find(&root, "s1", "l1"), None, "a link out of the root");
        std::os::unix::fs::symlink(outside.join("x.txt"), sub.join("agent-t1.jsonl")).unwrap();
        std::fs::write(outside.join("x.txt"), "").unwrap();
        assert_eq!(find(&root, "s1", "t1"), None, "not a jsonl in the end");
        for (s, id) in [("..", "a1"), ("s1/../s1", "a1"), ("s1", "../a1"), ("", "a1"), ("s1", "")] {
            assert_eq!(find(&root, s, id), None, "{s:?} {id:?}");
        }
        assert_eq!(find(Path::new("relative"), "s1", "a1"), None);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_long_transcript_is_read_once_and_then_only_what_grows() {
        let root = scratch("big");
        let dir = root.join("-proj/s1/subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("agent-a1.jsonl");
        let body = FIXTURE.repeat(4 << 20 >> 13);
        std::fs::write(&file, &body).unwrap();
        let ask = |from| Ask { pane: 1, id: "a1".into(), session: "s1".into(), from };
        let mut t = Transcript::default();
        assert!(t.take(read(&root, ask(0)).unwrap()));
        assert_eq!(t.offset, body.len() as u64);
        let one = FIXTURE.lines().nth(5).unwrap();
        std::fs::OpenOptions::new().append(true).open(&file).and_then(|mut f| std::io::Write::write_all(&mut f, format!("{one}\n").as_bytes())).unwrap();
        let f = read(&root, ask(t.offset)).unwrap();
        assert_eq!((f.from, f.to - f.from), (body.len() as u64, one.len() as u64 + 1), "the tail is the new line alone");
        assert_eq!(plain(&f.rows), ["Plain commands, one at a time."]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
