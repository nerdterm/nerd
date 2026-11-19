#!/bin/sh
# Runs the real publish-binary.sh from a checkout with stand-ins for gh, cargo and the signing and formula scripts:
# nothing is built, signed or sent anywhere. The global git config, which is the maintainer's and trusted, sends the
# public URL to a local repository and every other https or ssh address nowhere. Each refusal is checked by its message
# and by gh's calls.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
# In the checkout, not the per-user temp folder every isolated pane may write: the stand-ins here are run.
mkdir -p "$here/../target"
t=$(cd "$here/../target" && mktemp -d "$PWD/.nerd-test.XXXXXX")
trap 'rm -rf "$t"' EXIT
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }
o="$t/origin.git"
cat >"$t/gitconfig" <<EOF
[url "file://$o"]
    insteadOf = https://github.com/nerdterm/nerd.git
[url "file:///nonexistent/"]
    insteadOf = https://
    insteadOf = ssh://
    insteadOf = git@
EOF
export GIT_CONFIG_GLOBAL="$t/gitconfig" GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
unset GIT_TEMPLATE_DIR CARGO_HOME
# A gh that is not the stand-in must never be reached: no token, no config.
export GH_TOKEN= GH_CONFIG_DIR="$t/gh-config"
# The script works under ~/.local/bin: a HOME of its own.
export HOME="$t/home"

mkdir -p "$t/stub"
# Every git and gh call, with the folder it ran in: none may run in the checkout or name it.
cat >"$t/stub/git" <<EOF
#!/bin/sh
echo "\$PWD \$* \$(env | grep '^GIT_' | tr '\n' ' ')" >>"\$STUB/calls.log"
exec "$(command -v git)" "\$@"
EOF
cat >"$t/stub/gh" <<'EOF'
#!/bin/sh
echo "$PWD $*" >>"$STUB/calls.log"
echo "$*" >>"$STUB/gh.log"
case "$1 $2" in "release view") echo "$STUB_TARGET" ;; esac
EOF
cat >"$t/stub/cargo" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/cargo.log"
echo "$1 ${TMPDIR:-} $HOME" >>"$STUB/home.log"
[ "$1" = build ] || exit 0
# What it was asked to build, and the version stamp as build.rs makes it, from git in the folder it builds.
cat main.rs >"$STUB/built-main.rs"
git status --porcelain >/dev/null
stamp="$(git rev-parse --short HEAD), $(git log -1 --format=%cd --date=format:'%Y-%m-%d %H:%M')"
git describe --tags --abbrev=0 >"$STUB/described" 2>/dev/null || true
while [ $# -gt 0 ]; do [ "$1" = --target-dir ] && dir=$2; shift; done
mkdir -p "$dir/release"
printf '#!/bin/sh\necho "nerd %s (%s)"\n' "${STUB_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}" "$stamp" >"$dir/release/nerd"
chmod 755 "$dir/release/nerd"
"$dir/release/nerd" --version >"$STUB/version"
[ -n "${STUB_HIJACK:-}" ] || exit 0
# What a build script could do: append to every script run after it, in the checkout and in the source it builds, and
# plant commands in a folder on PATH.
for f in "$STUB_CHECKOUT/scripts/publish-binary.sh" "$STUB_CHECKOUT/scripts/sign-release.sh" "$STUB_CHECKOUT/scripts/publish-formula.sh" \
    "$STUB_CHECKOUT/install.sh" "$PWD/scripts/sign-release.sh" "$PWD/scripts/publish-formula.sh" "$PWD/install.sh" "$(cd .. && pwd)/pin/install"; do
    if [ -f "$f" ]; then printf 'echo %s >>"$STUB/hijacked"\n' "$f" >>"$f"; fi
done
for c in shasum dirname basename awk tar codesign rm mktemp; do
    printf '#!/bin/sh\necho %s >>"$STUB/planted"\nPATH=/usr/bin:/bin exec %s "$@"\n' "$c" "$c" >"$STUB/plant/$c"
    chmod 755 "$STUB/plant/$c"
done
EOF
chmod 755 "$t/stub/git" "$t/stub/gh" "$t/stub/cargo"
export STUB="$t" PATH="$t/stub:$PATH"
[ "$(command -v gh)" = "$t/stub/gh" ] || { echo "FAIL: gh is not the stand-in" >&2; exit 1; }

# origin: v0.1.0, its release commit to 0.2.0, a later master commit, one more release that also touches build.rs,
# and a release-shaped commit on a branch that never reached master.
git init -q --bare -b master "$o"
w="$t/make"
git clone -q "$o" "$w" 2>/dev/null
mkdir -p "$w/scripts"
cp "$here/publish-binary.sh" "$w/scripts/"
# The real signing script's commands, by name, so a planted one would show.
cat >"$w/scripts/sign-release.sh" <<'EOF'
#!/bin/sh
echo signed >>"$STUB/sign.log"
(cd "$(dirname "$1")" && shasum -a 256 "$(basename "$1")") >"$1.sha256"
touch "$1.sha256.sig"
sh "$2" "$1"
EOF
cat >"$w/scripts/publish-formula.sh" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/formula.log"
cat "${NERD_INSTALL_SH:-/dev/null}" >"$STUB/formula-install"
EOF
printf '#!/bin/sh\necho installed >>"$STUB/install.log"\n' >"$w/install.sh"
printf 'target/\n' >"$w/.gitignore"
version() { # files of release $1 with history $2
    printf '[package]\nname = "nerd"\nversion = "%s"\n' "$1" >"$w/Cargo.toml"
    printf '[[package]]\nname = "nerd"\nversion = "%s"\n' "$1" >"$w/Cargo.lock"
    printf '{".": "%s"}\n' "$1" >"$w/.release-please-manifest.json"
    { printf '## %s\n' "$1"; cat "$w/CHANGELOG.md" 2>/dev/null || true; } >"$t/changelog" && mv "$t/changelog" "$w/CHANGELOG.md"
}
commit() { git -C "$w" add -A && git -C "$w" commit -q -m "$1" && git -C "$w" rev-parse HEAD; }
version 0.1.0
echo src >"$w/main.rs"
first=$(commit parent)
version 0.2.0
release=$(commit 'release 0.2.0')
echo more >>"$w/main.rs"
later=$(commit 'a fix after the release')
version 0.3.0
echo 'fn main() {}' >"$w/build.rs"
touched=$(commit 'release 0.3.0, and build.rs')
# Release-shaped commits each hiding one more change, every one after a clean commit of the version before it.
version 0.4.0
printf '[[package]]\nname = "evil"\nversion = "1.0.0"\n' >>"$w/Cargo.lock"
lock=$(commit 'release 0.4.0, and a package in Cargo.lock')
version 0.4.0
commit 'clean 0.4.0' >/dev/null
version 0.5.0
printf '++ injected\n' >>"$w/Cargo.toml"
toml=$(commit 'release 0.5.0, and a line in Cargo.toml')
version 0.5.0
printf -- '-- hidden\n' >>"$w/CHANGELOG.md"
commit 'clean 0.5.0' >/dev/null
version 0.6.0
grep -v -e '^-- hidden$' "$w/CHANGELOG.md" >"$t/changelog" && mv "$t/changelog" "$w/CHANGELOG.md"
log=$(commit 'release 0.6.0, and a changelog line gone')
git -C "$w" push -q origin master
git -C "$o" tag v0.1.0 "$first"
git -C "$w" switch -q -c side "$release^"
version 0.2.0
side=$(commit 'release-shaped, never on master')
git -C "$w" push -q origin side
# Another repository with its own clean release 0.2.0 of another source, for a checkout whose origin points there.
git -C "$w" switch -q -c decoy "$release^"
echo decoy >"$w/main.rs"
commit 'another source' >/dev/null
version 0.2.0
decoy=$(commit 'release 0.2.0')
git init -q --bare -b master "$t/decoy.git"
git -C "$w" push -q "$t/decoy.git" decoy:master
git -C "$t/decoy.git" tag v0.2.0 "$decoy"

# publish TAG: the real script runs from a fresh checkout; the tag, if any, points where $at says.
runs=0
publish() {
    rm -rf "$t/plant" && mkdir "$t/plant"
    for f in ran built-main.rs gh.log cargo.log home.log sign.log formula.log formula-install hijacked planted calls.log version described; do
        rm -f "$t/$f"
    done
    git -C "$o" tag -d "$1" >/dev/null 2>&1 || true
    [ -z "${at:-}" ] || git -C "$o" tag "$1" "$at"
    runs=$((runs + 1))
    c="$t/checkout-$runs"
    git clone -q "$o" "$c"
    [ -z "${STUB_PLANT:-}" ] || plant "$c" "$STUB_PLANT"
    if [ -n "${STUB_ABOVE:-}" ]; then mkdir -p "$(dirname "$HOME/$STUB_ABOVE")" && echo '[workspace]' >"$HOME/$STUB_ABOVE"; fi
    rm -f "$t/calls.log"
    (cd "$c" && export STUB_CHECKOUT="$c" && if [ -n "${STUB_TEMPLATE:-}" ]; then export GIT_TEMPLATE_DIR="$t/templates"; fi \
        && if [ -n "${STUB_DIFF:-}" ]; then export GIT_CONFIG_GLOBAL="$t/gitconfig-diff"; fi \
        && if [ -n "${STUB_GITENV:-}" ]; then
            # As a git hook exports them, every one naming the checkout.
            export GIT_DIR="$c/.git" GIT_WORK_TREE="$c" GIT_INDEX_FILE="$c/.git/index" GIT_OBJECT_DIRECTORY="$c/.git/objects" \
                GIT_ALTERNATE_OBJECT_DIRECTORIES="$c/.git/objects" GIT_COMMON_DIR="$c/.git" GIT_NAMESPACE="$c" \
                GIT_GRAFT_FILE="$c/.git/info/grafts" GIT_SHALLOW_FILE="$c/.git/shallow" GIT_REPLACE_REF_BASE="$c/refs/replace/"
        fi \
        && PATH="$t/plant:$PATH" sh scripts/publish-binary.sh "$1") >"$t/out" 2>&1 && rc=0 || rc=$?
    cat "$t/out"
    [ -z "${STUB_ABOVE:-}" ] || rm -rf "${HOME:?}/${STUB_ABOVE%%/*}"
    [ -z "$(ls -A "$HOME/.local/bin" 2>/dev/null)" ] || bad "$1: left $(ls -A "$HOME/.local/bin") in ~/.local/bin"
    ! grep -F "$c" "$t/calls.log" >/dev/null 2>&1 || bad "$1: ran git or gh in the checkout or on it: $(grep -F "$c" "$t/calls.log")"
    # A prefix assignment before a function call outlives it in sh: each case starts clean.
    unset at STUB_VERSION STUB_TARGET STUB_HIJACK STUB_PLANT STUB_TEMPLATE STUB_DIFF STUB_GITENV STUB_ABOVE
    return "$rc"
}
# What a pane whose folder is the checkout could leave in its .git: each program it names records that it ran.
cat >"$t/evil" <<'EOF'
#!/bin/sh
echo "$1" >>"$STUB/ran"
case $1 in
    hook) echo planted >>main.rs ;;
    textconv) cat "$2" ;;
    textconv-x) sed 's/^/x/' "$2" ;;
    smudge) sed 's/^src$/planted/' ;;
    fsmonitor | uploadpack | helper | proxy) exit 1 ;;
    askpass | credential | alternates) exit 0 ;;
    *) cat ;;
esac
EOF
mkdir -p "$t/hooks" "$t/templates/hooks"
for h in post-checkout reference-transaction; do
    printf '#!/bin/sh\nexec "%s" hook\n' "$t/evil" | tee "$t/hooks/$h" >"$t/templates/hooks/$h"
done
chmod 755 "$t/evil" "$t/hooks/"* "$t/templates/hooks/"*
plant() { # checkout, what
    case $2 in
        hooks) cp "$t/hooks/"* "$1/.git/hooks/" ;;
        replace) git -C "$1" update-ref "refs/replace/$(git -C "$1" rev-parse "$release:main.rs")" "$(echo planted | git -C "$1" hash-object -w --stdin)" ;;
        # master's tip with the off-master commit as a second parent.
        grafts) printf '%s %s %s\n' "$(git -C "$1" rev-parse origin/master)" "$(git -C "$1" rev-parse origin/master^)" "$side" >"$1/.git/info/grafts" ;;
        config)
            printf '* filter=evil diff=evil text eol=crlf\n' >"$1/.git/info/attributes"
            echo "$o/objects" >"$1/.git/objects/info/alternates"
            for kv in "core.hooksPath=$t/hooks" "core.fsmonitor=$t/evil fsmonitor" "remote.origin.uploadpack=$t/evil uploadpack" \
                "remote.origin.fetch=+refs/heads/side:refs/remotes/origin/master" "remote.other.url=$o" "remote.other.uploadpack=$t/evil uploadpack" \
                "remotes.origin=other" "pager.fetch=$t/evil pager" "core.pager=$t/evil pager" "core.alternateRefsCommand=$t/evil alternates" \
                "core.gitProxy=$t/evil proxy" "core.askPass=$t/evil askpass" "credential.helper=$t/evil credential" \
                "filter.evil.smudge=$t/evil smudge" "filter.evil.clean=$t/evil clean" filter.evil.required=true "diff.evil.textconv=$t/evil textconv"; do
                git -C "$1" config "${kv%%=*}" "${kv#*=}"
            done
            ;;
        helper) git -C "$1" config remote.origin.url evil::x && git -C "$1" config alias.remote-evil "!$t/evil helper" ;;
        decoy) git -C "$1" config remote.origin.url "$t/decoy.git" ;;
    esac
}
uploaded() { grep -q '^release upload' "$t/gh.log" 2>/dev/null; }
expect_refused() { # case, message
    if uploaded; then bad "$1: uploaded"; fi
    grep -q "$2" "$t/out" || bad "$1: no \"$2\" in: $(cat "$t/out")"
}
built_src() { # case
    [ ! -f "$t/ran" ] || bad "$1: ran what the checkout's .git named: $(tr '\n' ' ' <"$t/ran")"
    [ "$(cat "$t/built-main.rs" 2>/dev/null)" = src ] || bad "$1: built another source: $(cat "$t/built-main.rs" 2>/dev/null)"
    uploaded || bad "$1: the release commit was not uploaded"
}

at=$release publish v0.2.0 >/dev/null || bad "(a) the release commit: $(cat "$t/out")"
built_src "(a)"
[ -f "$t/sign.log" ] || bad "(a) not signed"
grep -qF " fetch -q --no-tags https://github.com/nerdterm/nerd.git +refs/heads/master:" "$t/calls.log" || bad "(a) fetched from another address: $(grep fetch "$t/calls.log")"
# The stamp build.rs makes: the release commit's own hash and date.
grep -Eqx "nerd 0\.2\.0 \($(git -C "$o" rev-parse --short "$release"), [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}\)" "$t/version" \
    || bad "(a) the version stamp: $(cat "$t/version")"
p="$HOME/.local/bin/.nerd-publish.*/t"
build="build $p $HOME" tests="test $p $p/home"
# shellcheck disable=SC2254 # a glob on purpose
case $(sed -n 1p "$t/home.log") in $build) ;; *) bad "(a) the build's TMPDIR is not in the work folder: $(cat "$t/home.log")" ;; esac
# shellcheck disable=SC2254 # a glob on purpose
case $(sed -n 2p "$t/home.log") in $tests) ;; *) bad "(a) the tests' TMPDIR and HOME are not in the work folder: $(cat "$t/home.log")" ;; esac
case $(sed -n 3p "$t/home.log") in *.nerd-publish.*) bad "(a) the four tests that need the system's temp folder did not run in it: $(cat "$t/home.log")" ;; "test "*" $HOME") ;; *) bad "(a) the third cargo call: $(cat "$t/home.log")" ;; esac
[ "$(wc -l <"$t/home.log" | tr -d ' ')" = 3 ] || bad "(a) cargo ran $(wc -l <"$t/home.log") times"

at=$side publish v0.2.0 >/dev/null && bad "(b) published a commit off master"
expect_refused "(b) off master" "is not on master"

at=$later publish v0.2.0 >/dev/null && bad "(c) published a later master commit"
expect_refused "(c) moved to a later commit" "is not a release commit"

at=$touched publish v0.3.0 >/dev/null && bad "(d) published a release that touches build.rs"
expect_refused "(d) build.rs" "is not a release commit"

STUB_VERSION=0.1.9 at=$release publish v0.2.0 >/dev/null && bad "(e) published a binary of another version"
expect_refused "(e) another binary" "the built nerd says"
[ ! -f "$t/sign.log" ] || bad "(e) signed before the version check"

STUB_TARGET=$release at= publish v0.2.0 >/dev/null || bad "(f) the draft's release commit: $(cat "$t/out")"
uploaded || bad "(f) the draft's release commit was not uploaded"
[ "$(cat "$t/described" 2>/dev/null)" = v0.1.0 ] || bad "(f) build.rs did not find the last release: $(cat "$t/described" 2>/dev/null)"

STUB_TARGET=$side at= publish v0.2.0 >/dev/null && bad "(g) published a draft target off master"
expect_refused "(g) draft off master" "is not on master"

at=$release publish v0.5.0 >/dev/null && bad "(h) published a release commit under another version"
expect_refused "(h) another version" "changes more than nerd's version"
[ ! -f "$t/cargo.log" ] || bad "(h) built before the version check"

at=$lock publish v0.4.0 >/dev/null && bad "(i) published a release with another package in Cargo.lock"
expect_refused "(i) Cargo.lock" "Cargo.lock at v0.4.0 changes more than nerd's version"

at=$toml publish v0.5.0 >/dev/null && bad "(j) published a release with a line beginning ++ in Cargo.toml"
expect_refused "(j) Cargo.toml" "Cargo.toml at v0.5.0 changes more than nerd's version"

at=$log publish v0.6.0 >/dev/null && bad "(k) published a release whose changelog lost a line beginning --"
expect_refused "(k) CHANGELOG.md" "CHANGELOG.md at v0.6.0 removes lines"

STUB_TARGET=master at= publish v0.2.0 >/dev/null && bad "(l) published a draft whose target is a branch name"
expect_refused "(l) draft target" "targets master, not a commit id"

STUB_HIJACK=1 at=$release publish v0.2.0 >/dev/null || bad "(m) the release commit, with a build that rewrites scripts: $(cat "$t/out")"
uploaded || bad "(m) the release commit was not uploaded"
grep -q hijacked "$c/scripts/sign-release.sh" || bad "(m) the build never reached the checkout's scripts"
[ ! -f "$t/hijacked" ] || bad "(m) ran what the build appended to: $(tr '\n' ' ' <"$t/hijacked")"
[ ! -f "$t/planted" ] || bad "(m) ran commands the build planted: $(tr '\n' ' ' <"$t/planted")"
cmp -s "$t/formula-install" "$w/install.sh" || bad "(m) the formula did not read the release commit's install.sh"

# What the checkout's .git holds has no effect: hooks, a replace ref, config (a hook path, upload-pack, a fetch refspec, a
# remote group, a pager, filters, textconv, fsmonitor, alternates, a proxy, askpass, a credential helper) with attributes,
# an alias remote helper, an origin that points at another repository.
for p in "n hooks" "o replace" "p config" "q helper" "r decoy"; do
    STUB_PLANT=${p#* } at=$release publish v0.2.0 >/dev/null || bad "(${p%% *}) the release commit, with ${p#* } in the checkout's .git: $(cat "$t/out")"
    built_src "(${p%% *}) ${p#* }"
done

STUB_PLANT=grafts at=$side publish v0.2.0 >/dev/null && bad "(s) published a commit off master that the checkout's grafts put on it"
expect_refused "(s) grafts" "is not on master"

STUB_TEMPLATE=1 at=$release publish v0.2.0 >/dev/null || bad "(t) the release commit, with hooks in the git template folder: $(cat "$t/out")"
built_src "(t) template"

# The maintainer's own diff settings do not change what the checks count.
cat >"$t/gitconfig-diff" <<EOF
[include]
    path = $t/gitconfig
[color]
    ui = always
[diff]
    external = $t/evil external
[diff "x"]
    textconv = $t/evil textconv-x
[core]
    attributesFile = $t/attributes-diff
EOF
printf 'Cargo.toml diff=x\nCargo.lock diff=x\n' >"$t/attributes-diff"
STUB_DIFF=1 at=$release publish v0.2.0 >/dev/null || bad "(u) the release commit, with the maintainer's own diff settings: $(cat "$t/out")"
built_src "(u) diff settings"

STUB_GITENV=1 at=$release publish v0.2.0 >/dev/null || bad "(v) the release commit, with git variables naming the checkout: $(cat "$t/out")"
built_src "(v) git variables"

# What cargo and rustup read in a folder above the build, each kind in a folder of its own; the maintainer's is read.
for above in Cargo.toml rust-toolchain .local/rust-toolchain.toml .local/bin/.cargo/config .local/.cargo/config.toml .cargo/config; do
    STUB_ABOVE=$above at=$release publish v0.2.0 >/dev/null && bad "(w) built under $above"
    expect_refused "(w) $above" "$HOME/$above is above the build folder"
    [ ! -f "$t/cargo.log" ] || bad "(w) $above: ran cargo"
done
STUB_ABOVE=.cargo/config.toml at=$release publish v0.2.0 >/dev/null || bad "(x) the release commit, with CARGO_HOME's config.toml: $(cat "$t/out")"
built_src "(x) CARGO_HOME's config.toml"

[ "$fail" = 0 ] && echo "publish-binary: 29 cases pass"
exit "$fail"
