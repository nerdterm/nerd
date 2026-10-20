#!/bin/sh
# Run from a plain terminal after merging the release pull request: gh needs the keychain.
set -eu
tag=${1:?usage: scripts/publish-binary.sh vX.Y.Z}
cd "$(git rev-parse --show-toplevel)"
[ -z "$(git status --porcelain)" ] || { echo "publish: the tree must be clean" >&2; exit 1; }
git fetch -q --tags origin
# A draft release has no git tag until it is published: build its target commit.
ref=$tag
if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    ref=$(gh release view "$tag" --json targetCommitish -q .targetCommitish)
    git fetch -q origin "$ref"
fi
# Under target/, not the temp folders: the isolation tests need a checkout a This-folder-only pane cannot write.
mkdir -p target
work=$(mktemp -d "$PWD/target/publish.XXXXXX")
trap 'rm -rf "$work"' EXIT
git worktree add -q --detach "$work/src" "$ref"
fresh=$(mktemp -d)
# Resolved before HOME changes: /bin/sh expands later prefix assignments with the new HOME.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
(cd "$work/src" \
    && cargo build --release --locked --target-dir "$work/target" \
    && HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" \
       cargo test --release --locked --target-dir "$work/target" >"$work/tests.log" \
    || { sed -n '/^failures:/,$p' "$work/tests.log" >&2; exit 1; })
rm -rf "$fresh"
bin="$work/target/release/nerd"
codesign --force --sign - "$bin"
name="nerd-$tag-macos-arm64.tar.gz"
tar -czf "$work/$name" -C "$work/target/release" nerd
(cd "$work" && shasum -a 256 "$name" > "$name.sha256")
gh release upload "$tag" "$work/$name" "$work/$name.sha256" --clobber
# The bot leaves the release as a draft; this asset is what publishes it.
gh release edit "$tag" --draft=false
git worktree remove --force "$work/src"
echo "attached $name to $tag and published it"
# nerdterm.dev is built from the latest release: rebuild it now rather than at the next daily run.
gh workflow run pages --repo nerdterm/nerd-plugins
echo "started the nerdterm.dev build"
sh scripts/publish-formula.sh "$tag"
