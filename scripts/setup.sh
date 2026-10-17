#!/bin/sh
# core.hooksPath is local config: a fresh clone has the hooks on disk but off until this runs.
set -e
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
chmod +x .githooks/* scripts/*.sh
echo "hooks on: $(git config core.hooksPath)"
