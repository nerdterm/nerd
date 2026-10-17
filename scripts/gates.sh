#!/bin/sh
# Tests run under an empty HOME so the gate can't pass only because nerd's
# directories happen to exist here; toolchain paths are kept so cargo still resolves.
set -eu
# A hook in a linked worktree exports an absolute GIT_DIR; tests' temp repos would write into this one.
unset $(git rev-parse --local-env-vars)
cd "$(git rev-parse --show-toplevel)"
sh scripts/privacy.sh tree
sh scripts/privacy.test.sh
# Cargo never deletes old builds: a target/ another toolchain built (they share nothing) or past 4 GB starts over.
if [ -f target/.rustc_info.json ] && { ! grep -qF -- "$(rustc -V)" target/.rustc_info.json || [ "$(du -sk target | cut -f1)" -gt 4194304 ]; }; then
    cargo clean
fi
fresh=$(mktemp -d)
trap 'rm -rf "$fresh"' EXIT
cargo build --release
# The core rule: the server's sandbox must deny the network on every change.
target/release/nerd --sandbox-selftest
# Resolved before HOME changes: /bin/sh expands later prefix assignments with the new HOME.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
HOME="$fresh" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" cargo test --release
cargo clippy --release --all-targets -- -D warnings
# cargo-deny may be absent on the release runner; the pr workflow covers it.
if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check licenses bans sources
else
    echo "gates: cargo-deny not installed here, the pr check covers it" >&2
fi
