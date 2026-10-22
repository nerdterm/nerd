# nerd

Terminal multiplexer for AI agents. Rust 2024, macOS only. Start with `README.md` and `CONTRIBUTING.md`.

## Rules

- Everything in the repository is English: code, comments, UI strings, logs, docs, commits, issues, pull requests.
- No telemetry. The only network code is the proxy (`src/proxy.rs`), run for an approved plugin or for a pane set to Only these providers: outside the server's sandbox, in a sandbox of its own, it connects only to the hosts and ports the plugin's manifest or the chosen agents' manifests name. Beyond it nerd only starts programs: `git` and `ssh` when the person asks, `open` for a link or file they open, and `/usr/bin/curl` for a release check (`src/release.rs`), on the person's word or once a day after they said yes, every check logged; "What leaves nerd" in `docs/privacy-and-security.md` is the full list. The server runs under Seatbelt with networking denied. `cargo deny` stays clean.
- A new way out of nerd (a program that can reach the network, a pane nerd starts or types into, a plugin permission) enters "What leaves nerd" in `docs/privacy-and-security.md` and the allowlist in `tests/egress.rs` in the same change, or not at all.
- Dependencies: permissive licences only (MIT, Apache-2.0, BSD, Zlib, BSL-1.0, WTFPL). No copyleft.
- The server has no tty: never call `Terminal::clear` or `cursor::position` in server code.
- Every action is reachable by mouse and is in a menu; keys (⌥ and a letter, or an optional tmux-style prefix) only repeat them. Menus, settings and windows follow `docs/menus.md`.
- Plugins: a narrow public API over the plugin socket for everyone (`docs/plugins.md`); built-in plugins may also use internal code. Nothing becomes public by accident.
- Config.json is the user's: never overwrite it with defaults; a file that does not parse is left untouched.
- Comments only for what a reader must know not to break the code, one line. Prefer the smallest solution that works. A deliberate shortcut with a known limit is marked `ceiling:`, naming the limit and the way past it.
- Do not claim something works without the command output that shows it.
- No links to private agent sessions in commits, pull requests, issues or files: `scripts/privacy.sh` and the hooks refuse them.

## Build and check

```sh
scripts/setup.sh    # once per clone: git hooks
scripts/gates.sh    # build, tests under an empty HOME, clippy -D warnings, cargo deny, privacy check
```

Install a local build with `install -m 755 target/release/nerd ~/.local/bin/nerd && codesign --force --sign - ~/.local/bin/nerd`. Never `cp` over the old binary: macOS kills it.

Check visible changes in a nested tmux on its own tmux server against a test nerd server with its own home: `tmux -L nt new-session -d -e HOME=/tmp/nt <nerd> --new` (pass the variable with `-e`; no `NERD_SOCKET`, which would point every server of that home at one socket), then read `tmux capture-pane -p`. Stop only the test servers, with `HOME=/tmp/nt <nerd> --stop`; never `pkill -f 'nerd --daemon'`, which also stops your own nerd.

## Workflow

- One issue per change. Branch `feat/…`, `fix/…`, `docs/…`, `chore/…`, `refactor/…`, `perf/…` or `test/…`.
- Conventional commit subjects and pull request titles; `Closes #N` in the body; squash merge into `master`.
- A rename removes the old name outright: no aliases for flags, labels or config keys.
- Releases come from release-please. Never create or move a tag by hand.
