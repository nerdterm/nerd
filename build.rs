//! Stamps the binary with the commit and its date: the version number alone
//! cannot tell two builds of one day apart, and the update dialog has to.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let date = git(&["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d %H:%M"]).unwrap_or_default();
    println!("cargo:rustc-env=NERD_BUILD={hash}{}{}", if dirty { "+" } else { "" }, if date.is_empty() { String::new() } else { format!(", {date}") });
    // What this build brings, for the update window: the commits since the last release, newest first.
    let since = git(&["describe", "--tags", "--abbrev=0"]).map_or("HEAD".to_string(), |t| format!("{t}..HEAD"));
    let commits = git(&["log", &since, "--format=%h %s", "-n", "300"]).unwrap_or_default();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("commits.txt");
    let _ = std::fs::write(out, commits);
    println!("cargo:rerun-if-changed=CHANGELOG.md");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=.git/index");
}
