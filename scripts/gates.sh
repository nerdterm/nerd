#!/bin/sh
# Tests run under an empty HOME so the gate can't pass only because nerd's
# directories happen to exist here; toolchain paths are kept so cargo still resolves.
set -eu
# A hook in a linked worktree exports an absolute GIT_DIR; tests' temp repos would write into this one.
unset $(git rev-parse --local-env-vars)
cd "$(git rev-parse --show-toplevel)"
sh scripts/privacy.sh tree
sh scripts/privacy.test.sh
sh scripts/terminals.test.sh
sh scripts/publish-formula.test.sh
sh scripts/install.test.sh
sh scripts/sign-release.test.sh
sh scripts/publish-binary.test.sh
# Cargo never deletes old builds: a target/ another toolchain built (they share nothing) or past 4 GB starts over.
if [ -f target/.rustc_info.json ] && { ! grep -qF -- "$(rustc -V)" target/.rustc_info.json || [ "$(du -sk target | cut -f1)" -gt 4194304 ]; }; then
    cargo clean
fi
# The build and the tests write programs to their temp folder and run them, so theirs is under ~/.local/bin, which no isolated pane can write.
# The tests' HOME is inside it: a test build takes HOME only from its temp folder (paths::home).
# Daemon tests' socket paths fit macOS's 104 bytes only while it is no longer than the system's; a longer one uses the system's.
# ceiling: inside a pane, another isolated pane reaches this run through the shared temp folder; a per-pane temp folder is the way past.
# ceiling: tests naming /tmp directly (tests/mailcli.rs, plugins_git.rs, pane_exec.rs, daemon.rs) stay there; a short exclusive folder under temp_dir() in them is the way past.
make_work() {
    # Unset, the system's temp folder is /tmp.
    sys=${TMPDIR:-/tmp}
    sys=${sys%/}
    if work=$(mkdir -p "$HOME/.local/bin" && mktemp -d "$HOME/.local/bin/.ngates.XXXXXX"); then
        work=$(cd "$work" && pwd -P)
        if [ "${#work}" -le "${#sys}" ]; then
            tmp=$work
            return
        fi
        echo "gates: $work is longer than the system temp folder $sys, so socket paths in the tests would not fit; running in $sys" >&2
        rm -rf "$work"
    elif [ -z "${NERD_PANE:-}" ]; then
        echo "gates: cannot make a work folder under $HOME/.local/bin" >&2
        exit 1
    else
        echo "gates: cannot make a work folder under $HOME/.local/bin; running in the system temp folder $sys" >&2
    fi
    work=$(mktemp -d "$sys/ngates.XXXXXX")
    tmp=$sys
}
work=
tmp=
make_work
trap 'rm -rf "$work"' EXIT
fresh="$work/h"
mkdir -m 700 "$fresh"
TMPDIR="$tmp" cargo build --release
# The core rule: the server's sandbox must deny the network on every change.
target/release/nerd --sandbox-selftest
# Resolved before HOME changes: /bin/sh expands later prefix assignments with the new HOME.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
# Copied from publish-binary.sh: these four take the temp folder to be the system's and run no program from it; another like them fails the main run.
sys_tests="agents::tests::a_plugins_executable_half_is_checked_before_it_can_run agents::tests::a_plugins_profile_denies_by_default_and_names_what_it_allows
    agents::tests::what_a_plugin_can_do_comes_from_its_manifest_and_only_new_kinds_are_new
    isolation::tests::folder_only_keeps_agents_settings_and_what_runs_later_even_when_the_folder_is_home"
skips=$(for t in $sys_tests; do printf ' --skip %s' "$t"; done)
TMPDIR="$tmp" HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" cargo test --release -- --exact $skips
# A renamed one would pass with fewer tests, so the count is checked.
cargo test --release --bin nerd -- --exact $sys_tests >"$work/sys.log" 2>&1 || { cat "$work/sys.log" >&2; exit 1; }
cat "$work/sys.log"
grep -q 'test result: ok. 4 passed' "$work/sys.log" || { echo "gates: the four system-temp tests did not all pass" >&2; exit 1; }
# The release check's stand-in for curl exists in debug builds only.
TMPDIR="$tmp" HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" cargo test --test release
TMPDIR="$tmp" cargo clippy --release --all-targets -- -D warnings
# cargo-deny may be absent on the release runner; the pr workflow covers it.
if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check licenses bans sources
else
    echo "gates: cargo-deny not installed here, the pr check covers it" >&2
fi
