//! The plugin catalog: a git repository of entries, fetched only when the user refreshes it. Marks, catalog
//! installs and revocations are read from its copy under the state root.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::install::Pin;

/// The one catalog; there is no setting to point nerd at another.
pub const URL: &str = "https://github.com/nerdterm/nerd-plugins.git";
const DIR: &str = "catalog";

/// `plugins/<name>.json` in the catalog: the reviewed release of one plugin.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    #[serde(skip)]
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub url: String,
    pub tag: String,
    pub commit: String,
    pub version: String,
    pub risk: u8,
    /// `reviewed` by the maintainer, or `checked` by the catalog's CI alone (risk level 0).
    pub review: String,
}

/// `revoked.json`: a release that must not run (`hard`) or that the user is warned about (`soft`).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Revoked {
    pub name: String,
    pub commit: String,
    pub level: String,
    pub why: String,
}

/// `stars.json`: each plugin's GitHub stars, counted daily by the catalog's CI; `changed` is when a count last changed.
#[derive(Deserialize, Default)]
pub(crate) struct Stars {
    changed: String,
    stars: std::collections::BTreeMap<String, u32>,
}

#[derive(Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    pub revoked: Vec<Revoked>,
    pub(crate) stars: Stars,
    /// When it was last fetched; `None`: never.
    pub fetched: Option<std::time::SystemTime>,
}

pub fn dir() -> PathBuf {
    crate::paths::state_root().join(DIR)
}

/// The copy on disk; none is an empty catalog. An entry that breaks a rule is left out, and said why.
pub fn read() -> (Catalog, Vec<String>) {
    read_at(&dir())
}

fn token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn line(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(char::is_control)
}

pub fn read_at(dir: &Path) -> (Catalog, Vec<String>) {
    let mut c = Catalog { fetched: std::fs::metadata(dir).and_then(|m| m.modified()).ok(), ..Default::default() };
    let mut problems = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("plugins")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for f in files {
        let name = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let parsed = std::fs::read(&f).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Entry>(&b).map_err(|e| e.to_string()));
        let checked = parsed.and_then(|mut e| {
            e.name = name.clone();
            let ok = token(&name)
                && crate::install::source(&e.url, false).is_ok()
                && e.commit.len() == 40
                && e.commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && crate::agents::semver(e.tag.strip_prefix('v').unwrap_or(&e.tag)).is_some()
                && e.version == e.tag.strip_prefix('v').unwrap_or(&e.tag)
                && e.risk <= 3
                && (e.review == "reviewed" || (e.review == "checked" && e.risk == 0))
                && line(&e.title, 64)
                && line(&e.summary, 200);
            if ok { Ok(e) } else { Err("an entry breaks the catalog's rules".into()) }
        });
        match checked {
            Ok(e) => c.entries.push(e),
            Err(why) => problems.push(format!("catalog entry {}: {why}", crate::render::unspoofed(&name))),
        }
    }
    if let Ok(b) = std::fs::read(dir.join("revoked.json")) {
        match serde_json::from_slice::<Vec<Revoked>>(&b) {
            Ok(rs) => {
                for r in rs {
                    if token(&r.name) && r.commit.len() == 40 && (r.level == "hard" || r.level == "soft") && line(&r.why, 200) {
                        c.revoked.push(r);
                    } else {
                        problems.push(format!("a revocation of {} breaks the catalog's rules", crate::render::unspoofed(&r.name)));
                    }
                }
            }
            Err(e) => problems.push(format!("revoked.json does not parse: {e}")),
        }
    }
    if let Ok(b) = std::fs::read(dir.join("stars.json")) {
        match serde_json::from_slice::<Stars>(&b) {
            Ok(st) if st.changed.len() == 10 && st.changed.bytes().all(|b| b.is_ascii_digit() || b == b'-') && st.stars.keys().all(|k| token(k)) => c.stars = st,
            _ => problems.push("stars.json breaks the catalog's rules".into()),
        }
    }
    (c, problems)
}

impl Catalog {
    /// GitHub stars of the catalog's plugin `name` from `url`, and the date a count last changed.
    pub fn stars(&self, name: &str, url: &str) -> Option<(u32, &str)> {
        self.entries.iter().find(|e| e.name == name && e.url == url)?;
        self.stars.stars.get(name).map(|n| (*n, self.stars.changed.as_str()))
    }

    pub fn by_url(&self, url: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.url == url)
    }

    /// `reviewed`, `checked`, `not in catalog`, or `by hand` for a plugin with no lock line.
    pub fn mark(&self, pin: Option<&Pin>) -> &str {
        let Some(p) = pin else { return "by hand" };
        match self.entries.iter().find(|e| e.name == p.name && e.url == p.source && e.commit == p.commit) {
            Some(e) => &e.review,
            None => "not in catalog",
        }
    }

    pub fn revoked(&self, name: &str, commit: &str) -> Option<&Revoked> {
        self.revoked.iter().find(|r| r.name == name && r.commit == commit)
    }
}

/// `nerd --plugin-catalog`: fetch the catalog, then act on what it revokes.
pub fn refresh() -> Result<Vec<String>, String> {
    refresh_from(URL, false)
}

/// `local` lets a test fetch a catalog from a folder; the product always fetches `URL`.
pub(crate) fn refresh_from(url: &str, local: bool) -> Result<Vec<String>, String> {
    crate::install::fetch_tree(url, local, &dir())?;
    let (c, mut lines) = read();
    lines.insert(0, format!("Catalog fetched: {} {}.", c.entries.len(), if c.entries.len() == 1 { "plugin" } else { "plugins" }));
    lines.extend(revoke(&c)?);
    Ok(lines)
}

/// A hard revocation turns the installed plugin off until it is updated to the catalog's current release, or
/// removed; a soft one only says so.
pub fn revoke(c: &Catalog) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for p in crate::install::read_lock()? {
        let Some(r) = c.revoked(&p.name, &p.commit) else { continue };
        let why = crate::render::unspoofed(&r.why);
        if r.level == "hard" {
            match crate::install::turn(&p.name, false, &format!("revoked: {why}")) {
                Ok(()) => {
                    crate::log::warn(&format!("plugin {} turned off: the catalog revoked {}: {why}", p.name, p.commit));
                    lines.push(format!("{} is turned off: the catalog revoked this release ({why}).", p.name));
                }
                Err(e) => {
                    crate::log::error(&format!("plugin {}: the catalog revoked {}, and turning it off failed: {e}", p.name, p.commit));
                    lines.push(format!("{}: the catalog revoked this release ({why}), and turning it off failed: {e}", p.name));
                }
            }
        } else {
            lines.push(format!("{}: the catalog warns about this release ({why}).", p.name));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    #[test]
    fn a_catalog_entry_that_breaks_a_rule_is_left_out_and_marks_follow_the_exact_commit() {
        let dir = std::env::temp_dir().join(format!("nerd-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        let entry = |url: &str, commit: String, review: &str| serde_json::json!({ "title": "T", "url": url, "tag": "v1.0.0", "commit": commit, "version": "1.0.0", "risk": 0, "review": review }).to_string();
        std::fs::write(dir.join("plugins/older.json"), entry("https://github.com/me/o.git", sha('e'), "checked").replace("\"version\":\"1.0.0\"", "\"version\":\"0.9.0\"")).unwrap();
        std::fs::write(dir.join("plugins/risky.json"), entry("https://github.com/me/r.git", sha('f'), "checked").replace("\"risk\":0", "\"risk\":2")).unwrap();
        std::fs::write(dir.join("plugins/good.json"), entry("https://github.com/me/good.git", sha('a'), "checked")).unwrap();
        std::fs::write(dir.join("plugins/local.json"), entry("/tmp/x", sha('b'), "checked")).unwrap();
        std::fs::write(dir.join("plugins/short.json"), entry("https://github.com/me/s.git", "abc".into(), "checked")).unwrap();
        std::fs::write(dir.join("plugins/who.json"), entry("https://github.com/me/w.git", sha('c'), "trusted")).unwrap();
        std::fs::write(dir.join("revoked.json"), serde_json::json!([{ "name": "good", "commit": sha('a'), "level": "hard", "why": "steals" }, { "name": "x", "commit": "1", "level": "hard", "why": "" }]).to_string()).unwrap();
        let (c, problems) = read_at(&dir);
        assert_eq!(c.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["good"]);
        assert_eq!(problems.len(), 6, "a version not its tag's, and risk 2 checked without the maintainer, are refused too: {problems:?}");
        let pin = |url: &str, commit: String| Pin { name: "good".into(), source: url.into(), tag: None, commit };
        assert_eq!(c.mark(Some(&pin("https://github.com/me/good.git", sha('a')))), "checked");
        assert_eq!(c.mark(Some(&pin("https://github.com/me/good.git", sha('d')))), "not in catalog", "another commit is not the reviewed one");
        assert_eq!(c.mark(Some(&pin("https://github.com/evil/good.git", sha('a')))), "not in catalog");
        assert_eq!(c.mark(None), "by hand");
        assert_eq!(c.revoked("good", &sha('a')).map(|r| r.level.as_str()), Some("hard"));
        assert_eq!(c.stars("good", "https://github.com/me/good.git"), None, "no stars.json, no stars");
        std::fs::write(dir.join("stars.json"), r#"{"changed":"2026-10-01","stars":{"good":42,"gone":7}}"#).unwrap();
        let (c, problems) = read_at(&dir);
        assert_eq!(problems.len(), 6, "{problems:?}");
        assert_eq!(c.stars("good", "https://github.com/me/good.git"), Some((42, "2026-10-01")));
        assert_eq!(c.stars("good", "https://github.com/evil/good.git"), None, "stars belong to the catalog's own release");
        assert_eq!(c.stars("gone", "https://github.com/me/gone.git"), None, "only for an entry the catalog keeps");
        std::fs::write(dir.join("stars.json"), r#"{"changed":"soon","stars":{"Good/../x":1}}"#).unwrap();
        let (c, problems) = read_at(&dir);
        assert!(problems.iter().any(|p| p.contains("stars.json")) && c.stars("good", "https://github.com/me/good.git").is_none(), "{problems:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A plugin repository with releases `v1.0.0` and `v1.1.0`, installed at the first; the commits, in order.
    fn installed_plugin(root: &Path) -> (String, String) {
        let repo = root.join("src/plug");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let mut commits = Vec::new();
        for v in ["1.0.0", "1.1.0"] {
            std::fs::write(repo.join("plugin.json"), serde_json::json!({ "name": "plug", "title": "P", "version": v, "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] }).to_string()).unwrap();
            std::fs::write(repo.join("p.sh"), "exit 0\n").unwrap();
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-qm", v]);
            git(&repo, &["tag", &format!("v{v}")]);
            commits.push(git(&repo, &["rev-parse", "HEAD"]));
        }
        crate::install::place(crate::install::stage_add(&repo.to_string_lossy(), Some("v1.0.0"), true).unwrap()).unwrap();
        (commits[0].clone(), commits[1].clone())
    }

    #[test]
    fn a_refresh_replaces_the_copy_and_a_hard_revocation_keeps_a_release_off_until_an_update() {
        crate::session::tests::with_temp_state_dir("catalog-revoke", || {
            let root = crate::paths::state_root();
            let (first, _) = installed_plugin(&root);
            let cat = root.join("src/catalog");
            std::fs::create_dir_all(cat.join("plugins")).unwrap();
            git(&cat, &["init", "-q", "-b", "main"]);
            let revoked = |level: &str| serde_json::json!([{ "name": "plug", "commit": first, "level": level, "why": "it reads keys" }]).to_string();
            std::fs::write(cat.join("revoked.json"), revoked("soft")).unwrap();
            git(&cat, &["add", "-A"]);
            git(&cat, &["commit", "-qm", "soft"]);

            let lines = refresh_from(&cat.to_string_lossy(), true).unwrap();
            assert_eq!(lines, ["Catalog fetched: 0 plugins.", "plug: the catalog warns about this release (it reads keys)."]);
            assert!(dir().join("revoked.json").is_file() && !dir().join(".git").exists(), "the files only");
            assert!(crate::install::read_off().unwrap().is_empty(), "a soft revocation turns nothing off");

            std::fs::write(cat.join("revoked.json"), revoked("hard")).unwrap();
            git(&cat, &["commit", "-qam", "hard"]);
            let lines = refresh_from(&cat.to_string_lossy(), true).unwrap();
            assert_eq!(lines[1], "plug is turned off: the catalog revoked this release (it reads keys).");
            assert_eq!(crate::install::read_off().unwrap().get("plug").map(String::as_str), Some("revoked: it reads keys"));
            assert!(crate::install::turn("plug", true, "").unwrap_err().contains("revoked"), "it cannot be turned back on");
            crate::install::turn("plug", false, "turned off").unwrap();
            assert!(crate::install::read_off().unwrap()["plug"].starts_with("revoked"), "off does not turn a revocation into an ordinary off");
            assert!(crate::install::turn("plug", true, "").is_err());
            let (s, _, _) = crate::install::stage_update("plug").unwrap().expect("v1.1.0 is newer");
            crate::install::apply_update(s).unwrap();
            assert!(crate::install::read_off().unwrap()["plug"].starts_with("revoked"), "an upstream release the catalog never saw lifts nothing");
        });
    }

    #[test]
    fn an_update_whose_lock_cannot_be_written_puts_the_old_files_back() {
        crate::session::tests::with_temp_state_dir("update-lock-fails", || {
            let root = crate::paths::state_root();
            let (first, _) = installed_plugin(&root);
            let (s, _, _) = crate::install::stage_update("plug").unwrap().expect("v1.1.0 is newer");
            // The lock's temp file cannot be made where a folder stands.
            std::fs::create_dir_all(root.join("plugins.lock-tmp")).unwrap();
            assert!(crate::install::apply_update(s).is_err());
            assert!(std::fs::read_to_string(root.join("plugins/plug/plugin.json")).unwrap().contains("1.0.0"), "the old tree is back");
            assert_eq!(crate::install::read_lock().unwrap()[0].commit, first);
        });
    }

    #[test]
    fn a_revoked_release_is_never_installed() {
        crate::session::tests::with_temp_state_dir("catalog-refuse", || {
            let root = crate::paths::state_root();
            let (first, _) = installed_plugin(&root);
            crate::install::remove("plug", false).unwrap();
            std::fs::create_dir_all(dir()).unwrap();
            std::fs::write(dir().join("revoked.json"), serde_json::json!([{ "name": "plug", "commit": first, "level": "hard", "why": "bad" }]).to_string()).unwrap();
            let e = crate::install::stage_add(&root.join("src/plug").to_string_lossy(), Some("v1.0.0"), true).map(|_| ()).unwrap_err();
            assert!(e.contains("the catalog revoked this release: bad"), "{e}");
        });
    }
}
