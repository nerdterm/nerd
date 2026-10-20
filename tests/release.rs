//! `nerd --check-update` by the built binary against a stand-in for curl that writes canned files and reaches nothing.
//! Only a debug build takes the stand-in, so under `--release` this file holds no test rather than reach GitHub.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TAG: &str = "v0.99.0";
const ARCHIVE: &str = "nerd-v0.99.0-macos-arm64.tar.gz";

struct Env {
    home: PathBuf,
    /// What the stand-in serves, by the last part of the url, and the urls it was asked for.
    served: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let root = std::env::temp_dir().join(format!("nerd-it-release-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Library/Application Support/nerd")).unwrap();
        let served = root.join("served");
        std::fs::create_dir_all(&served).unwrap();
        let curl = root.join("curl");
        std::fs::write(
            &curl,
            "#!/bin/sh\nout= url=\nwhile [ $# -gt 0 ]; do case \"$1\" in --output) out=$2; shift ;; --) url=$2; break ;; esac; shift; done\n\
             echo \"$url\" >> \"$SERVED/asked\"\n\
             if [ -n \"$OFFLINE\" ]; then echo 'curl: (6) Could not resolve host: api.github.com' >&2; printf 000; exit 6; fi\n\
             if [ -n \"$CODE\" ]; then printf %s \"$CODE\"; exit 0; fi\n\
             cp \"$SERVED/${url##*/}\" \"$out\" && printf 200\n",
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let body = "## [0.99.0](https://github.com/nerdterm/nerd/compare/v0.13.0...v0.99.0)\n\n### Added\n\n* a thing ([#1](https://x/1))\n";
        let latest = serde_json::json!({ "tag_name": TAG, "body": body, "assets": [{ "name": ARCHIVE }, { "name": format!("{ARCHIVE}.sha256") }] });
        std::fs::write(served.join("latest"), latest.to_string()).unwrap();
        let e = Env { home: home.canonicalize().unwrap(), served };
        e.archive("#!/bin/sh\necho 'nerd 0.99.0 (test)'\n");
        e
    }

    fn state(&self) -> PathBuf {
        self.home.join("Library/Application Support/nerd")
    }

    /// The release's archive holding `script` as its nerd, with its true checksum.
    fn archive(&self, script: &str) {
        let dir = self.served.join("build");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nerd"), script).unwrap();
        std::fs::set_permissions(dir.join("nerd"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(Command::new("tar").args(["-czf"]).arg(self.served.join(ARCHIVE)).arg("-C").arg(&dir).arg("nerd").status().unwrap().success());
        let sum = Command::new("shasum").args(["-a", "256"]).arg(ARCHIVE).current_dir(&self.served).output().unwrap();
        std::fs::write(self.served.join(format!("{ARCHIVE}.sha256")), sum.stdout).unwrap();
    }

    fn run(&self, exe: &Path, args: &[&str], vars: &[(&str, &str)]) -> String {
        let out = Command::new(exe)
            .args(args)
            .env("HOME", &self.home)
            .env("NERD_TEST_CURL", self.served.parent().unwrap().join("curl"))
            .env("SERVED", &self.served)
            .env_remove("NERD_STATE_DIR")
            .env_remove("NERD_SOCKET")
            .env_remove("NERD_INSTANCE")
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    /// What the window's run left for the server.
    fn said(&self) -> serde_json::Value {
        self.said_in(&self.state())
    }

    fn said_in(&self, dir: &Path) -> serde_json::Value {
        let p = dir.join("release.json");
        let v = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        std::fs::remove_file(p).unwrap();
        v
    }

    fn asked(&self) -> Vec<String> {
        std::fs::read_to_string(self.served.join("asked")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// A copy of the built nerd where install.sh puts it, so it may replace itself.
    fn installed(&self) -> PathBuf {
        let bin = self.home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_nerd"), bin.join("nerd")).unwrap();
        bin.join("nerd")
    }

    fn log(&self) -> String {
        let mut all = String::new();
        let mut dirs = vec![self.state()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.file_name().is_some_and(|n| n == "daemon.log") {
                    all += &std::fs::read_to_string(p).unwrap();
                }
            }
        }
        all
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

#[test]
fn a_check_names_the_newest_release_its_changelog_and_why_this_nerd_cannot_install_it() {
    let e = Env::new("check");
    e.run(Path::new(env!("CARGO_BIN_EXE_nerd")), &["--check-update", "--window"], &[]);
    let f = &e.said()["Found"];
    assert_eq!(f["tag"], TAG);
    assert_eq!(f["newer"], true);
    assert_eq!(f["notes"], serde_json::json!(["0.99.0", "", "Added", "", "- a thing (#1)"]));
    assert!(f["blocked"].as_str().is_some_and(|b| b.contains("only a nerd in ~/.local/bin")), "a build in target/ never replaces itself: {f}");
    assert_eq!(e.asked(), ["https://api.github.com/repos/nerdterm/nerd/releases/latest"], "one question, nothing downloaded");
    assert!(e.log().contains("release check: newest v0.99.0, running"), "{}", e.log());
}

#[test]
fn no_network_and_a_refusal_by_github_are_said_apart() {
    let e = Env::new("offline");
    let nerd = Path::new(env!("CARGO_BIN_EXE_nerd"));
    e.run(nerd, &["--check-update", "--window"], &[("OFFLINE", "1")]);
    assert_eq!(e.said()["Failed"], "could not reach GitHub: (6) Could not resolve host: api.github.com");
    e.run(nerd, &["--check-update", "--window"], &[("CODE", "403")]);
    assert!(e.said()["Failed"].as_str().unwrap().contains("rate limit"));
    let out = e.run(nerd, &["--check-update"], &[("OFFLINE", "1")]);
    assert!(out.contains("could not reach GitHub"), "{out}");
    assert!(e.log().contains("WARN") && e.log().contains("release check failed: could not reach GitHub"), "{}", e.log());
}

#[test]
fn a_release_is_installed_only_for_the_window_s_click_and_only_when_its_checksum_matches() {
    let e = Env::new("install");
    let nerd = e.installed();
    let before = std::fs::read(&nerd).unwrap();
    let go = e.state().join("release-install");

    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert!(e.said()["Failed"].as_str().unwrap().contains("only from a button in nerd's window"));
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");

    std::fs::write(&go, TAG).unwrap();
    std::fs::write(e.served.join(format!("{ARCHIVE}.sha256")), format!("{}  {ARCHIVE}\n", "0".repeat(64))).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Failed"], "checksum mismatch, nothing installed");
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");
    assert!(!go.exists(), "a click is good for one run");

    e.archive("#!/bin/sh\necho 'nerd 0.99.0 (test)'\n");
    std::fs::write(&go, "v0.98.0").unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert!(e.said()["Failed"].as_str().unwrap().contains("the newest release is now v0.99.0"), "only what the window showed");

    std::fs::write(&go, TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Installed"], "nerd 0.99.0 (test)");
    let out = Command::new(&nerd).arg("--version").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nerd 0.99.0 (test)\n");
    assert!(e.asked().iter().any(|u| u == &format!("https://github.com/nerdterm/nerd/releases/download/{TAG}/{ARCHIVE}.sha256")), "{:?}", e.asked());
    let left: Vec<_> = std::fs::read_dir(nerd.parent().unwrap()).unwrap().flatten().map(|d| d.file_name()).collect();
    assert_eq!(left, ["nerd"], "renamed into place, nothing left beside it");
    assert!(!e.home.join(".zshrc").exists(), "the shell's file is left alone");
    assert!(e.log().contains("release v0.99.0 installed in ~/.local/bin: nerd 0.99.0 (test)"), "{}", e.log());
}

#[test]
fn each_server_on_a_mac_takes_only_its_own_click_and_its_own_answer() {
    let e = Env::new("instances");
    let nerd = e.installed();
    let (a, b) = (e.state().join("instances/a"), e.state().join("instances/b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("release-install"), TAG).unwrap();

    e.run(&nerd, &["--check-update", "--install", "--window"], &[("NERD_INSTANCE", "b")]);
    assert!(e.said_in(&b)["Failed"].as_str().unwrap().contains("only from a button in nerd's window"), "server b has no click of its own");
    assert!(a.join("release-install").exists(), "nor did it take server a's");
    e.run(&nerd, &["--check-update", "--install", "--window"], &[("NERD_INSTANCE", "a")]);
    assert_eq!(e.said_in(&a)["Installed"], "nerd 0.99.0 (test)");
    assert!(!e.state().join("release.json").exists() && !b.join("release.json").exists(), "each answer only where its server reads");
}

#[test]
fn from_a_terminal_with_no_one_to_answer_nothing_is_installed() {
    let e = Env::new("terminal");
    let nerd = e.installed();
    let before = std::fs::read(&nerd).unwrap();
    let out = e.run(&nerd, &["--check-update"], &[]);
    assert!(out.contains("Newest:  nerd 0.99.0") && out.contains("- a thing (#1)"), "{out}");
    assert_eq!(std::fs::read(&nerd).unwrap(), before);
    assert!(!e.asked().iter().any(|u| u.ends_with(".tar.gz")), "nothing downloaded");
    assert!(e.run(&nerd, &["--check-update", "--install"], &[]).contains("usage: nerd --check-update"), "--install is the window's");
}
