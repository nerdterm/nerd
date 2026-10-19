# Contributing to nerd

nerd is maintained by one person in their own time. These rules keep that sustainable.

## Before you start

- **Bugs:** open an issue with the bug report template. Include your macOS version, Mac model, terminal and `nerd --version`.
- **Ideas:** open an issue with the feature request template. Expect many to be declined: nerd stays small, and macOS only until the maintainer adds other platforms.
- **Security problems:** do not open an issue. Follow [SECURITY.md](SECURITY.md).
- **Questions about your own setup:** check the README, [docs/troubleshooting.md](docs/troubleshooting.md) and existing issues first.

There is no guaranteed response time.

## Pull requests

Pull requests are accepted **only for issues labelled `accepted`**. Comment on the issue before you start, so two people do not do the same work. Pull requests without an accepted issue are closed without review.

Out of scope, and closed:

- support for Linux, Windows or other platforms: planned by the maintainer, pull requests for them are not accepted;
- network code in nerd itself beyond the plugin and pane proxy, telemetry and update checks: nerd reaches the network only by starting `git` or `ssh` when the user asks, and through plugins the user approved and panes set to Only these providers ([docs/privacy-and-security.md](docs/privacy-and-security.md), "What leaves nerd");
- new dependencies under a copyleft licence (GPL, LGPL, AGPL, MPL);
- large refactors and reformatting unrelated to the issue.

## Development setup

Requires macOS and Rust 1.88 or newer.

```sh
git clone https://github.com/nerdterm/nerd.git
cd nerd
scripts/setup.sh
```

`setup.sh` enables the git hooks. Before every push they run `scripts/privacy.sh`, which refuses key material, tokens and private agent session links in what is pushed, and `scripts/gates.sh`: build, tests, clippy with warnings as errors, `cargo deny` and the same privacy check over the tree.

`scripts/terminals.sh` runs nerd in Ghostty, Kitty, WezTerm, iTerm2 and Terminal.app, each on a test server of its own, and prints a table of what reached nerd from each: graphics, keys, paste, mouse and colour. Run it from Terminal.app after a change to how nerd talks to the terminal. Typing into the test windows needs Accessibility, and screenshots need Screen Recording, for the terminal it runs in; without them those columns say so. It types nothing into a terminal while another instance of that terminal is running, because the input could reach that instance instead.

If you use Claude Code, the repository's `.claude/settings.json` applies to you: it denies `git push --no-verify` and destructive git commands, and its hook (`scripts/privacy-hook.sh`) refuses a command that carries a private session link, or a `gh` call that would post one.

## Making a change

1. Fork the repository and create a branch named `feat/…`, `fix/…`, `docs/…`, `chore/…`, `refactor/…`, `perf/…` or `test/…`.
2. Keep the change focused on the issue. Add or update tests for behaviour you change.
3. Use [Conventional Commits](https://www.conventionalcommits.org): `fix: tree scrolls past the last row`.
4. Open the pull request against `master`. The title must be a conventional commit subject, and the description must reference the issue, for example `Closes #42`. A check refuses pull requests without both.
5. Keep comments short: only what a reader needs in order not to break the code.

Pull requests are squash merged. Versions and the changelog are produced by release-please; do not edit the version or `CHANGELOG.md`.

## Licence

By contributing you agree that your contribution is licensed under the [MIT License](LICENSE).

## Conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
