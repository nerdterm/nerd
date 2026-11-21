#!/bin/sh
# Run from a plain terminal after merging the release pull request: gh needs the keychain.
set -eu
# All of it in main, called by the last line: /bin/sh rereads a running script from disk, which the build can rewrite.
main() {
    tag=${1:?usage: scripts/publish-binary.sh vX.Y.Z}
    stop() { echo "publish: $*; stop and look at who changed it" >&2; exit 1; }
    # A git hook exports these: they would point every git here at another repository, index, object store or config.
    # Named, not asked of `git rev-parse --local-env-vars`: that would run git in the checkout with them still set.
    unset GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_OBJECT_DIRECTORY GIT_DIR GIT_WORK_TREE \
        GIT_IMPLICIT_WORK_TREE GIT_GRAFT_FILE GIT_INDEX_FILE GIT_NO_REPLACE_OBJECTS GIT_REPLACE_REF_BASE GIT_PREFIX GIT_SHALLOW_FILE \
        GIT_COMMON_DIR GIT_NAMESPACE
    for v in $(env | sed -En 's/^(GIT_CONFIG_(KEY|VALUE)_[0-9]+)=.*/\1/p'); do unset "$v"; done
    # Under ~/.local/bin, which no This-folder-only pane may write: one in the temp folders could change the build before it is signed.
    mkdir -p "$HOME/.local/bin"
    work=$(mktemp -d "$HOME/.local/bin/.nerd-publish.XXXXXX")
    trap 'rm -rf "$work"' EXIT
    # Its real path, which cargo and rustup walk up from: ~/.local or ~/.local/bin may be a link to another folder.
    work=$(cd "$work" && pwd -P)
    # Nothing reads the checkout's .git, which a pane may have written (docs/releasing.md): no git, gh or build runs in the
    # checkout, and every git call names the repository made here from the public URL. ~/.gitconfig is the maintainer's.
    cd "$work"
    src="$work/src"
    # No template: hooks in a folder that init.templateDir names would run at the checkout below.
    git init -q --template= "$src"
    g() { git --git-dir="$src/.git" --work-tree="$src" "$@"; }
    # Every tag, so build.rs finds the last release for the update window's list of commits.
    g fetch -q --no-tags https://github.com/nerdterm/nerd.git +refs/heads/master:refs/remotes/origin/master '+refs/tags/*:refs/tags/*'
    # A draft release has no git tag until it is published: build its target commit.
    ref=$tag
    if ! g rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        ref=$(gh release view "$tag" --repo nerdterm/nerd --json targetCommitish -q .targetCommitish)
        printf '%s' "$ref" | grep -Eqx '[0-9a-f]{40}' || stop "the draft $tag targets $ref, not a commit id"
    fi
    # Only the release commit itself is built and signed: on master, changing the version and the changelog and nothing
    # else. Whoever can move a tag, edit a draft or push to the release pull request's branch cannot choose the commit.
    want=${tag#v}
    # The maintainer's own diff settings must not change what is counted.
    d() { g diff --no-color --no-ext-diff --no-textconv --no-renames "$@"; }
    # Only master and the tags were fetched: a commit that is not here is not on master.
    sha=$(g rev-parse -q --verify "$ref^{commit}") || stop "$tag ($ref) is not on master"
    g merge-base --is-ancestor "$sha" refs/remotes/origin/master || stop "$tag ($sha) is not on master"
    files=$(d --name-only "$sha^" "$sha" | LC_ALL=C sort | tr '\n' ' ')
    [ "$files" = ".release-please-manifest.json CHANGELOG.md Cargo.lock Cargo.toml " ] || stop "$tag ($sha) is not a release commit: it changes $files"
    was=$(g cat-file blob "$sha^:Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1)
    # Counted, not read off the diff's text: a line of content may itself begin with "--" or "++".
    for f in Cargo.toml Cargo.lock; do
        [ "$(d --numstat "$sha^" "$sha" -- "$f")" = "$(printf '1\t1\t%s' "$f")" ] \
            && d -U0 "$sha^" "$sha" -- "$f" | grep -Fqx -- "-version = \"$was\"" \
            && d -U0 "$sha^" "$sha" -- "$f" | grep -Fqx -- "+version = \"$want\"" \
            || stop "$f at $tag changes more than nerd's version from $was to $want"
    done
    [ "$(d --numstat "$sha^" "$sha" -- CHANGELOG.md | cut -f2)" = 0 ] || stop "CHANGELOG.md at $tag removes lines"
    # Read from git before the build, which can rewrite any file of this user: what runs after it is the release's text.
    sign_src=$(g cat-file blob "$sha:scripts/sign-release.sh")
    formula_src=$(g cat-file blob "$sha:scripts/publish-formula.sh")
    install_src=$(g cat-file blob "$sha:install.sh")
    # A checkout, not an archive: build.rs stamps the version with the hash and date it reads from git there.
    g checkout -q --detach "$sha"
    # The build and the tests write programs to their temp folder and run them, so theirs is in the work folder, and the
    # tests' HOME inside it: a test build takes HOME only from inside its temp folder (paths::home). One letter: the daemon
    # tests' socket paths fit macOS's 104 bytes only while it is no longer than the system's temp folder.
    tmp="$work/t"
    fresh="$tmp/home"
    mkdir -m 700 "$tmp" "$fresh"
    # These four take the temp folder to be the system's and run no program from it; another like them stops the run. scripts/gates.sh keeps a copy of the list.
    sys="agents::tests::a_plugins_executable_half_is_checked_before_it_can_run agents::tests::a_plugins_profile_denies_by_default_and_names_what_it_allows
        agents::tests::what_a_plugin_can_do_comes_from_its_manifest_and_only_new_kinds_are_new
        isolation::tests::folder_only_keeps_agents_settings_and_what_runs_later_even_when_the_folder_is_home"
    skips=$(for s in $sys; do printf ' --skip %s' "$s"; done)
    # Resolved before HOME changes: /bin/sh expands later prefix assignments with the new HOME.
    cargo_home=${CARGO_HOME:-$HOME/.cargo}
    rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
    user_path=$PATH
    # cargo and rustup read these in every folder above the source: a workspace root that patches dependencies, a
    # toolchain that is any program, build config. By real paths up to HOME, as the folders above it are root's, or to /
    # when HOME is not above; CARGO_HOME's config.toml is yours.
    home=$(cd "$HOME" && pwd -P)
    trusted=$(cd "$cargo_home" 2>/dev/null && pwd -P || :)/config.toml
    dir=$work
    while :; do
        for f in Cargo.toml rust-toolchain rust-toolchain.toml .cargo/config .cargo/config.toml; do
            if [ -e "$dir/$f" ] || [ -L "$dir/$f" ]; then
                [ "$dir/$f" = "$trusted" ] || stop "$dir/$f is above the build folder, and cargo or rustup would read it"
            fi
        done
        { [ "$dir" = "$home" ] || [ -z "$dir" ]; } && break
        dir=${dir%/*}
    done
    (cd "$src" \
        && TMPDIR="$tmp" cargo build --release --locked --target-dir "$work/target" \
        && TMPDIR="$tmp" HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" \
           cargo test --release --locked --target-dir "$work/target" -- --exact $skips >"$work/tests.log" \
        && cargo test --release --locked --target-dir "$work/target" --bin nerd -- --exact $sys >>"$work/tests.log" \
        || { sed -n '/^failures:/,$p' "$work/tests.log" >&2; exit 1; })
    # From here on only the system's commands: the build may have planted others in a folder on the user's PATH.
    # gh and the formula get that PATH back, after the key.
    PATH=/usr/bin:/bin:/usr/sbin:/sbin
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
    echo "attached $name to $tag and published it"
    # nerdterm.dev is built from the latest release: rebuild it now rather than at the next daily run.
    PATH=$user_path gh workflow run pages --repo nerdterm/nerd-plugins
    echo "started the nerdterm.dev build"
    NERD_INSTALL_SH="$pin/install" PATH=$user_path /bin/sh -c "$formula_src" publish-formula "$tag"
}
main "$@"; exit
