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
- network code in nerd itself beyond the plugin and pane proxy, and telemetry: nerd reaches the network only by starting `git` or `ssh` when the user asks, `curl` for a release check the user asked for or allowed, and through plugins the user approved and panes set to Only these providers ([docs/privacy-and-security.md](docs/privacy-and-security.md), "What leaves nerd");
- new dependencies under a copyleft licence (GPL, LGPL, AGPL, MPL);
- large refactors and reformatting unrelated to the issue.

## Development setup

Requires macOS and Rust 1.92 or newer.

```sh
git clone https://github.com/nerdterm/nerd.git
cd nerd
scripts/setup.sh
```

`setup.sh` enables the git hooks. Before every push they run `scripts/privacy.sh`, which refuses key material, tokens and private agent session links in what is pushed, and `scripts/gates.sh`: build, tests, clippy with warnings as errors, `cargo deny` and the same privacy check over the tree.

`scripts/terminals.sh` runs nerd in Ghostty, Kitty, WezTerm and iTerm2, each on a test server of its own, and prints a table of what reached nerd from each: graphics, keys, paste, mouse, colour, on a U.S. or ABC layout every Option shortcut, and every shortcut through Control + B, on that layout and with the Russian layout's characters. The Kitty and iTerm2 Option rows read ok only with the left Option key sending Alt; `scripts/terminals.sh --preset` applies each terminal's `nerd --preset` to its test home and starts it on that config alone, so Ghostty, Kitty and WezTerm show the preset's effect (iTerm2 reads only the real home's settings and is not checked so). Run it from Terminal.app, which it does not test, after a change to how nerd talks to the terminal. Typing into the test windows needs Accessibility, and screenshots need Screen Recording, for the terminal it runs in; without them those columns say so. It types nothing into a terminal while another instance of that terminal is running, because the input could reach that instance instead.

If you use Claude Code, the repository's `.claude/settings.json` applies to you: it denies `git push --no-verify` and destructive git commands, and its hook (`scripts/privacy-hook.sh`) refuses a command that carries a private session link, or a `gh` call that would post one.

## Making a change

1. Fork the repository and create a branch named `feat/…`, `fix/…`, `docs/…`, `chore/…`, `refactor/…`, `perf/…` or `test/…`.
2. Keep the change focused on the issue. Add or update tests for behaviour you change.
3. Use [Conventional Commits](https://www.conventionalcommits.org): `fix: tree scrolls past the last row`.
4. Open the pull request against `master`. The title must be a conventional commit subject, and the description must reference the issue, for example `Closes #42`. A check refuses pull requests without both.
5. Keep comments short: only what a reader needs in order not to break the code.

Pull requests are squash merged. Versions and the changelog are produced by release-please; do not edit the version or `CHANGELOG.md`.

## Versions

nerd follows [Semantic Versioning](https://semver.org). The pull request title becomes the commit on `master`, and its type sets the next version:

| Title | Next version | When |
|---|---|---|
| `fix: …`, `perf: …`, `refactor: …`, `revert: …` | patch, 1.0.0 → 1.0.1 | a correction, or a change that undoes one; plugins keep working |
| `feat: …` | minor, 1.0.0 → 1.1.0 | a new ability; plugins keep working |
| `feat!: …`, `fix!: …` | major, 1.0.0 → 2.0.0 | the plugin API changed so that a plugin written against it stops working |
| `docs: …`, `test: …`, `chore: …`, `ci: …`, `build: …` | no release | nothing a user of nerd runs changes |

Major means the plugin API changed without backward compatibility. The plugin API is the socket's messages, events and their fields, the manifest, and the environment a plugin is started with, as [plugins](docs/plugins.md) describes them. Its compatibility contract says they only grow, so a major version should never be needed; if one ever is, a rename or removal is it, as the old name stops working at once, with no alias. Everything else in nerd can change in a `feat` or a `fix`.

The pull request description becomes the rest of the commit, and release-please reads it too:

- A major change ends its description with a line of its own that starts with `BREAKING CHANGE: ` and says what a plugin's author has to do. A footer in your own commits is lost in the squash.
- Any other line that starts with `BREAKING CHANGE:`, `feat:` or `fix:` is read the same way, wherever it is, so put such words inside backticks.
- A pull request whose files are all under `docs/`, `.github/` or `.claude/` gives no release, whatever its title.
- Use `revert: …` to undo a change: a title as GitHub's revert button writes it, `Revert "…"`, is not understood and gives no release and no changelog line.

## Licence

By contributing you agree that your contribution is licensed under the [MIT License](LICENSE).

## Conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
