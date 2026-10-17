#!/usr/bin/env bash
# Installs nothing and prints the install command when a tool is missing; cargo audit and osv-scanner fetch their
# advisory databases, the rest runs offline.
set -euo pipefail

status=0

if command -v cargo-deny >/dev/null 2>&1; then
    echo "== cargo deny =="
    cargo deny check licenses bans sources || status=1
else
    echo "cargo-deny not found. Install manually: cargo install cargo-deny --locked" >&2
    status=1
fi

if command -v cargo-audit >/dev/null 2>&1; then
    echo "== cargo audit (RustSec) =="
    cargo audit || status=1
else
    echo "cargo-audit not found. Install manually: cargo install cargo-audit --locked" >&2
    status=1
fi

if command -v osv-scanner >/dev/null 2>&1; then
    echo "== osv-scanner (OSV, OpenSSF malicious packages) =="
    osv-scanner scan source --lockfile Cargo.lock || status=1
else
    echo "osv-scanner not found. Install manually: brew install osv-scanner" >&2
    status=1
fi

if command -v gitleaks >/dev/null 2>&1; then
    echo "== gitleaks (secrets in the whole history) =="
    gitleaks git . --no-banner --redact || status=1
else
    echo "gitleaks not found. Install manually: brew install gitleaks" >&2
    status=1
fi

# cackle only builds on Linux; skipped (not failed) on macOS.
if [[ "$(uname -s)" != "Linux" ]]; then
    echo "cackle: Linux only, step skipped (run on a CI runner: cargo install cackle --locked && cargo acl check)" >&2
elif command -v cargo-acl >/dev/null 2>&1; then
    echo "== cargo acl (cackle) =="
    cargo acl check || status=1
else
    echo "cackle not found. Install manually: cargo install cackle --locked" >&2
    status=1
fi

exit "$status"
