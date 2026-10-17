//! The two commands the other Mac runs for `nerd --remote`, as the built binary answers them.

use std::process::{Command, Stdio};

fn nerd(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nerd"))
        .args(args)
        .env("HOME", home)
        .env_remove("NERD_INSTANCE")
        .env_remove("NERD_SOCKET")
        .env_remove("NERD_STATE_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn the_relay_never_starts_a_server_and_names_each_refusal() {
    let home = std::env::temp_dir().join(format!("nerd-it-relay-{}", std::process::id()));
    let instance = home.join("Library/Application Support/nerd/instances/1");
    std::fs::create_dir_all(&instance).unwrap();

    let out = nerd(&home, &["--relay", "1"]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "nerd: server 1 is not running\n");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!instance.join("nerd.sock").exists(), "no server was started");
    assert_eq!(nerd(&home, &["--relay", "9"]).status.code(), Some(4));
    assert_eq!(nerd(&home, &["--relay", "../1"]).status.code(), Some(5));
    assert_eq!(nerd(&home, &["--relay"]).status.code(), Some(5));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_list_for_another_mac_is_versioned_json() {
    let home = std::env::temp_dir().join(format!("nerd-it-list-{}", std::process::id()));
    std::fs::create_dir_all(home.join("Library/Application Support/nerd/instances/1")).unwrap();

    let out = nerd(&home, &["--list", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, serde_json::json!({"relay": 1, "instances": [{"id": "1", "label": "1", "running": false, "spaces": 0}]}));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn export_logs_names_the_file_and_the_old_flag_is_an_unknown_option() {
    let home = std::env::temp_dir().join(format!("nerd-it-export-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    let out = nerd(&home, &["--export-logs", "1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (path, rest) = text.split_once('\n').unwrap();
    assert!(std::path::Path::new(path).exists() && rest == "Nothing was sent.\n", "{text}");
    assert_eq!(nerd(&home, &["--export-logs", "x"]).status.code(), Some(1), "hours are a number");

    let old = nerd(&home, &["--report"]);
    assert_eq!(old.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&old.stderr).starts_with("nerd: unknown option --report"), "{}", String::from_utf8_lossy(&old.stderr));

    let _ = std::fs::remove_dir_all(&home);
}
