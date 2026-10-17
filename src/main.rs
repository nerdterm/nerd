//! nerd: a terminal multiplexer for working with multiple agents.

mod agents;
mod app;
mod battery;
mod bell;
mod cf;
mod clock;
mod catalog;
mod changes;
mod broker;
mod chrome;
mod client;
mod clients;
mod config;
mod daemon;
mod detect;
mod ghostty;
mod git;
mod hook;
mod install;
mod isolation;
mod isolation_ui;
mod keys;
mod keyshelp;
mod log;
mod menu;
mod layout;
mod links;
mod mail;
mod mailcli;
mod messages;
mod mouse;
mod msock;
mod navigator;
mod notify;
mod panel;
mod paths;
mod plugin_list;
mod plugins;
mod policy;
mod pointer;
mod proxy;
mod preview;
mod previews;
mod proto;
mod pty;
mod render;
mod report;
mod run;
mod sandbox;
mod search;
mod select;
mod session;
mod settings;
mod signals;
mod sidebar;
mod slots;
mod spawn;
mod space_picker;
mod term;
mod transcript;
mod trial;
mod transport;
mod tree;
mod ui;
mod worker;

/// Internal: set on the client nerd starts from its own offer of a window outside tmux,
/// so that client never makes the offer again by itself.
fn from_window(args: &[String]) -> bool {
    args.iter().any(|a| a == "--from-window")
}

/// The instance `--resume` was given, if any: the flag is not a name.
fn resume_name(args: &[String]) -> Option<String> {
    args.get(1).filter(|a| *a != "--from-window").cloned()
}

/// Commands that print and exit end quietly on a closed pipe (`nerd --list | head -1`), as Unix
/// tools do. Rust ignores SIGPIPE, and `println!` into a closed pipe panics.
/// The daemon, broker and interactive client keep ignoring it: a closed socket must not kill them.
fn quiet_on_closed_pipe(first: Option<&str>) {
    // --remote writes into ssh's stdin, and a dead ssh must end in a message, not a silent kill.
    if matches!(first, None | Some("--daemon") | Some("--broker") | Some("--exec-broker") | Some("--resume") | Some("--remote")) {
        return;
    }
    // SAFETY: restoring the default disposition installs no handler.
    let _ = unsafe { nix::sys::signal::signal(nix::sys::signal::Signal::SIGPIPE, nix::sys::signal::SigHandler::SigDfl) };
}

/// Errors reach people: said plainly, not in Rust's debug form (`Custom { kind: Other, .. }`).
fn main() {
    if let Err(e) = run() {
        eprintln!("nerd: {e}");
        std::process::exit(1);
    }
}

fn run() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    quiet_on_closed_pipe(args.first().map(String::as_str));
    let from_window = from_window(&args);
    match args.first().map(String::as_str) {
        Some("--daemon") => {
            log::set_role("server");
            return daemon::run();
        }
        // Internal: the daemon launches this itself.
        Some("--broker") => {
            log::set_role("broker");
            return broker::run();
        }
        // Internal: after an update, an older broker starts this beside itself for plugins.
        Some("--exec-broker") => {
            log::set_role("broker");
            return broker::run_beside(args.get(1).map(String::as_str).unwrap_or_default());
        }
        Some("--sandbox-selftest") => return sandbox::selftest(),
        // Internal: the broker launches plugins through this.
        Some("--plugin-exec") => return plugins::exec_main(&args[1..]),
        Some("--pane-exec") => return isolation::pane_exec_main(&args[1..]),
        Some("--plugin-check") => return install::check_main(&args[1..]),
        Some("--plugin-trial") => return trial::main(&args[1..]),
        Some(f @ ("--plugin-add" | "--plugin-update" | "--plugin-remove" | "--plugin-off" | "--plugin-on" | "--plugin-catalog")) => return install::main(f, &args[1..]),
        Some("--status") | Some("--list") => return client::status(args.iter().any(|a| a == "--json")),
        Some("--remote") => {
            log::set_role("client");
            return client::remote(args.get(1).cloned(), resume_name(&args[1..]), from_window);
        }
        // Internal: what `--remote` runs on the other Mac, over ssh.
        Some("--relay") => {
            log::set_role("client");
            client::relay(args.get(1).cloned())
        }
        Some("--stop") => return client::stop(args.get(1).cloned()),
        Some("--update") => return client::update(args.get(1).cloned()),
        Some("--disconnect") => return client::disconnect(args.get(1).cloned()),
        Some("--rename") if args.len() > 3 => {
            return Err(std::io::Error::other("usage: nerd --rename <server> [name]; a name is one word"));
        }
        Some("--rename") => return client::rename(args.get(1).cloned(), args.get(2).cloned()),
        Some("--resume") => {
            log::set_role("client");
            return client::run(Some(resume_name(&args)), from_window);
        }
        Some("--install") => return install_autostart(),
        Some("--install-keys") => {
            ghostty::link()?;
            println!("Keys written: {}\nIncluded from {}", ghostty::keys_path().display(), ghostty::ghostty_config().display());
            return Ok(());
        }
        Some("--uninstall-keys") => return ghostty::unlink(),
        Some("--install-hook") => {
            let first = hook::first_installs();
            if !first.is_empty() {
                print!("{}", first.prompt());
                std::io::Write::flush(&mut std::io::stdout())?;
                let yes = install::answered_yes(std::io::stdin().lock())?;
                log::info(&format!("first hook install from the command line {}: {}", if yes { "confirmed" } else { "declined" }, first.summary()));
                if !yes {
                    println!("Nothing installed");
                    return Ok(());
                }
            }
            for (agent, done) in hook::install()? {
                match done {
                    Ok(file) => println!("{agent}: {}", file.display()),
                    Err(e) => println!("{agent}: not installed, {e}"),
                }
            }
            return Ok(());
        }
        Some("--uninstall-hook") => {
            let gone = hook::uninstall()?;
            println!("{}", if gone.is_empty() { "No hook was installed".to_string() } else { format!("Removed from {}", gone.join(", ")) });
            return Ok(());
        }
        Some("--uninstall") => {
            let _ = ghostty::unlink();
            return config::autostart::uninstall();
        }
        Some("--export-logs") => {
            let hours = match args.get(1) {
                Some(h) => h.parse().map_err(|_| std::io::Error::other("usage: nerd --export-logs [hours]"))?,
                None => 24,
            };
            let path = report::write(hours, &config::load_or_init(), &[])?;
            println!("{}\nNothing was sent.", path.display());
            return Ok(());
        }
        Some("--config") => {
            println!("{}", paths::config_path().display());
            return Ok(());
        }
        Some("--version") => {
            println!("{}", version_line());
            return Ok(());
        }
        // Internal: the running server asks a new build what it brings.
        Some("--changes-since") => {
            for row in changes::since(args.get(1).map(String::as_str).unwrap_or_default()) {
                println!("{}", row.join("\t"));
            }
            return Ok(());
        }
        Some("send") => return mailcli::send(&args[1..]),
        Some("agents") => return mailcli::agents(&args[1..]),
        Some("inbox") => return mailcli::inbox(&args[1..]),
        Some("wait") => return mailcli::wait(&args[1..]),
        Some("--hook-relay") => return mailcli::hook_relay(&args[1..]),
        Some("--help") | Some("-h") => {
            print_help();
            return Ok(());
        }
        Some(flag) if flag.starts_with('-') && flag != "--from-window" => {
            return Err(std::io::Error::other(format!("unknown option {flag}; nerd --help lists them")));
        }
        _ => {}
    }
    log::set_role("client");
    client::run(None, from_window)
}

/// Format: `nerd 0.2.0 (f4db629, 2026-09-16 15:03)`, `+` after the commit if the tree was dirty at build time.
pub fn version_line() -> String {
    format!("nerd {} ({})", env!("CARGO_PKG_VERSION"), env!("NERD_BUILD"))
}

fn print_help() {
    let installed = if config::autostart::is_installed() {
        "   [installed]"
    } else {
        ""
    };
    println!(
        "nerd: a multiplexer for agents\n\
         \n\
         Running with no arguments opens a new server with its own desktop.\n\
         \n\
           --resume [NAME]   resume a server by name or number (the last used without one)\n\
           --rename N [NAME] name a server, or clear its name\n\
           --list            servers: running or stopped, sandbox, spaces\n\
           --remote M [NAME] a server on machine M from remotes in the settings file, over ssh\n\
           --update [NAME]   swap running servers for the installed build; panes survive\n\
           --stop [NAME]     stop one server, or all of them\n\
           --disconnect [NAME]  end every connection to a server, or to all; programs keep running\n\
           --daemon          start the server (usually automatic)\n\
           --install         install autostart (LaunchAgent){}\n\
           --uninstall       remove autostart\n\
           --install-hook    status hook in every agent found (Claude Code, Codex, Cursor, ...){}\n\
           --install-keys    option shortcuts for Ghostty, layout-independent{}\n\
           --uninstall-keys  remove them\n\
           --uninstall-hook  remove the hooks\n\
           --plugin-check DIR [--catalog] [--json]  check a plugin folder: what it can do, and any problem\n\
           --plugin-add URL[@TAG]  install a plugin from https://github.com/OWNER/REPO, or from a folder you develop or checked\n\
                           yourself, after showing what it can do; --lock FILE installs a lock's\n\
           --plugin-update [NAME]  a newer release, asking again when it can do more\n\
           --plugin-remove NAME    remove a plugin; --data also deletes its data\n\
           --plugin-off NAME, --plugin-on NAME  turn a plugin off or on\n\
           --plugin-catalog  fetch the plugin catalog; a release it revokes is turned off\n\
           --plugin-trial DIR  run a plugin once in its sandbox with no network, send it its events, say what it did\n\
           send NAME|all|you TEXT  send a message to a named pane (TEXT - reads stdin); --reply ID, --ask-route NAME, --wait SECS\n\
           agents        the named panes this pane may write to\n\
           inbox [--all] [--show ID]  messages to this pane\n\
           wait ID [--thread] [--count N] [--timeout S]  wait for replies (600 s)\n\
           --config      path to the settings file\n\
           --export-logs [HOURS]  save the logs of the last hours (24) in a file to read and attach to a bug report\n\
           --sandbox-selftest  test the sandbox profile on itself\n\
           --version     version\n\
         \n\
         Right click anything for its menu. Keys:\n\
           ⌥+K  keys        ⌥+M  main menu   ⌥+/  search the pane\n\
           ⌥+T  file tree   ⌥+N  navigator   ⌥+,  settings\n\
           ⌥+C  new tab     ⌥+W  close pane  ⌥+Q  detach\n\
           ⌥+-  split down  ⌥+\\  split side  ⌥+Z  full screen\n\
           ⌥+S  sidebar     ⌥+U  update      ⌥+shift+arrow  move between panes and areas\n\
           Esc close a menu, dialog or search",
        installed,
        if hook::is_installed() { "   [installed]" } else { "" },
        if ghostty::is_linked() { "   [installed]" } else { "" }
    );
}

fn install_autostart() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    config::autostart::install(&exe)?;
    let p = config::autostart::plist_path();
    println!(
        "Autostart written: {}\nEnable it now: launchctl load -w {}",
        p.display(),
        p.display()
    );
    Ok(())
}

/// Excludes the tab bar; the sidebar is subtracted later at draw time since it toggles at runtime.
pub fn body_area(size: ratatui::layout::Size) -> ratatui::layout::Rect {
    ratatui::layout::Rect::new(0, 1, size.width, size.height.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::{from_window, resume_name};

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_flag_is_found_anywhere_and_is_never_an_instance_name() {
        assert!(from_window(&args(["--resume", "3", "--from-window"].as_ref())));
        assert!(!from_window(&args(["--resume", "3"].as_ref())));

        assert_eq!(resume_name(&args(["--resume", "3", "--from-window"].as_ref())), Some("3".into()));
        assert_eq!(resume_name(&args(["--resume", "--from-window"].as_ref())), None, "the flag is not a name");
        assert_eq!(resume_name(&args(["--resume"].as_ref())), None);
    }
}
