#!/bin/sh
# Run from a plain terminal after merging the release pull request: gh needs the keychain.
set -eu
# All of it in main, called by the last line: /bin/sh rereads a running script from disk, which the build can rewrite.
main() {
    tag=${1:?usage: scripts/publish-binary.sh vX.Y.Z}
    cd "$(git rev-parse --show-toplevel)"
    [ -z "$(git status --porcelain)" ] || { echo "publish: the tree must be clean" >&2; exit 1; }
    git fetch -q --tags origin
    # A draft release has no git tag until it is published: build its target commit.
    stop() { echo "publish: $*; stop and look at who changed it" >&2; exit 1; }
    ref=$tag
    if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        ref=$(gh release view "$tag" --repo nerdterm/nerd --json targetCommitish -q .targetCommitish)
        printf '%s' "$ref" | grep -Eqx '[0-9a-f]{40}' || stop "the draft $tag targets $ref, not a commit id"
        git fetch -q origin "$ref"
    fi
    # Only the release commit itself is built and signed: on master, changing the version and the changelog and nothing
    # else. Whoever can move a tag, edit a draft or push to the release pull request's branch cannot choose the commit.
    git fetch -q origin master
    want=${tag#v}
    d() { git diff --no-color --no-ext-diff --no-renames "$@"; }
    sha=$(git rev-parse -q --verify "$ref^{commit}") || stop "$ref is not a commit"
    git merge-base --is-ancestor "$sha" origin/master || stop "$tag ($sha) is not on master"
    files=$(d --name-only "$sha^" "$sha" | LC_ALL=C sort | tr '\n' ' ')
    [ "$files" = ".release-please-manifest.json CHANGELOG.md Cargo.lock Cargo.toml " ] || stop "$tag ($sha) is not a release commit: it changes $files"
    was=$(git show "$sha^:Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1)
    # Counted, not read off the diff's text: a line of content may itself begin with "--" or "++".
    for f in Cargo.toml Cargo.lock; do
        [ "$(d --numstat "$sha^" "$sha" -- "$f")" = "$(printf '1\t1\t%s' "$f")" ] \
            && d -U0 "$sha^" "$sha" -- "$f" | grep -Fqx -- "-version = \"$was\"" \
            && d -U0 "$sha^" "$sha" -- "$f" | grep -Fqx -- "+version = \"$want\"" \
            || stop "$f at $tag changes more than nerd's version from $was to $want"
    done
    [ "$(d --numstat "$sha^" "$sha" -- CHANGELOG.md | cut -f2)" = 0 ] || stop "CHANGELOG.md at $tag removes lines"
    # Under target/, not the temp folders: the isolation tests need a checkout a This-folder-only pane cannot write.
    mkdir -p target
    work=$(mktemp -d "$PWD/target/publish.XXXXXX")
    trap 'rm -rf "$work"' EXIT
    # Read from git before the build, which can rewrite any file of this user: what runs after it is the release's text.
    sign_src=$(git cat-file blob "$sha:scripts/sign-release.sh")
    formula_src=$(git cat-file blob "$sha:scripts/publish-formula.sh")
    install_src=$(git cat-file blob "$sha:install.sh")
    git worktree add -q --detach "$work/src" "$sha"
    fresh=$(mktemp -d)
    # Resolved before HOME changes: /bin/sh expands later prefix assignments with the new HOME.
    cargo_home=${CARGO_HOME:-$HOME/.cargo}
    rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
    user_path=$PATH
    (cd "$work/src" \
        && cargo build --release --locked --target-dir "$work/target" \
        && HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" \
           cargo test --release --locked --target-dir "$work/target" >"$work/tests.log" \
        || { sed -n '/^failures:/,$p' "$work/tests.log" >&2; exit 1; })
    # From here on only the system's commands: the build may have planted others in a folder on the user's PATH.
    # gh and the formula get that PATH back, after the key.
    PATH=/usr/bin:/bin:/usr/sbin:/sbin
    rm -rf "$fresh"
    # Made only now, so the build never saw it; mkdir fails if anything made it first.
    pin="$work/pin"
    mkdir "$pin"
    printf '%s\n' "$install_src" >"$pin/install"
    bin="$work/target/release/nerd"
    # Before the key is asked for: what was built is the version being published.
    [ "$("$bin" --version | awk '{ print $2 }')" = "$want" ] || stop "the built nerd says $("$bin" --version), not $want"
    codesign --force --sign - "$bin"
    name="nerd-$tag-macos-arm64.tar.gz"
    # No AppleDouble ._nerd beside it: nerd's Homebrew upgrade refuses any member but nerd.
    COPYFILE_DISABLE=1 tar -czf "$work/$name" -C "$work/target/release" nerd
    # Signed with the release key, then installed by this tag's own install.sh into a scratch folder: a release its users would refuse never leaves.
    /bin/sh -c "$sign_src" sign-release "$work/$name" "$pin/install"
    PATH=$user_path gh release upload "$tag" --repo nerdterm/nerd "$work/$name" "$work/$name.sha256" "$work/$name.sha256.sig" --clobber
    # The bot leaves the release as a draft; this asset is what publishes it.
    PATH=$user_path gh release edit "$tag" --repo nerdterm/nerd --draft=false
    git worktree remove --force "$work/src"
    echo "attached $name to $tag and published it"
    # nerdterm.dev is built from the latest release: rebuild it now rather than at the next daily run.
    PATH=$user_path gh workflow run pages --repo nerdterm/nerd-plugins
    echo "started the nerdterm.dev build"
    NERD_INSTALL_SH="$pin/install" PATH=$user_path /bin/sh -c "$formula_src" publish-formula "$tag"
}
main "$@"; exit
