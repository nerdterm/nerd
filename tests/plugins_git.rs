//! Plugins installed from git by the built binary, against repositories built here, some of them hostile.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Env {
    home: PathBuf,
    /// A message server of its own that lets panes act, since the test may run inside a nerd pane.
    msock: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let home = std::env::temp_dir().join(format!("nerd-it-plug-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Library/Application Support/nerd")).unwrap();
        // The pane rule is tested in install.rs.
        Env { home: home.canonicalize().unwrap(), msock: fake_message_server(name) }
    }

    fn state(&self) -> PathBuf {
        self.home.join("Library/Application Support/nerd")
    }

    fn nerd(&self, args: &[&str]) -> (bool, String) {
        self.nerd_with(args, &[])
    }

    fn nerd_with(&self, args: &[&str], vars: &[(&str, &str)]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
            .args(args)
            .envs(vars.iter().copied())
            .env("HOME", &self.home)
            .env("NERD_MSOCK", &self.msock)
            .env_remove("NERD_STATE_DIR")
            .env_remove("NERD_SOCKET")
            .env_remove("NERD_INSTANCE")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    fn lock(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.state().join("plugins.lock")).unwrap_or("[]".into())).unwrap()
    }

    /// A repository of a plugin under HOME, with a git identity of its own so it commits under an empty HOME.
    fn repo(&self, name: &str) -> Repo {
        let dir = self.home.join("src").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let r = Repo { dir, home: self.home.clone() };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
        if let Some(dir) = self.msock.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Answers `Hello` and `PaneRights` with `allow_from_panes` on, as a server with it set would.
fn fake_message_server(name: &str) -> PathBuf {
    use std::io::{BufRead as _, Write as _};
    let dir = PathBuf::from(format!("/tmp/nt-s2a-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let Ok(r) = s.try_clone() else { continue };
            for l in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                let answer = if l.contains("Hello") { r#"{"ok":{"protocol":1,"pane":1,"name":null}}"# } else { r#"{"allow_from_panes":true}"# };
                let _ = s.write_all(format!("{answer}\n").as_bytes());
            }
        }
    });
    path
}

struct Repo {
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(&self.dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.hooksPath=/dev/null"]).args(args).env("HOME", &self.home).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn put(&self, path: &str, text: &str) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A plugin at `version`, committed and tagged `v<version>`; `extra` goes into its manifest.
    fn release(&self, version: &str, extra: serde_json::Value) -> String {
        let mut m = serde_json::json!({ "name": "plug", "title": "Plug", "version": version, "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] });
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        self.put("plugin.json", &m.to_string());
        self.put("p.sh", "exit 0\n");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", version, "--allow-empty"]);
        self.git(&["tag", &format!("v{version}")]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn url(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }
}

#[test]
fn add_installs_the_newest_release_at_its_commit_and_only_once() {
    let env = Env::new("add");
    let repo = env.repo("plug");
    repo.release("1.0.0", serde_json::json!({}));
    let newest = repo.release("1.1.0", serde_json::json!({}));
    repo.put("later.txt", "not released");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-qm", "later"]);

    let (ok, out) = env.nerd(&["--plugin-add", &repo.url()]);
    assert!(ok && out.contains("Nothing changed"), "no terminal and no --yes: {out}");
    assert!(!env.state().join("plugins/plug").exists());

    let (ok, out) = env.nerd(&["--plugin-add", &repo.url(), "--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("release v1.1.0") && out.contains("Risk: only shows things") && out.contains("Runs /bin/sh"), "{out}");
    let installed = env.state().join("plugins/plug");
    assert!(installed.join("plugin.json").is_file() && !installed.join("later.txt").exists(), "the release, not the branch");
    assert!(!installed.join(".git").exists(), "no history under plugins/");
    assert_eq!(env.lock(), serde_json::json!([{ "name": "plug", "source": repo.url(), "tag": "v1.1.0", "commit": newest }]));

    let (ok, out) = env.nerd(&["--plugin-add", &repo.url(), "--yes"]);
    assert!(!ok && out.contains("installed already"), "{out}");
}

#[test]
fn update_takes_a_newer_release_and_asks_again_when_it_can_do_more() {
    let env = Env::new("update");
    let repo = env.repo("plug");
    let one = serde_json::json!({ "commands": [{ "id": "a", "title": "A", "where": "global" }] });
    let two = serde_json::json!({ "commands": [{ "id": "a", "title": "A", "where": "global" }, { "id": "b", "title": "B", "where": "pane" }] });
    repo.release("1.0.0", one.clone());
    assert!(env.nerd(&["--plugin-add", &format!("{}@v1.0.0", repo.url()), "--yes"]).0);
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--yes"]);
    assert!(ok && out.contains("up to date"), "{out}");

    repo.release("1.1.0", two.clone());
    let second = repo.release("1.2.0", two.clone());
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--yes"]);
    assert!(ok && out.contains("Updated plug") && out.contains("It can do nothing it could not before"), "a second menu item is nothing new: {out}");
    assert!(out.contains("New commits:") && out.contains("1.2.0") && out.contains("1.1.0"), "{out}");
    assert_eq!(env.lock()[0]["commit"], second.as_str());

    let mut more = two.clone();
    more["permissions"] = serde_json::json!({ "network": { "hosts": ["example.com"], "ports": [443] } });
    repo.release("1.3.0", more);
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--yes"]);
    assert!(ok && out.contains("It can now also:\n  Reaches only example.com on port 443, through") && out.contains("Nothing changed"), "--yes never grants more: {out}");
    assert_eq!(env.lock()[0]["commit"], second.as_str());
    assert!(!std::fs::read_to_string(env.state().join("plugins/plug/plugin.json")).unwrap().contains("network"));

    // A second update in a row works on the tree the first one left.
    repo.release("1.4.0", two);
    let (ok, out) = env.nerd(&["--plugin-update", "--yes"]);
    assert!(ok && out.contains("Updated plug"), "{out}");
    assert_eq!(env.lock()[0]["tag"], "v1.4.0");
}

#[test]
fn a_hostile_repository_runs_nothing_and_is_refused_before_it_is_written() {
    let env = Env::new("hostile");
    let marker = env.home.join("PWNED");
    let filtered = env.repo("filtered");
    filtered.put(".gitattributes", "* filter=evil\n");
    filtered.release("1.0.0", serde_json::json!({}));
    // Written after the fixture is built: the user's own git config defines the filter the repository asks for.
    let evil = env.home.join("evil.sh");
    std::fs::write(&evil, format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display())).unwrap();
    std::fs::set_permissions(&evil, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::write(env.home.join(".gitconfig"), format!("[filter \"evil\"]\n\tsmudge = {e}\n\tclean = cat\n[core]\n\tfsmonitor = {e}\n", e = evil.display())).unwrap();
    let (ok, out) = env.nerd(&["--plugin-add", &filtered.url(), "--yes"]);
    assert!(ok, "{out}");
    assert!(!marker.exists(), "a filter from the user's config ran on the plugin's files");
    let _ = std::fs::remove_file(env.home.join(".gitconfig"));

    let link = env.repo("link");
    std::os::unix::fs::symlink(env.home.join("Library"), link.dir.join("out")).unwrap();
    link.release("1.0.0", serde_json::json!({ "name": "link" }));
    let (ok, out) = env.nerd(&["--plugin-add", &link.url(), "--yes"]);
    assert!(!ok && out.contains("out links outside the plugin's folder"), "{out}");

    let sub = env.repo("sub");
    sub.release("1.0.0", serde_json::json!({ "name": "sub" }));
    let head = sub.git(&["rev-parse", "HEAD"]);
    sub.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{head},inner")]);
    sub.git(&["commit", "-qm", "sub"]);
    sub.git(&["tag", "-f", "v1.0.0"]);
    let (ok, out) = env.nerd(&["--plugin-add", &sub.url(), "--yes"]);
    assert!(!ok && out.contains("inner is a submodule"), "{out}");

    let lying = env.repo("lying");
    lying.release("1.0.0", serde_json::json!({ "name": "lying", "version": "2.0.0" }));
    let (ok, out) = env.nerd(&["--plugin-add", &lying.url(), "--yes"]);
    assert!(!ok && out.contains("its version 2.0.0 is not its tag v1.0.0"), "{out}");

    for url in [
        format!("--upload-pack=touch {}", marker.display()), format!("ext::sh -c touch% {}", marker.display()), "http://example.com/x".into(), "file:///tmp/x".into(),
        "https://gitlab.com/me/x.git".into(), "git@github.com:me/x.git".into(), "ssh://git@github.com/me/x".into(),
    ] {
        let (ok, out) = env.nerd(&["--plugin-add", &url, "--yes"]);
        assert!(!ok && out.contains("is not a GitHub repository"), "{url}: {out}");
    }
    assert!(!marker.exists());
    let installed: Vec<String> = std::fs::read_dir(env.state().join("plugins")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(installed, ["plug"], "only the filtered one, whose files are its own");
}

#[test]
fn remove_and_off_leave_nothing_behind_and_a_broken_lock_stops_every_change() {
    let env = Env::new("remove");
    let repo = env.repo("plug");
    repo.release("1.0.0", serde_json::json!({}));
    assert!(env.nerd(&["--plugin-add", &repo.url(), "--yes"]).0);
    let data = env.state().join("plugin-data/plug");
    std::fs::create_dir_all(&data).unwrap();

    let (ok, out) = env.nerd(&["--plugin-off", "plug"]);
    assert!(ok, "{out}");
    let off: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(env.state().join("plugins-off.json")).unwrap()).unwrap();
    assert_eq!(off, serde_json::json!({ "plug": "turned off" }));
    assert!(env.nerd(&["--plugin-on", "plug"]).0);
    assert_eq!(std::fs::read_to_string(env.state().join("plugins-off.json")).unwrap().trim(), "{}");

    let (ok, out) = env.nerd(&["--plugin-remove", "plug", "--keep-data"]);
    assert!(ok, "{out}");
    assert!(!env.state().join("plugins/plug").exists() && data.exists());
    assert_eq!(env.lock(), serde_json::json!([]));
    let staging: Vec<_> = std::fs::read_dir(env.state().join("plugin-staging")).unwrap().flatten().filter(|e| e.file_name() != ".lock").collect();
    assert!(staging.is_empty(), "staging is empty after a remove: {staging:?}");

    assert!(env.nerd(&["--plugin-add", &repo.url(), "--yes"]).0);
    assert!(env.nerd(&["--plugin-remove", "plug", "--data"]).0);
    assert!(!data.exists());

    std::fs::write(env.state().join("plugins.lock"), "{ broken").unwrap();
    let (ok, out) = env.nerd(&["--plugin-add", &repo.url(), "--yes"]);
    assert!(!ok && out.contains("plugins.lock does not parse"), "{out}");
    assert_eq!(std::fs::read_to_string(env.state().join("plugins.lock")).unwrap(), "{ broken", "left as it was");

    std::fs::write(env.state().join("plugins.lock"), serde_json::json!([{ "name": "plug", "source": repo.url(), "commit": "0".repeat(40) }]).to_string()).unwrap();
    let lock = env.home.join("other.lock");
    std::fs::copy(env.state().join("plugins.lock"), &lock).unwrap();
    let (ok, out) = env.nerd(&["--plugin-add", "--lock", &lock.to_string_lossy(), "--yes"]);
    assert!(!ok && out.contains("is not a GitHub repository"), "a lock from elsewhere never names a local folder: {out}");
}

#[test]
fn a_tag_or_commit_after_the_address_is_what_installs() {
    let env = Env::new("at");
    let repo = env.repo("plug");
    let first = repo.release("1.0.0", serde_json::json!({}));
    repo.release("1.1.0", serde_json::json!({}));
    let (ok, out) = env.nerd(&["--plugin-add", &format!("{}@{first}", repo.url()), "--yes"]);
    assert!(ok, "{out}");
    assert_eq!(env.lock()[0]["commit"], first.as_str());
    assert_eq!(env.lock()[0]["tag"], serde_json::Value::Null);
    let _ = Path::new("");
}

#[test]
fn a_trial_runs_the_plugin_without_network_and_says_what_it_did() {
    let env = Env::new("trial");
    let (ok, out) = env.nerd(&["--plugin-trial", concat!(env!("CARGO_MANIFEST_DIR"), "/templates/segment")]);
    assert!(ok && out.contains("  connected\n") && out.contains("it sent Segment"), "{out}");

    let quiet = env.home.join("quiet");
    std::fs::create_dir_all(&quiet).unwrap();
    std::fs::write(quiet.join("plugin.json"), serde_json::json!({ "name": "quiet", "title": "Q", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"], "permissions": { "network": { "hosts": ["one.one.one.one"], "ports": [80] } } }).to_string()).unwrap();
    // It asks for the network and tries it; the trial gives it none.
    std::fs::write(quiet.join("p.sh"), "/usr/bin/nc -z -w 2 1.1.1.1 80 && echo NETWORK-REACHED || echo no-network\nexit 3\n").unwrap();
    let (ok, out) = env.nerd(&["--plugin-trial", &quiet.to_string_lossy()]);
    assert!(!ok && out.contains("it never connected") && out.contains("it exited on its own"), "{out}");
    assert!(out.contains("Reaches only one.one.one.one on port 80") && out.contains("no-network") && !out.contains("NETWORK-REACHED"), "{out}");

    // Its log is its to swap: a link to a secret prints nothing of it, a pipe does not hang the trial.
    std::fs::create_dir_all(env.home.join(".ssh")).unwrap();
    std::fs::write(env.home.join(".ssh/id_test"), "PRIVATE-KEY-MATERIAL\n").unwrap();
    std::fs::create_dir_all(env.home.join("other")).unwrap();
    std::fs::write(env.home.join("other/output.log"), "PRIVATE-KEY-MATERIAL\n").unwrap();
    let swaps = [
        ("linker", "rm -f \"$NERD_PLUGIN_DATA/output.log\"; ln -s \"$HOME/.ssh/id_test\" \"$NERD_PLUGIN_DATA/output.log\""),
        ("piper", "rm -f \"$NERD_PLUGIN_DATA/output.log\"; mkfifo \"$NERD_PLUGIN_DATA/output.log\""),
        ("swapper", "rm -rf \"$NERD_PLUGIN_DATA\"; ln -s \"$HOME/other\" \"$NERD_PLUGIN_DATA\""),
    ];
    for (name, swap) in swaps {
        let dir = env.home.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), serde_json::json!({ "name": name, "title": "S", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] }).to_string()).unwrap();
        std::fs::write(dir.join("p.sh"), format!("{swap}\nexit 0\n")).unwrap();
        // A hang must fail the test, not stall it.
        let (tx, rx) = std::sync::mpsc::channel();
        let (home, path) = (env.home.clone(), dir.to_string_lossy().into_owned());
        std::thread::spawn(move || {
            let out = Command::new(env!("CARGO_BIN_EXE_nerd")).args(["--plugin-trial", &path]).env("HOME", &home).env_remove("NERD_STATE_DIR").env_remove("NERD_SOCKET").env_remove("NERD_INSTANCE").stdin(Stdio::null()).output().unwrap();
            let _ = tx.send(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        });
        let out = rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap_or_else(|_| panic!("{name}: the trial hung"));
        assert!(!out.contains("PRIVATE-KEY-MATERIAL") && out.contains("it exited on its own"), "{name}: {out}");
    }

    // A plugin inside the state root still reads its own folder: the deny of the real root comes before it.
    let inside = env.state().join("plugins/segment");
    std::fs::create_dir_all(&inside).unwrap();
    for f in ["plugin.json", "plugin.sh"] {
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/segment/").to_string() + f, inside.join(f)).unwrap();
    }
    let (ok, out) = env.nerd(&["--plugin-trial", &inside.to_string_lossy()]);
    assert!(ok && out.contains("  connected\n"), "{out}");
}

#[test]
fn an_agent_plugin_s_hooks_leave_when_it_is_off_or_removed_and_come_back_when_on() {
    let env = Env::new("hooks");
    let repo = env.repo("agent");
    std::fs::create_dir_all(env.home.join(".plug")).unwrap();
    repo.put("plugin.json", &serde_json::json!({ "name": "plug", "title": "Plug", "version": "1.0.0", "agent": { "names": ["plug"], "config": { "path": "~/.plug/hooks.json", "shape": "nested" }, "arg": "plug", "events": [{ "on": "Stop", "do": "idle" }] } }).to_string());
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-qm", "one"]);
    repo.git(&["tag", "v1.0.0"]);
    let (ok, out) = env.nerd(&["--plugin-add", &repo.url(), "--yes"]);
    assert!(ok && out.contains("Risk: hooks into an agent") && out.contains("adding nerd's hooks to ~/.plug/hooks.json"), "{out}");
    let answer = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nerd")).args(args).env("HOME", &env.home).env_remove("NERD_STATE_DIR").env_remove("NERD_SOCKET").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    let out = answer(&["--install-hook"], "y\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let hooked = || std::fs::read_to_string(env.home.join(".plug/hooks.json")).unwrap_or_default().contains("nerd-agent-state");
    assert!(hooked(), "installed");
    assert!(env.nerd(&["--plugin-off", "plug"]).0);
    assert!(!hooked(), "off takes nerd's hooks out");
    assert!(env.nerd(&["--plugin-on", "plug"]).0);
    assert!(hooked(), "on puts them back");
    assert!(env.nerd(&["--plugin-remove", "plug", "--keep-data"]).0);
    assert!(!hooked(), "remove takes them out");
}

#[test]
fn no_git_config_proxy_netrc_or_askpass_of_the_user_reaches_a_fetch() {
    let env = Env::new("isolated");
    // Anything that honoured the user's setup would connect here instead of to GitHub.
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mirror = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (proxy_at, mirror_at) = (proxy.local_addr().unwrap(), mirror.local_addr().unwrap());
    std::fs::write(env.home.join(".gitconfig"), format!("[url \"http://{mirror_at}/\"]\n\tinsteadOf = https://github.com/\n[http]\n\tproxy = http://{proxy_at}\n")).unwrap();
    std::fs::write(env.home.join(".netrc"), "machine github.com login someone password secret\n").unwrap();
    let marker = env.home.join("ASKED");
    let askpass = env.home.join("askpass.sh");
    std::fs::write(&askpass, format!("#!/bin/sh\ntouch {}\necho secret\n", marker.display())).unwrap();
    std::fs::set_permissions(&askpass, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let proxied = format!("http://{proxy_at}");
    let vars = [("https_proxy", proxied.as_str()), ("HTTPS_PROXY", proxied.as_str()), ("ALL_PROXY", proxied.as_str()), ("SSH_ASKPASS", askpass.to_str().unwrap()), ("SSH_ASKPASS_REQUIRE", "force"), ("XDG_CONFIG_HOME", env.home.to_str().unwrap())];
    let (ok, out) = env.nerd_with(&["--plugin-add", "https://github.com/nerdterm/no-such-plugin-for-a-test", "--yes"], &vars);
    assert!(!ok, "{out}");
    for (l, what) in [(&proxy, "a proxy"), (&mirror, "an insteadOf address")] {
        l.set_nonblocking(true).unwrap();
        assert!(l.accept().is_err(), "the fetch went through {what}: {out}");
    }
    assert!(!marker.exists(), "an askpass of the user's ran: {out}");
}

#[test]
fn an_update_of_a_plugin_from_another_host_is_refused_before_any_fetch() {
    let env = Env::new("elsewhere");
    std::fs::write(env.state().join("plugins.lock"), serde_json::json!([{ "name": "plug", "source": "https://gitlab.com/me/plug.git", "commit": "a".repeat(40) }]).to_string()).unwrap();
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--yes"]);
    assert!(!ok && out.contains("plug was installed from https://gitlab.com/me/plug.git; nerd installs from GitHub only"), "{out}");
}

#[test]
fn a_lock_from_elsewhere_is_checked_whole_before_anything_is_fetched() {
    let env = Env::new("lock");
    let lock = env.home.join("other.lock");
    let pin = |name: &str, commit: &str| serde_json::json!({ "name": name, "source": "https://github.com/me/x.git", "commit": commit });
    for (pins, why) in [
        (serde_json::json!([pin("a", "not-a-commit")]), "is not 40 characters"),
        (serde_json::json!([pin("a", &"a".repeat(40)), pin("a", &"b".repeat(40))]), "in the lock twice"),
        (serde_json::json!([pin("../x", &"a".repeat(40))]), "is not a plugin's name"),
    ] {
        std::fs::write(&lock, pins.to_string()).unwrap();
        let (ok, out) = env.nerd(&["--plugin-add", "--lock", &lock.to_string_lossy(), "--yes"]);
        assert!(!ok && out.contains(why), "{why}: {out}");
    }
}

#[test]
fn a_plugin_that_streams_without_a_newline_cannot_hold_the_trial() {
    let env = Env::new("flood");
    let flood = env.home.join("flood");
    std::fs::create_dir_all(&flood).unwrap();
    std::fs::write(flood.join("plugin.json"), serde_json::json!({ "name": "flood", "title": "F", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] }).to_string()).unwrap();
    std::fs::write(flood.join("p.sh"), "/usr/bin/yes | /usr/bin/tr -d '\\n' | /usr/bin/nc -U \"$NERD_SOCKET\"\n").unwrap();
    let start = std::time::Instant::now();
    let (ok, out) = env.nerd(&["--plugin-trial", &flood.to_string_lossy()]);
    assert!(!ok && out.contains("its first line was not Connect"), "{out}");
    assert!(start.elapsed() < std::time::Duration::from_secs(40), "it took {:?}", start.elapsed());
}

#[test]
fn a_plugin_that_drips_bytes_without_a_newline_cannot_hold_the_trial() {
    let env = Env::new("drip");
    for (name, script) in [("drip", "while :; do printf x; sleep 0.1; done | /usr/bin/nc -U \"$NERD_SOCKET\"\n"), ("dripper", "{ printf '{\"Connect\":{\"protocol\":2,\"role\":\"plugin\",\"plugin\":\"dripper\"}}\\n'; while :; do printf x; sleep 0.1; done; } | /usr/bin/nc -U \"$NERD_SOCKET\"\n")] {
        let dir = env.home.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), serde_json::json!({ "name": name, "title": "D", "exec": { "command": "sh", "args": ["p.sh"] }, "events": ["command"] }).to_string()).unwrap();
        std::fs::write(dir.join("p.sh"), script).unwrap();
        let start = std::time::Instant::now();
        let (_, out) = env.nerd(&["--plugin-trial", &dir.to_string_lossy()]);
        assert!(start.elapsed() < std::time::Duration::from_secs(40), "{name} held it {:?}: {out}", start.elapsed());
    }
}

#[test]
fn the_window_s_hidden_fetch_only_stages_says_nothing_on_screen_and_ends_its_log_with_why() {
    let env = Env::new("window");
    let repo = env.repo("plug");
    repo.release("1.0.0", serde_json::json!({}));
    assert!(env.nerd(&["--plugin-add", &repo.url(), "--yes"]).0);
    let log = || std::fs::read_to_string(env.state().join("plugin-staging/fetch.log")).unwrap();

    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--window"]);
    assert!(ok && out.is_empty() && log().trim() == "plug is up to date.", "{out}{}", log());

    repo.release("1.1.0", serde_json::json!({}));
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--window"]);
    assert!(ok && out.is_empty(), "nothing reaches a screen: {out}");
    assert!(log().contains("release v1.1.0") && log().contains("Answer in nerd's Plugins window."), "{}", log());
    let ready: Vec<_> = std::fs::read_dir(env.state().join("plugin-staging")).unwrap().flatten().filter(|e| e.path().join("ready.json").is_file()).collect();
    assert_eq!(ready.len(), 1, "staged for the window's question, even where panes may install");
    assert_eq!(env.lock()[0]["tag"], "v1.0.0", "nothing placed");

    let (ok, _) = env.nerd(&["--plugin-update", "plug", "--window", "--yes"]);
    assert!(!ok && log().lines().last().unwrap().starts_with("nerd: refused: --yes"), "{}", log());

    // Its repository is gone: git fails at once, never asks, and its whole error is in the log.
    std::fs::rename(repo.url(), format!("{}-gone", repo.url())).unwrap();
    let (ok, out) = env.nerd(&["--plugin-update", "plug", "--window"]);
    let last = log().lines().last().unwrap().to_string();
    assert!(!ok && out.is_empty() && last.starts_with("nerd: git clone:") && log().lines().count() > 1, "{}", log());
}

/// A plugin may write its data folder, so it can swap its log for a link to any file of yours; the next start must
/// not follow it. Its environment carries only what it needs, no token of the broker's.
#[test]
fn a_plugin_log_that_became_a_link_is_replaced_not_followed() {
    let env = Env::new("loglink");
    let victim = env.home.join(".zshrc");
    std::fs::write(&victim, "keep me\n").unwrap();
    let data = env.home.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::os::unix::fs::symlink(&victim, data.join("output.log")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nerd"))
        .args(["--plugin-exec", "/bin/sh", "-c", "echo FROM-PLUGIN; env"])
        .env("NERD_PLUGIN_PROFILE", "(version 1)\n(allow default)\n")
        .env("NERD_PLUGIN_LOG", data.join("output.log"))
        .env("GITHUB_TOKEN", "secret-of-the-broker")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me\n", "the link's target is untouched");
    let log = data.join("output.log");
    assert!(!std::fs::symlink_metadata(&log).unwrap().file_type().is_symlink(), "a file of its own now");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("FROM-PLUGIN") && text.contains("PATH=") && !text.contains("secret-of-the-broker"), "{text}");
}

/// A process that left the pane's tree still carries the pane's marker: its --yes is refused too.
#[test]
fn yes_is_refused_to_a_process_that_left_the_panes_tree() {
    let env = Env::new("detached");
    std::fs::write(env.state().join("config.json"), "{}").unwrap();
    let repo = env.repo("plug");
    repo.release("1.0.0", serde_json::json!({}));
    let out = env.home.join("out.txt");
    // A subshell in the background leaves its parent at once: launchd adopts the command.
    let line = format!("( {:?} --plugin-add {:?} --yes > {:?} 2>&1 & )", env!("CARGO_BIN_EXE_nerd"), repo.dir, out);
    let ok = Command::new("/bin/sh").args(["-c", &line]).env("HOME", &env.home).env("NERD_PANE", "7").env_remove("NERD_STATE_DIR").env_remove("NERD_SOCKET").env_remove("NERD_INSTANCE").status().unwrap().success();
    assert!(ok);
    let done = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !std::fs::read_to_string(&out).is_ok_and(|t| t.contains("nerd:") || t.contains("Installed")) && std::time::Instant::now() < done {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let said = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(said.contains("refused: --yes") && !said.contains("Installed"), "{said}");
    assert!(env.lock().as_array().is_none_or(|a| a.is_empty()), "nothing installed");
}

/// Turning a plugin back on, or removing it, is the Plugins window's to do when asked from a pane.
#[test]
fn on_and_remove_are_refused_from_a_pane() {
    let env = Env::new("pane-on");
    std::fs::write(env.state().join("config.json"), "{}").unwrap();
    for args in [&["--plugin-on", "plug"][..], &["--plugin-remove", "plug"], &["--plugin-on", "plug", "--window"], &["--plugin-remove", "plug", "--data", "--window"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_nerd")).args(args).env("HOME", &env.home).env("NERD_PANE", "7").env_remove("NERD_STATE_DIR").env_remove("NERD_SOCKET").env_remove("NERD_INSTANCE").stdin(Stdio::null()).output().unwrap();
        // With --window its words go to the fetch log.
        let said = format!("{}{}", String::from_utf8_lossy(&out.stderr), std::fs::read_to_string(env.state().join("plugin-staging/fetch.log")).unwrap_or_default());
        assert!(!out.status.success() && said.contains(&format!("refused: {} from inside a pane", args[0])), "{args:?}: {said}");
    }
}
