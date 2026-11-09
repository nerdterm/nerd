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
        let latest = serde_json::json!({ "tag_name": TAG, "body": body, "assets": [{ "name": ARCHIVE }, { "name": format!("{ARCHIVE}.sha256") }, { "name": format!("{ARCHIVE}.sha256.sig") }] });
        std::fs::write(served.join("latest"), latest.to_string()).unwrap();
        // Test signing keys: "main" stands in for nerd's release keys, "other" is anyone else's.
        for k in ["main", "other"] {
            let key = root.join(format!("{k}.key"));
            assert!(Command::new("/usr/bin/openssl").args(["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out"]).arg(&key).status().unwrap().success());
            assert!(Command::new("/usr/bin/openssl").args(["ec", "-pubout", "-in"]).arg(&key).arg("-out").arg(root.join(format!("{k}.pub"))).stderr(Stdio::null()).status().unwrap().success());
        }
        let e = Env { home: home.canonicalize().unwrap(), served };
        e.archive("#!/bin/sh\necho 'nerd 0.99.0 (test)'\n");
        e
    }

    fn state(&self) -> PathBuf {
        self.home.join("Library/Application Support/nerd")
    }

    /// The release's archive holding `script` as its nerd, with its true checksum.
    fn archive(&self, script: &str) {
        self.archive_with(script, &[]);
    }

    /// The same with `extra` files beside nerd.
    fn archive_with(&self, script: &str, extra: &[&str]) {
        let dir = self.served.join("build");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nerd"), script).unwrap();
        std::fs::set_permissions(dir.join("nerd"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        extra.iter().for_each(|x| std::fs::write(dir.join(x), "x").unwrap());
        // No AppleDouble files beside nerd: the Homebrew path refuses any member but nerd.
        assert!(Command::new("tar").env("COPYFILE_DISABLE", "1").args(["-czf"]).arg(self.served.join(ARCHIVE)).arg("-C").arg(&dir).arg("nerd").args(extra).status().unwrap().success());
        let sum = Command::new("shasum").args(["-a", "256"]).arg(ARCHIVE).current_dir(&self.served).output().unwrap();
        std::fs::write(self.served.join(format!("{ARCHIVE}.sha256")), sum.stdout).unwrap();
        self.sign("main");
    }

    /// The served checksum signed with the test key `key`.
    fn sign(&self, key: &str) {
        let sum = self.served.join(format!("{ARCHIVE}.sha256"));
        let key = self.served.parent().unwrap().join(format!("{key}.key"));
        assert!(Command::new("/usr/bin/openssl").args(["dgst", "-sha256", "-sign"]).arg(key).arg("-out").arg(self.served.join(format!("{ARCHIVE}.sha256.sig"))).arg(sum).status().unwrap().success());
    }

    fn run(&self, exe: &Path, args: &[&str], vars: &[(&str, &str)]) -> String {
        let out = Command::new(exe)
            .args(args)
            .env("HOME", &self.home)
            .env("NERD_TEST_CURL", self.served.parent().unwrap().join("curl"))
            .env("SERVED", &self.served)
            .env("NERD_TEST_RELEASE_KEYS", std::fs::read_to_string(self.served.parent().unwrap().join("main.pub")).unwrap())
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

    /// A copy of the built nerd as Homebrew installs it from nerdterm/tap, its tap a git repository, and stand-ins for
    /// sandbox-exec and brew that note what they were asked; the link Homebrew puts on PATH.
    fn brewed(&self) -> PathBuf {
        let root = self.home.parent().unwrap();
        let prefix = root.join("prefix");
        let keg = prefix.join("Cellar/nerd/0.21.0");
        let tap = prefix.join("Library/Taps/nerdterm/homebrew-tap");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(tap.join("Formula")).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_nerd"), keg.join("bin/nerd")).unwrap();
        std::fs::write(keg.join("INSTALL_RECEIPT.json"), r#"{"source":{"tap":"nerdterm/tap"}}"#).unwrap();
        std::os::unix::fs::symlink(keg.join("bin/nerd"), prefix.join("bin/nerd")).unwrap();
        let (r, t) = (root.display(), tap.display());
        let script = |p: &Path, text: String| {
            std::fs::write(p, text).unwrap();
            std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        };
        script(&prefix.join("bin/brew"), format!("#!/bin/sh\ncase $1 in\n--version) echo 'Homebrew 7.0.11' ;;\n--cache) echo '{r}/cache' ;;\nupgrade) cp '{t}/Formula/nerd.rb' '{r}/seen.rb'; ls '{r}/cache/downloads' > '{r}/cached' ;;\n*) exit 9 ;;\nesac\n"));
        script(&root.join("sandbox-exec"), format!("#!/bin/sh\n[ \"$1\" = -p ] || exit 97\nprintf '%s' \"$2\" > '{r}/profile'\nshift 2\nexec \"$@\"\n"));
        std::fs::write(tap.join("README.md"), "# nerdterm/tap\n").unwrap();
        std::fs::write(tap.join("Formula/nerd.rb"), "class Nerd < Formula\nend\n").unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false", "commit", "-qm", "tap"]] {
            assert!(Command::new("git").arg("-C").arg(&tap).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").status().unwrap().success());
        }
        prefix.join("bin/nerd")
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
fn a_release_is_installed_only_for_the_window_s_click_and_only_when_nerd_s_key_signed_it() {
    let e = Env::new("install");
    let nerd = e.installed();
    let before = std::fs::read(&nerd).unwrap();
    let go = e.state().join("release-install");

    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert!(e.said()["Failed"].as_str().unwrap().contains("only from a button in nerd's window"));
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");

    std::fs::write(&go, TAG).unwrap();
    e.sign("other");
    e.run(&nerd, &["--check-update", "--install", "--window"], &[("NERD_INSTALL_UNVERIFIED", "1")]);
    assert_eq!(e.said()["Failed"], "the signature does not check against nerd's release keys, nothing installed");
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");
    assert!(e.log().contains("release v0.99.0 not installed: the signature does not check"), "{}", e.log());

    std::fs::write(&go, TAG).unwrap();
    std::fs::write(e.served.join(format!("{ARCHIVE}.sha256")), format!("{}  {ARCHIVE}\n", "0".repeat(64))).unwrap();
    e.sign("main");
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Failed"], "checksum mismatch, nothing installed");
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");
    assert!(!go.exists(), "a click is good for one run");

    e.archive("#!/bin/sh\necho 'nerd 0.98.0 (test)'\n");
    std::fs::write(&go, TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Failed"], "the archive holds nerd 0.98.0, not 0.99.0, nothing installed");
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");

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
fn an_unsigned_release_or_one_not_newer_is_never_installed() {
    let e = Env::new("unsigned");
    let nerd = e.installed();
    let before = std::fs::read(&nerd).unwrap();
    let go = e.state().join("release-install");
    let latest = |tag: &str, sig: bool| {
        let archive = format!("nerd-{tag}-macos-arm64.tar.gz");
        let mut assets = vec![serde_json::json!({ "name": archive }), serde_json::json!({ "name": format!("{archive}.sha256") })];
        if sig {
            assets.push(serde_json::json!({ "name": format!("{archive}.sha256.sig") }));
        }
        std::fs::write(e.served.join("latest"), serde_json::json!({ "tag_name": tag, "assets": assets }).to_string()).unwrap();
    };

    latest(TAG, false);
    std::fs::write(&go, TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Failed"], format!("release {TAG} has no {ARCHIVE}.sha256.sig yet"));

    let running = format!("v{}", env!("CARGO_PKG_VERSION"));
    latest(&running, true);
    std::fs::write(&go, &running).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &[]);
    assert_eq!(e.said()["Found"]["newer"], false, "the running version again is only shown");
    assert_eq!(std::fs::read(&nerd).unwrap(), before, "nothing installed");
    assert!(!e.asked().iter().any(|u| u.ends_with(".tar.gz")), "nothing downloaded");
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

#[test]
fn a_homebrew_nerd_from_nerd_s_tap_is_upgraded_by_its_own_brew_with_nerd_s_formula() {
    let e = Env::new("brew");
    let nerd = e.brewed();
    let root = e.home.parent().unwrap().to_path_buf();
    let stand_in = root.join("sandbox-exec");
    let vars = [("NERD_TEST_BREW", stand_in.to_str().unwrap())];
    let tap = root.join("prefix/Library/Taps/nerdterm/homebrew-tap");

    e.run(&nerd, &["--check-update", "--window"], &vars);
    let f = &e.said()["Found"];
    assert!(f["brew"] == true && f["blocked"].is_null(), "{f}");

    std::fs::write(e.state().join("release-install"), TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &vars);
    assert_eq!(e.said()["Installed"], "nerd 0.99.0");
    let sum = std::fs::read_to_string(e.served.join(format!("{ARCHIVE}.sha256"))).unwrap();
    let seen = std::fs::read_to_string(root.join("seen.rb")).unwrap();
    assert!(seen.contains(&format!("  url \"https://github.com/nerdterm/nerd/releases/download/{TAG}/{ARCHIVE}\"\n")) && seen.contains(&format!("  sha256 \"{}\"\n", &sum[..64])), "{seen}");
    assert!(std::fs::read_to_string(root.join("cached")).unwrap().trim_end().ends_with(&format!("--{ARCHIVE}")), "primed before brew ran");
    assert!(std::fs::read_to_string(root.join("profile")).unwrap().starts_with("(version 1)(allow default)(deny network*)"));
    assert_eq!(std::fs::read_to_string(tap.join("Formula/nerd.rb")).unwrap(), "class Nerd < Formula\nend\n", "the tap's own formula back");
    let status = Command::new("git").arg("-C").arg(&tap).args(["status", "--porcelain", "--untracked-files=all", "--ignored"]).output().unwrap();
    assert!(status.status.success() && status.stdout.is_empty(), "{}", String::from_utf8_lossy(&status.stdout));
    assert!(!e.home.join(".local/bin").exists(), "nothing in ~/.local/bin");
    let log = e.log();
    for line in ["release v0.99.0: Homebrew 7.0.11", "release v0.99.0: tap checked", "release v0.99.0: formula written", "release v0.99.0: brew upgrade done", "release v0.99.0: tap restored", "release v0.99.0 installed with Homebrew: nerd 0.99.0"] {
        assert!(log.contains(line), "{line}: {log}");
    }
}

#[test]
fn a_homebrew_upgrade_takes_only_a_signed_archive_of_nerd_alone() {
    let e = Env::new("brew-refused");
    let nerd = e.brewed();
    let root = e.home.parent().unwrap().to_path_buf();
    let stand_in = root.join("sandbox-exec");
    let vars = [("NERD_TEST_BREW", stand_in.to_str().unwrap())];
    let go = e.state().join("release-install");

    e.archive_with("#!/bin/sh\necho 'nerd 0.99.0 (test)'\n", &["LICENSE"]);
    std::fs::write(&go, TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &vars);
    assert_eq!(e.said()["Failed"], "this release needs Homebrew's own formula: run brew update && brew upgrade nerd");

    e.archive("#!/bin/sh\necho 'nerd 0.99.0 (test)'\n");
    e.sign("other");
    std::fs::write(&go, TAG).unwrap();
    e.run(&nerd, &["--check-update", "--install", "--window"], &vars);
    assert_eq!(e.said()["Failed"], "the signature does not check against nerd's release keys, nothing installed");

    assert!(!root.join("profile").exists() && !root.join("seen.rb").exists(), "brew never ran");
    assert!(!e.home.join(".local/bin").exists());
}
