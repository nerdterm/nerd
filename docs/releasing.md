# Releasing

The maintainer runs a release; nothing here is done by an agent. Each push to `master` makes release-please keep one open
pull request, `chore(master): release X.Y.Z`. Replace `N` and `X.Y.Z` below with its number and version.

1. Find the release pull request:

   ```sh
   gh pr list --state open --search "chore(master): release"
   ```

2. Approve its `pr` check. It waits with `action_required`, because release-please opened the pull request. Open the
   pull request, and under its checks press the button that approves and runs the waiting workflow:

   ```sh
   gh pr view N --web
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

6. Check it: `false`, then the archive, its `.sha256` and its `.sha256.sig`:

   ```sh
   gh release view vX.Y.Z --json isDraft,assets --jq '.isDraft, .assets[].name'
   ```

If the formula step failed, run it alone: it reads the published checksum and checks its signature against this
checkout's install.sh, so running it again is safe.
`scripts/publish-formula.sh --dry-run vX.Y.Z` prints the formula without writing it.

```sh
scripts/publish-formula.sh vX.Y.Z
```

`release not found` in step 5 means step 3 or 4 is not done yet. No macOS runners in CI: the binary is built only on
the maintainer's Mac, at the tag.

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
