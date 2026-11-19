#!/bin/sh
# Runs the real publish-binary.sh in a scratch repository with stand-ins for gh, cargo and the signing and formula
# scripts: nothing is built, signed or sent anywhere. Each refusal is checked by its message and by gh's calls.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
# A gh that is not the stand-in must never be reached: no token, no config.
export GH_TOKEN= GH_CONFIG_DIR="$t/gh-config"
# The script works under ~/.local/bin: a HOME of its own.
export HOME="$t/home"

mkdir -p "$t/stub"
cat >"$t/stub/gh" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/gh.log"
case "$1 $2" in "release view") echo "$STUB_TARGET" ;; esac
EOF
cat >"$t/stub/cargo" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/cargo.log"
[ "$1" != test ] || echo "${TMPDIR:-} $HOME" >>"$STUB/home.log"
[ "$1" = build ] || exit 0
# As build.rs does, and what it was asked to build.
git status --porcelain >/dev/null && git log -1 --format=%cd >/dev/null
cat main.rs >"$STUB/built-main.rs"
while [ $# -gt 0 ]; do [ "$1" = --target-dir ] && dir=$2; shift; done
mkdir -p "$dir/release"
printf '#!/bin/sh\necho "nerd %s (test)"\n' "${STUB_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}" >"$dir/release/nerd"
chmod 755 "$dir/release/nerd"
[ -n "${STUB_HIJACK:-}" ] || exit 0
# What a build script could do: append to every script run after it (in the clone this worktree belongs to) and plant
# commands in a folder on PATH.
top=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
for f in "$top/scripts/publish-binary.sh" "$top/scripts/sign-release.sh" "$top/scripts/publish-formula.sh" "$top/install.sh" "$PWD/install.sh" "$(cd .. && pwd)/pin/install"; do
    if [ -f "$f" ]; then printf 'echo %s >>"$STUB/hijacked"\n' "$f" >>"$f"; fi
done
for c in shasum dirname basename awk tar codesign rm mktemp; do
    printf '#!/bin/sh\necho %s >>"$STUB/planted"\nPATH=/usr/bin:/bin exec %s "$@"\n' "$c" "$c" >"$STUB/plant/$c"
    chmod 755 "$STUB/plant/$c"
done
EOF
chmod 755 "$t/stub/gh" "$t/stub/cargo"
export STUB="$t" PATH="$t/stub:$PATH"
[ "$(command -v gh)" = "$t/stub/gh" ] || { echo "FAIL: gh is not the stand-in" >&2; exit 1; }

# origin: a parent, its release commit to 0.2.0, a later master commit, one more release that also touches build.rs,
# and a release-shaped commit on a branch that never reached master.
o="$t/origin.git"
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
commit parent >/dev/null
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
git -C "$w" switch -q -c side "$release^"
version 0.2.0
side=$(commit 'release-shaped, never on master')
git -C "$w" push -q origin side

# publish TAG: a fresh clone runs the real script; the tag, if any, points where $at says.
publish() {
    rm -rf "$t/ran" "$t/built-main.rs" "$t/gh.log" "$t/cargo.log" "$t/home.log" "$t/sign.log" "$t/formula.log" "$t/formula-install" "$t/hijacked" "$t/planted" "$t/plant"
    mkdir "$t/plant"
    git -C "$o" tag -d "$1" >/dev/null 2>&1 || true
    [ -z "${at:-}" ] || git -C "$o" tag "$1" "$at"
    c="$t/clone-$1-$(date +%s%N)"
    git clone -q "$o" "$c"
    [ -z "${STUB_PLANT:-}" ] || plant "$c" "$STUB_PLANT"
    (cd "$c" && PATH="$t/plant:$PATH" sh scripts/publish-binary.sh "$1") >"$t/out" 2>&1 && rc=0 || rc=$?
    cat "$t/out"
    [ -z "$(ls -A "$HOME/.local/bin" 2>/dev/null)" ] || bad "$1: left $(ls -A "$HOME/.local/bin") in ~/.local/bin"
    [ "$(git -C "$c" worktree list | wc -l | tr -d ' ')" = 1 ] || bad "$1: left a worktree entry: $(git -C "$c" worktree list)"
    # A prefix assignment before a function call outlives it in sh: each case starts clean.
    unset at STUB_VERSION STUB_TARGET STUB_HIJACK STUB_PLANT
    return "$rc"
}
# What a pane whose folder is the checkout can leave in its .git: each program it names records that it ran.
cat >"$t/evil" <<'EOF'
#!/bin/sh
echo "$1" >>"$STUB/ran"
case $1 in hook) echo planted >>main.rs ;; textconv) cat "$2" ;; fsmonitor) exit 1 ;; smudge) sed 's/^src$/planted/' ;; *) cat ;; esac
EOF
chmod 755 "$t/evil"
plant() { # clone, what
    case $2 in
        hook)
            printf '#!/bin/sh\nexec "%s" hook\n' "$t/evil" >"$1/.git/hooks/post-checkout"
            chmod 755 "$1/.git/hooks/post-checkout"
            ;;
        replace) git -C "$1" update-ref "refs/replace/$(git -C "$1" rev-parse "$release:main.rs")" "$(echo planted | git -C "$1" hash-object -w --stdin)" ;;
        config)
            printf '* filter=evil diff=evil\n' >"$1/.git/info/attributes"
            for kv in "core.fsmonitor=$t/evil fsmonitor" "filter.evil.smudge=$t/evil smudge" "filter.evil.clean=$t/evil clean" \
                filter.evil.required=true "diff.evil.textconv=$t/evil textconv"; do
                git -C "$1" config "${kv%%=*}" "${kv#*=}"
            done
            ;;
    esac
}
uploaded() { grep -q '^release upload' "$t/gh.log" 2>/dev/null; }
expect_refused() { # case, message
    if uploaded; then bad "$1: uploaded"; fi
    grep -q "$2" "$t/out" || bad "$1: no \"$2\" in: $(cat "$t/out")"
}

at=$release publish v0.2.0 >/dev/null || bad "(a) the release commit: $(cat "$t/out")"
uploaded || bad "(a) the release commit was not uploaded"
[ -f "$t/sign.log" ] || bad "(a) not signed"
case $(head -n 1 "$t/home.log" 2>/dev/null) in "$HOME"/.local/bin/.nerd-publish.*/t\ "$HOME"/.local/bin/.nerd-publish.*/t/home) ;; *) bad "(a) the tests' TMPDIR and HOME are not in the work folder: $(cat "$t/home.log" 2>/dev/null)" ;; esac
[ "$(wc -l <"$t/home.log" | tr -d ' ')" = 2 ] && ! sed -n 2p "$t/home.log" | grep -q '^[^ ]*/\.nerd-publish\.' || bad "(a) the four tests that need the system's temp folder did not run in it: $(cat "$t/home.log")"

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
grep -q hijacked "$c/scripts/sign-release.sh" || bad "(m) the build never reached the clone's scripts"
[ ! -f "$t/hijacked" ] || bad "(m) ran what the build appended to: $(tr '\n' ' ' <"$t/hijacked")"
[ ! -f "$t/planted" ] || bad "(m) ran commands the build planted: $(tr '\n' ' ' <"$t/planted")"
cmp -s "$t/formula-install" "$w/install.sh" || bad "(m) the formula did not read the release commit's install.sh"

for p in "n hook" "o replace" "p config"; do
    STUB_PLANT=${p#* } at=$release publish v0.2.0 >/dev/null || bad "(${p%% *}) the release commit, with a ${p#* } a pane left in .git: $(cat "$t/out")"
    [ ! -f "$t/ran" ] || bad "(${p%% *}) ran what .git named: $(tr '\n' ' ' <"$t/ran")"
    [ "$(cat "$t/built-main.rs" 2>/dev/null)" = src ] || bad "(${p%% *}) built another source: $(cat "$t/built-main.rs" 2>/dev/null)"
done

[ "$fail" = 0 ] && echo "publish-binary: 16 cases pass"
exit "$fail"
