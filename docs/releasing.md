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

5. From a plain terminal (gh needs the keychain), in a clean checkout, build, test and attach the binary, which
   publishes the release. It takes several minutes and ends with `attached nerd-vX.Y.Z-macos-arm64.tar.gz to vX.Y.Z
   and published it`:

   ```sh
   scripts/publish-binary.sh vX.Y.Z
   ```

6. Check it: `false`, then the archive and its `.sha256`:

   ```sh
   gh release view vX.Y.Z --json isDraft,assets --jq '.isDraft, .assets[].name'
   ```

`release not found` in step 5 means step 3 or 4 is not done yet. No macOS runners in CI: the binary is built only on
the maintainer's Mac, at the tag.
