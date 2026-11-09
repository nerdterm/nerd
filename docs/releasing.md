# Releasing

The maintainer runs a release; nothing here is done by an agent. Each push to `master` makes release-please keep one open
pull request, `chore(master): release X.Y.Z`. Replace `N` and `X.Y.Z` below with its number and version.

1. Find the release pull request:

   ```sh
   gh pr list --state open --search "chore(master): release"
   ```

2. Approve its `pr` check. It waits with `action_required`, because release-please opened the pull request. Approve
   and run the newest waiting run from the terminal (the same call as the "Approve and run" button on the pull
   request's page):

   ```sh
   run=$(gh run list --workflow pr --branch release-please--branches--master--components--nerd --limit 1 \
     --json databaseId,conclusion --jq '.[] | select(.conclusion == "action_required") | .databaseId')
   gh api -X POST "repos/nerdterm/nerd/actions/runs/${run:?no pr run waits for approval}/approve"
   ```

   Then wait until it is green:

   ```sh
   gh pr checks N --watch
   ```

3. Merge it. This creates the tag `vX.Y.Z` and a draft release:

   ```sh
   gh pr merge N --squash
   ```

4. Wait for the draft release:

   ```sh
   until gh release view vX.Y.Z >/dev/null 2>&1; do sleep 5; done; echo ready
   ```

5. From a plain terminal (gh needs the keychain), in a clean checkout, build, test, sign and attach the binary, which
   publishes the release, starts the nerdterm.dev build and points the Homebrew formula in nerdterm/homebrew-tap at
   the new archive. It asks for the release key's passphrase once, installs the signed archive with the tag's own
   install.sh into a scratch folder before uploading anything, and stops if that refuses it. It takes several minutes
   and ends with `attached nerd-vX.Y.Z-macos-arm64.tar.gz to vX.Y.Z and published it`, `started the nerdterm.dev
   build` and `updated the Homebrew formula to X.Y.Z`:

   ```sh
   scripts/publish-binary.sh vX.Y.Z
   ```

   It stops before building if the draft's target is not a commit id ("not a commit id"), the tag's commit is not
   on master ("is not on master"), or it is not the release commit itself, which changes only the version and adds
   to the changelog ("is not a release commit", "changes more than nerd's version", "removes lines"); and after
   building, before anything is signed, if the built nerd reports another version ("the built nerd says"). Any of
   these means someone moved the tag or changed the release pull request: stop and look at who did, never work around
   it. Other stops are ordinary: a failing test (it prints cargo test's `failures:`), "the tree must be clean", and
   network errors from git or gh; fix the cause and run it again.

   Building and testing run every dependency's build code as you. Before building, the script reads the release
   commit's `scripts/sign-release.sh`, `scripts/publish-formula.sh` and `install.sh` from git and later runs that text,
   not the files on disk; after the build it runs only the system's own commands and the nerd it built until the key
   has been used. A build that rewrites a script or puts a command of its own on your PATH cannot run in place of these
   steps. What it cannot stop: build code that keeps running can read the terminal while you type the passphrase, use
   your gh login without a prompt (upload to the draft, write the tap), change the binary being built, or wait for a
   later release. Only building releases as a separate macOS user closes that; if a run looks wrong, see "If a release
   run looks wrong".

6. Check it: `false`, then the archive, its `.sha256` and its `.sha256.sig`:

   ```sh
   gh release view vX.Y.Z --json isDraft,assets --jq '.isDraft, .assets[].name'
   ```

7. Upgrade a Homebrew nerd on your Mac from the release window with **upgrade with Homebrew**, and check that it
   ends with `nerd X.Y.Z installed with Homebrew`.

A change to the formula template that an older nerd's rendering would install wrongly must come with a change to the
archive that the member check in the older nerd refuses (an archive holding more than `nerd`).

If the formula step failed, run it alone: it reads the published checksum and checks its signature against this
checkout's install.sh, so running it again is safe once `git status --porcelain` prints nothing.
`scripts/publish-formula.sh --dry-run vX.Y.Z` prints the formula without writing it.

```sh
scripts/publish-formula.sh vX.Y.Z
```

`release not found` in step 5 means step 3 or 4 is not done yet. No macOS runners in CI: the binary is built only on
the maintainer's Mac, at the tag.

## If a release run looks wrong

Step 5 asks for the passphrase once. If it asks a second time, if the release gets an asset or nerdterm/homebrew-tap a
commit that the run did not report, or if it stops anywhere step 5 does not name, take it that code from the build is
still running as you:

- Do not type the passphrase, and do not run step 5 again.
- If the passphrase was typed, treat the key as leaked: move to the backup key as "If the main key leaks or is lost"
  below says.
- Clean up from github.com on another device, or on this Mac only after what was left behind is removed and the Mac
  restarted: a new `gh auth login` before that hands the new token to whatever is still running.
- Revoke GitHub CLI's token on github.com (Settings, Applications).
- Look for what was left behind: `~/.zshrc`, `~/.zprofile`, `~/Library/LaunchAgents`, `~/.cargo/bin`, `~/.local/bin`,
  `/opt/homebrew/bin`, the checkout's `.git/config` and `.git/hooks`, and `~/.gitconfig` (a `core.fsmonitor` there runs
  on the next `git status`).
- Put the release back to draft, then delete its assets: on github.com, or with `gh release edit vX.Y.Z --draft=true`
  and `gh release delete-asset vX.Y.Z <name>` for each name step 6 lists.
- In nerdterm/homebrew-tap, revert `Formula/nerd.rb` to the last commit the release script made.

## Dependency advisories

`.github/workflows/advisories.yml` runs `cargo deny check advisories` every Monday and on demand
(`gh workflow run advisories.yml`); it is never part of `pr`. A red scheduled run emails the account that last changed
its cron line. Update the dependency named; a notice with no fix (an unmaintained crate) gets an `ignore` entry with its
reason in `deny.toml`.

## The release key

Releases are signed with an ECDSA P-256 key kept off GitHub. install.sh carries the public halves of it and of a
backup key between `# release keys begin` and `# release keys end`. `scripts/install.test.sh` checks that the block
holds two P-256 keys. nerdterm/nerd-plugins pins the same two in `.github/nerd-release-keys.pem`, and its workflows
install a release only after checking its signature against them.

- **Making them**, once: generate a passphrase of at least 24 random characters in your password manager, then run
  `scripts/release-key.sh` from a plain terminal. It writes `~/.config/nerd-release/release.pem` and `backup.pem`,
  encrypted with that passphrase, and prints both public keys for the block.
- **Keeping them**: store `backup.pem` in the password manager with its passphrase and delete it from the Mac. Never
  sync `~/.config/nerd-release`; Time Machine copies only the encrypted file. `NERD_RELEASE_KEY` points
  `scripts/sign-release.sh` at another key file.
- **If the main key leaks or is lost**: take `backup.pem` from the password manager, run the next release with
  `NERD_RELEASE_KEY=<that file>`, and in the same release put a new pair from `scripts/release-key.sh` into install.sh
  and into nerdterm/nerd-plugins. Installed copies trust the old pair until they update to it.
- **If both are lost**: no update from nerd's menu installs again; everyone has to reinstall with `curl … | sh` or
  Homebrew once a release carries new keys.
