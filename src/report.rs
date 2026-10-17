//! A report for a bug: the logs of a period and crash summaries in one text file the human reads before sending.

use std::path::{Path, PathBuf};

/// Writes the report of the last `hours` into the state root; `peers` are the hosts of clients the server knows. Nothing is sent anywhere.
pub fn write(hours: u64, cfg: &crate::config::Config, peers: &[String]) -> std::io::Result<PathBuf> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let root = crate::paths::state_root();
    let home = crate::paths::home().unwrap_or_default();
    let (logs, crashes) = build(&root, &home.join("Library/Logs/DiagnosticReports"), Path::new("/Library/Logs/DiagnosticReports"), now.saturating_sub(hours.saturating_mul(3600)), now);
    let mut words: Vec<(String, &str)> = Vec::new();
    for p in peers {
        words.push((p.trim_end_matches(".local").to_string(), "machine"));
        words.push((p.clone(), "machine"));
    }
    if let Ok(h) = nix::unistd::gethostname() {
        let h = h.to_string_lossy().into_owned();
        words.push((h.trim_end_matches(".local").to_string(), "host"));
        words.push((h, "host"));
    }
    for r in cfg.remote_entries() {
        words.push((r.name, "remote"));
        for part in r.host.split('@').chain([r.host.as_str()]) {
            words.push((part.to_string(), "remote"));
        }
    }
    let home = home.to_string_lossy();
    // Crash text keeps long runs: Rust frame symbols are shaped like tokens.
    let text = redact(&logs, &home, &words, true) + &redact(&crashes, &home, &words, false);
    let dir = root.join("reports");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("nerd-report-{}.log", local(now).replace(['-', ':'], "").replace(' ', "-")));
    crate::paths::write_private(&path, "tmp", text.as_bytes())?;
    Ok(path)
}

/// The header with the log lines at or after `since` from the root and every instance, and the crash and Jetsam summaries.
pub fn build(root: &Path, crash_dir: &Path, jetsam_dir: &Path, since: u64, now: u64) -> (String, String) {
    let mut out = format!(
        "{}\nmacOS {}\nPeriod: {} to {}\n\
         Redacted: user folders, the host name, remote machines, other Macs that connected, the hosts of opened links, IPv4 addresses, and anything shaped like a key or token.\n\
         Not redacted: folder and file names, and a machine named in a line other than a client's end. Read it before you send it.\n",
        crate::version_line(),
        macos_version(),
        local(since),
        local(now)
    );
    let mut dirs = vec![("main".to_string(), root.to_path_buf())];
    let mut instances: Vec<_> = std::fs::read_dir(root.join("instances"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    instances.sort_by_key(|n| (n.len(), n.clone()));
    dirs.extend(instances.into_iter().map(|n| (format!("server {n}"), root.join("instances").join(&n))));
    for (name, dir) in dirs {
        let mut lines = String::new();
        for file in ["daemon.log.old", "daemon.log"] {
            for line in std::fs::read_to_string(dir.join(file)).unwrap_or_default().lines() {
                let Some(l) = crate::log::parse(line) else { continue };
                if l.secs >= since as i64 {
                    lines += &if l.old { format!("{} INFO  ? {}\n", crate::log::stamp(l.secs, 0), l.text) } else { format!("{line}\n") };
                }
            }
        }
        if !lines.is_empty() {
            out += &format!("\n== {name}\n{lines}");
        }
    }
    // Terminal apps the clients attached under: their crash reports belong to the period too.
    let terminals: Vec<String> = out.lines().filter_map(|l| l.split(" under ").nth(1)?.split(" pid ").next()).map(|n| n.to_lowercase()).collect();
    let mut crashes = reports(crash_dir, since, |n| n.contains("nerd") || terminals.iter().any(|t| !t.is_empty() && n.to_lowercase().starts_with(t.as_str())));
    crashes.sort();
    let mut crash = String::new();
    for path in crashes {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        crash += &format!("\n== crash {name}\n");
        crash += &std::fs::read_to_string(&path).ok().and_then(|t| crash_summary(&t)).unwrap_or_else(|| format!("{name}: unreadable\n"));
    }
    let mut jetsams = reports(jetsam_dir, since, |n| n.starts_with("JetsamEvent-"));
    jetsams.sort();
    for path in jetsams {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        crash += &format!("\n== jetsam {name}\n");
        crash += &std::fs::read_to_string(&path).ok().and_then(|t| jetsam_summary(&t)).unwrap_or_else(|| format!("{name}: unreadable\n"));
    }
    (out, crash)
}

/// The `.ips` files in `dir` modified at or after `since` whose name passes `keep`.
fn reports(dir: &Path, since: u64, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".ips") && keep(&n)
        })
        .filter(|e| e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.duration_since(std::time::UNIX_EPOCH).is_ok_and(|d| d.as_secs() >= since)))
        .map(|e| e.path())
        .collect()
}

/// `<name>-<16 lowercase hex>`: how cargo names a test binary.
fn is_test_binary(name: &str) -> bool {
    name.rsplit_once('-').is_some_and(|(n, h)| !n.is_empty() && h.len() == 16 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// A Jetsam `.ips` file is a one-line JSON header followed by a JSON body.
fn jetsam_summary(text: &str) -> Option<String> {
    let (_, body) = text.split_once('\n')?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("?").to_string();
    let killed: Vec<String> = v["processes"].as_array().into_iter().flatten().filter(|p| p.get("reason").is_some()).map(|p| format!("{} ({})", s(&p["name"]), s(&p["reason"]))).collect();
    let killed = if killed.is_empty() { "none".to_string() } else { killed.join(", ") };
    Some(format!("date: {}, largest process: {}, killed: {killed}\n", s(&v["date"]), s(&v["largestProcess"])))
}

/// An `.ips` file is a one-line JSON header followed by a JSON body.
fn crash_summary(text: &str) -> Option<String> {
    let (_, body) = text.split_once('\n')?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("?").to_string();
    let mut out = format!("time: {}\npath: {}\n", s(&v["captureTime"]), s(&v["procPath"]));
    out += &format!("exception: {} {}\n", s(&v["exception"]["type"]), s(&v["exception"]["signal"]));
    if let Some(proc) = v["parentProc"].as_str() {
        out += &format!("parent: {proc} pid {}{}\n", v["parentPid"], if is_test_binary(proc) { " (a test binary)" } else { "" });
    }
    if let Some(ind) = v["termination"]["indicator"].as_str() {
        out += &format!("termination: {ind}\n");
    }
    for msgs in v["asi"].as_object().into_iter().flat_map(|o| o.values()) {
        for m in msgs.as_array().into_iter().flatten() {
            out += &format!("asi: {}\n", s(m));
        }
    }
    let thread = &v["threads"][v["faultingThread"].as_u64().unwrap_or(0) as usize];
    for f in thread["frames"].as_array().into_iter().flatten().take(12) {
        let frame = match f["symbol"].as_str() {
            Some(sym) => sym.to_string(),
            None => format!("{} + {:#x}", s(&v["usedImages"][f["imageIndex"].as_u64().unwrap_or(0) as usize]["name"]), f["imageOffset"].as_u64().unwrap_or(0)),
        };
        out += &format!("  {frame}\n");
    }
    Some(out)
}

fn macos_version() -> String {
    let plist = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").unwrap_or_default();
    let key = |k: &str| {
        let after = plist.split(&format!("<key>{k}</key>")).nth(1)?;
        Some(after.split("<string>").nth(1)?.split("</string>").next()?.to_string())
    };
    format!("{} ({})", key("ProductVersion").unwrap_or("?".into()), key("ProductBuildVersion").unwrap_or("?".into()))
}

/// `YYYY-MM-DD HH:MM:SS` in local time.
fn local(ts: u64) -> String {
    crate::log::stamp(ts as i64, 0)[..19].replace('T', " ")
}

/// Home as `~`, any user folder as `user`, each whole word as its replacement, and a client's machine in an `ended:` line;
/// with `tokens`, IPv4 addresses and long runs holding a letter and a digit as `[redacted]` too.
pub fn redact(text: &str, home: &str, words: &[(String, &str)], tokens: bool) -> String {
    let mut out = if home.len() > 1 { replace_word(text, home, "~") } else { text.to_string() };
    for base in ["/Users/", "/home/"] {
        out = user_folders(&out, base);
    }
    let mut words: Vec<_> = words.iter().filter(|(w, _)| !w.is_empty()).collect();
    words.sort_by_key(|(w, _)| std::cmp::Reverse(w.len()));
    for (word, with) in words {
        out = replace_word(&out, word, with);
    }
    let out: String = out.split_inclusive('\n').map(ended_machine).map(|l| opened_host(&l)).collect();
    if !tokens {
        return out;
    }
    let out = runs(&out, |c| c.is_ascii_digit() || c == '.', |run| {
        let ip = run.trim_end_matches('.');
        let parts: Vec<_> = ip.split('.').collect();
        (parts.len() == 4 && parts.iter().all(|p| (1..=3).contains(&p.len()) && p.parse::<u8>().is_ok()))
            .then(|| format!("[redacted]{}", &run[ip.len()..]))
    });
    runs(&out, |c| c.is_ascii_alphanumeric() || "_+=-".contains(c), |run| {
        (run.len() >= 24 && run.chars().any(|c| c.is_ascii_alphabetic()) && run.chars().any(|c| c.is_ascii_digit())).then(|| "[redacted]".to_string())
    })
}

/// `/Users/<name>` as `/Users/user`, whoever's folder it is; `Shared` is nobody's.
fn user_folders(text: &str, base: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(base) {
        out += &rest[..i + base.len()];
        rest = &rest[i + base.len()..];
        let n = rest.find(|c: char| !(is_word(c) || c == '.')).unwrap_or(rest.len());
        out += if n == 0 || &rest[..n] == "Shared" { &rest[..n] } else { "user" };
        rest = &rest[n..];
    }
    out + rest
}

/// `link opened in the browser: <scheme>://<host>`: the host says what the human looked at, so a report keeps only the scheme.
fn opened_host(line: &str) -> String {
    let marker = "link opened in the browser: ";
    let Some(i) = line.find(marker) else { return line.to_string() };
    let rest = &line[i + marker.len()..];
    let scheme = rest.split("://").next().unwrap_or("");
    let end = if line.ends_with('\n') { "\n" } else { "" };
    format!("{}{marker}{scheme}://[host]{end}", &line[..i])
}

/// `ended: <since> <machine> ...` from `clients::summary`: since is `HH:MM`, `Mon D HH:MM`, `earlier` or `?`.
fn ended_machine(line: &str) -> String {
    let Some(i) = line.find("ended: ") else { return line.to_string() };
    let (head, tail) = line.split_at(i + "ended: ".len());
    let parts: Vec<&str> = tail.splitn(5, ' ').collect();
    let skip = match parts.first() {
        Some(t) if t.contains(':') || *t == "earlier" || *t == "?" => 1,
        Some(t) if t.len() == 3 && parts.get(2).is_some_and(|t| t.contains(':')) => 3,
        _ => return line.to_string(),
    };
    match parts.get(skip) {
        Some(&"this") | Some(&"unknown") | None => line.to_string(),
        Some(_) => {
            let mut parts = parts;
            parts[skip] = "machine";
            format!("{head}{}", parts.join(" "))
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(word) {
        let (before, after) = (rest[..i].chars().next_back(), rest[i + word.len()..].chars().next());
        out += &rest[..i];
        out += if before.is_some_and(is_word) || after.is_some_and(is_word) { word } else { with };
        rest = &rest[i + word.len()..];
    }
    out + rest
}

/// Each maximal run of `part` characters, replaced where `swap` gives a replacement.
fn runs(text: &str, part: impl Fn(char) -> bool, swap: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        match (part(c) && i < text.len(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let run = &text[s..i];
                out += &swap(run).unwrap_or_else(|| run.to_string());
                start = None;
                if i < text.len() {
                    out.push(c);
                }
            }
            (false, None) if i < text.len() => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_keeps_the_period_in_order_under_a_section_per_log() {
        crate::session::tests::with_temp_state_dir("report", || {
            let root = crate::paths::state_root();
            std::fs::create_dir_all(root.join("instances/2")).unwrap();
            std::fs::create_dir_all(root.join("instances/10")).unwrap();
            // Old and new lines in one file, as a handover leaves them.
            let new = |secs: i64, text: &str| format!("{} WARN  server[7] {text}\n", crate::log::stamp(secs, 250));
            std::fs::write(root.join("daemon.log"), format!("50 main old\n{}150 main new\n{}", new(99, "main new too old"), new(160, "main newer"))).unwrap();
            std::fs::write(root.join("instances/2/daemon.log.old"), "120 two rolled\n90 two too old\n").unwrap();
            std::fs::write(root.join("instances/2/daemon.log"), format!("{}not a line\n", new(130, "two live"))).unwrap();
            std::fs::write(root.join("instances/10/daemon.log"), "10 ten old\n").unwrap();
            let crash = root.join("crash");
            std::fs::create_dir_all(&crash).unwrap();
            std::fs::write(crash.join("other-1.ips"), "x").unwrap();
            std::fs::write(crash.join("nerd-2026-09-26-173000.ips"), "not json").unwrap();
            let (r, crashes) = build(&root, &crash, &root.join("none"), 100, 200);
            let at = |s: &str| r.find(s).unwrap_or_else(|| panic!("{s} missing in {r}"));
            assert!(at("== main") < at(" main new") && at(" main new") < at("== server 2"));
            assert!(at(" two rolled") < at(" two live"));
            for gone in ["main old", "too old", "not a line", "== server 10", "other-1"] {
                assert!(!r.contains(gone), "{gone} in {r}");
            }
            assert!(at(" main new") < at(" main newer"));
            assert!(r.contains(&format!("{} INFO  ? main new\n", crate::log::stamp(150, 0))), "an old line in the new format");
            assert!(r.contains(&new(160, "main newer")), "a new line as written");
            assert!(crashes.contains("nerd-2026-09-26-173000.ips: unreadable"));
        });
    }

    #[test]
    fn a_crash_is_summed_up_from_its_faulting_thread() {
        let ips = r#"{"app_name":"nerd","bug_type":"309"}
{"captureTime":"2026-09-26 17:30:00.0000 +0200","procPath":"/Users/USER/.local/bin/nerd","exception":{"type":"EXC_CRASH","signal":"SIGABRT"},
"asi":{"libsystem_c.dylib":["abort() called"]},"faultingThread":1,"usedImages":[{"name":"nerd"}],
"threads":[{"frames":[{"symbol":"idle"}]},{"frames":[{"symbol":"__pthread_kill"},{"imageIndex":0,"imageOffset":4096},{"symbol":"_RNvNtCshwJff5UJh9i_4nerd6daemon3run"}]}]}"#;
        let s = crash_summary(ips).unwrap();
        assert_eq!(
            s,
            "time: 2026-09-26 17:30:00.0000 +0200\npath: /Users/USER/.local/bin/nerd\nexception: EXC_CRASH SIGABRT\nasi: abort() called\n  __pthread_kill\n  nerd + 0x1000\n  _RNvNtCshwJff5UJh9i_4nerd6daemon3run\n"
        );
        assert!(redact(&s, "/Users/ann", &[], false).contains("  _RNvNtCshwJff5UJh9i_4nerd6daemon3run\n"), "a Rust symbol survives");
    }

    #[test]
    fn a_crash_names_its_parent_and_a_test_binary_is_marked() {
        let ips = |p: &str| format!("{{}}\n{{\"captureTime\":\"t\",\"procPath\":\"/x/nerd\",\"parentProc\":\"{p}\",\"parentPid\":42,\"exception\":{{}},\"threads\":[]}}");
        assert!(crash_summary(&ips("daemon-cc1b0ab99383cc3d")).unwrap().contains("parent: daemon-cc1b0ab99383cc3d pid 42 (a test binary)\n"));
        assert!(crash_summary(&ips("zsh")).unwrap().contains("parent: zsh pid 42\n"));
    }

    #[test]
    fn a_terminal_crash_is_picked_by_the_name_in_an_attach_line() {
        crate::session::tests::with_temp_state_dir("report-terminal", || {
            let root = crate::paths::state_root();
            std::fs::write(root.join("daemon.log"), format!("{} INFO  server[7] client 1 attached: x, client pid 9 under Ghostty pid 5\n", crate::log::stamp(150, 0))).unwrap();
            let crash = root.join("crash");
            std::fs::create_dir_all(&crash).unwrap();
            let ips = "{}\n{\"captureTime\":\"t\",\"procPath\":\"/Applications/Ghostty.app\",\"exception\":{},\"threads\":[]}";
            std::fs::write(crash.join("ghostty-2026-10-01.ips"), ips).unwrap();
            std::fs::write(crash.join("kitty-2026-10-01.ips"), ips).unwrap();
            let (_, crashes) = build(&root, &crash, &root.join("none"), 100, 200);
            assert!(crashes.contains("== crash ghostty-2026-10-01.ips") && !crashes.contains("kitty"), "{crashes}");
        });
    }

    #[test]
    fn a_jetsam_report_names_what_was_killed() {
        let ips = |procs: &str| format!("{{}}\n{{\"date\":\"2026-10-02 10:00:00.00 +0200\",\"largestProcess\":\"WebContent\",\"processes\":{procs}}}");
        let s = jetsam_summary(&ips(r#"[{"name":"a"},{"name":"nerd","reason":"per-process-limit"}]"#)).unwrap();
        assert_eq!(s, "date: 2026-10-02 10:00:00.00 +0200, largest process: WebContent, killed: nerd (per-process-limit)\n");
        assert!(jetsam_summary(&ips(r#"[{"name":"a"}]"#)).unwrap().ends_with("killed: none\n"));
    }

    #[test]
    fn redaction_takes_what_names_a_person_or_a_secret_and_keeps_paths() {
        let words = [("Mac-mini.local".to_string(), "host"), ("Mac-mini".to_string(), "host"), ("studio".to_string(), "remote")];
        let r = |s: &str| redact(s, "/Users/ann", &words, true);
        assert_eq!(r("client 1 attached: from /Users/ann/dev/projects/nerd/src"), "client 1 attached: from ~/dev/projects/nerd/src");
        assert_eq!(r("host Mac-mini.local and Mac-mini, not Mac-minis"), "host host and host, not Mac-minis");
        assert_eq!(r("connect: studio lists 2, studios stay"), "connect: remote lists 2, studios stay");
        assert_eq!(r("ssh to 192.168.1.20. version 0.6.0"), "ssh to [redacted]. version 0.6.0");
        for secret in ["ghp_0123456789abcdefABCDEF0123456789", "sk-ant-api03-abcdefghijklmnop1234", "1AF025DE-ABA4-4064-906C-DDFB87E763FD", "6831925f0e1d2c3b4a5968778695a4b3c2d1e0f9"] { // privacy: test
            assert_eq!(r(&format!("key {secret}.")), "key [redacted].", "{secret}");
        }
        assert_eq!(r("/Users/ann/x /Users/annabel/y /Users/ann"), "~/x /Users/user/y ~", "a longer name is someone else's folder");
        assert_eq!(r("from /Users/bob.smith/dev and /home/eve/src, /Users/Shared/z"), "from /Users/user/dev and /home/user/src, /Users/Shared/z");
        assert_eq!(r("1 ended: 17:30 Bobs-iMac over ssh detached at 17:40\n"), "1 ended: 17:30 machine over ssh detached at 17:40\n");
        assert_eq!(r("ended: Sep 25 09:01 studio-2 directly lost at 10:00"), "ended: Sep 25 09:01 machine directly lost at 10:00");
        assert_eq!(r("ended: 17:30 this Mac directly closed at 17:40"), "ended: 17:30 this Mac directly closed at 17:40");
        assert_eq!(r("ended: a connection over ssh that had not attached"), "ended: a connection over ssh that had not attached");
        assert_eq!(r("INFO  server[7] link opened in the browser: https://intranet.example.com:8443\n"), "INFO  server[7] link opened in the browser: https://[host]\n");
        assert_eq!(r("nerd 0.6.0 (6831925, 2026-09-16 15:03) 1790438984 sandbox"), "nerd 0.6.0 (6831925, 2026-09-16 15:03) 1790438984 sandbox");
    }
}
