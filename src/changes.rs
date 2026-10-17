//! What this build brings over an older one: the changelog's sections for newer versions, then the
//! commits since the last release, as build.rs recorded them.

const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const COMMITS: &str = include_str!(concat!(env!("OUT_DIR"), "/commits.txt"));
const MOST: usize = 16;

/// `nerd 0.7.0` for a release, `nerd 0.6.0 (d987588)` for a build between releases.
pub fn short_version() -> String {
    let (v, hash) = parse(&crate::version_line());
    if COMMITS.trim().is_empty() && !env!("NERD_BUILD").contains('+') { format!("nerd {v}") } else { format!("nerd {v} ({hash})") }
}

/// Rows for a dialog: the kind on its first change's row, then one change per row.
pub fn since(running: &str) -> Vec<Vec<String>> {
    since_in(CHANGELOG, COMMITS, env!("CARGO_PKG_VERSION"), running)
}

/// `nerd 0.6.0 (d987588+, 2026-09-30 23:55)` → (`0.6.0`, `d987588`).
fn parse(line: &str) -> (String, String) {
    let v = line.split_whitespace().nth(1).unwrap_or_default().to_string();
    let hash = line.split('(').nth(1).and_then(|s| s.split([',', ')']).next()).unwrap_or_default().trim_end_matches('+').to_string();
    (v, hash)
}

fn number(v: &str) -> (u64, u64, u64) {
    let mut n = v.split(['.', '-']).map(|p| p.parse().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

fn since_in(changelog: &str, commits: &str, this: &str, running: &str) -> Vec<Vec<String>> {
    let (old, hash) = parse(running);
    let mut kinds: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |kind: &str, what: String| match kinds.iter_mut().find(|(k, _)| k == kind) {
        Some((_, list)) => list.push(what),
        None => kinds.push((kind.to_string(), vec![what])),
    };
    // Newest first; a build of this same version stops at the one running.
    for line in commits.lines() {
        let (h, subject) = line.split_once(' ').unwrap_or((line, ""));
        if number(this) == number(&old) && !hash.is_empty() && (h.starts_with(&hash) || hash.starts_with(h)) {
            break;
        }
        let (kind, text) = subject.split_once(": ").unwrap_or(("", subject));
        let kind = match kind.split('(').next().unwrap_or_default().trim_end_matches('!') {
            "feat" => "Added",
            "fix" => "Fixed",
            "perf" => "Faster",
            _ => continue,
        };
        // Squash merges end with the pull request's number.
        let text = text.rsplit_once(" (#").filter(|(_, n)| n.ends_with(')')).map_or(text, |(t, _)| t);
        add(kind, text.to_string());
    }
    if number(this) > number(&old) {
        let mut version = String::new();
        let mut kind = String::new();
        for line in changelog.lines() {
            if let Some(rest) = line.strip_prefix("## [") {
                version = rest.split(']').next().unwrap_or_default().to_string();
            } else if let Some(k) = line.strip_prefix("### ") {
                kind = k.trim().to_string();
            } else if let Some(item) = line.strip_prefix("* ")
                && number(&version) > number(&old)
                && number(&version) <= number(this)
            {
                add(&kind, item.split(" ([").next().unwrap_or(item).trim().to_string());
            }
        }
    }
    let total: usize = kinds.iter().map(|(_, l)| l.len()).sum();
    let mut rows = Vec::new();
    for (kind, list) in kinds {
        for (i, what) in list.into_iter().enumerate() {
            if rows.len() == MOST {
                rows.push(vec![String::new(), format!("and {} more", total - MOST)]);
                return rows;
            }
            rows.push(vec![if i == 0 { kind.clone() } else { String::new() }, what]);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "## [0.7.0](x) (2026-10-01)\n\n### Added\n\n* a clock setting ([#1](x)) ([abc](x))\n\n### Fixed\n\n* tabs keep their place ([#2](x))\n\n## [0.6.0](x)\n\n### Added\n\n* old thing ([#0](x))\n";
    const COMMITS: &str = "c3 feat: the newest (#9)\nc2 docs: not shown\nc1 fix(ui): a fix\nb0 feat: already running\n";

    #[test]
    fn a_build_of_the_same_version_lists_its_commits_since_the_running_one() {
        let rows = since_in(LOG, COMMITS, "0.6.0", "nerd 0.6.0 (b0+, 2026-09-30 22:25)");
        assert_eq!(rows, [vec!["Added".to_string(), "the newest".into()], vec!["Fixed".into(), "a fix".into()]]);
    }

    #[test]
    fn a_newer_version_adds_its_changelog_and_skips_older_releases() {
        let rows = since_in(LOG, "", "0.7.0", "nerd 0.6.0 (b0, 2026-09-30 22:25)");
        assert_eq!(rows, [vec!["Added".to_string(), "a clock setting".into()], vec!["Fixed".into(), "tabs keep their place".into()]]);
        assert!(since_in(LOG, "", "0.7.0", "nerd 0.7.0 (c3)").is_empty(), "nothing newer");
    }

    #[test]
    fn a_long_list_ends_with_how_many_more() {
        let many: String = (0..30).map(|i| format!("h{i} fix: change {i}\n")).collect();
        let rows = since_in("", &many, "0.6.0", "nerd 0.6.0 (zzz)");
        assert_eq!(rows.len(), MOST + 1);
        assert_eq!(rows[MOST][1], "and 14 more");
    }
}
