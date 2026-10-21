#!/bin/sh
# Signs a release archive and proves it installs with the release's own install.sh: ARCHIVE INSTALL_SH.
# Writes ARCHIVE.sha256 and ARCHIVE.sha256.sig; on any failure writes neither.
set -eu
archive=${1:?usage: scripts/sign-release.sh ARCHIVE INSTALL_SH}
installer=${2:?usage: scripts/sign-release.sh ARCHIVE INSTALL_SH}
key=${NERD_RELEASE_KEY:-$HOME/.config/nerd-release/release.pem}
ssl=/usr/bin/openssl
[ -f "$archive" ] && [ -f "$installer" ] || { echo "sign-release: no such file" >&2; exit 1; }
[ -f "$key" ] || { echo "sign-release: no key at $key (scripts/release-key.sh makes one)" >&2; exit 1; }
sum="$archive.sha256"
trap 'rm -f "$sum" "$sum.sig"' EXIT
(cd "$(dirname "$archive")" && shasum -a 256 "$(basename "$archive")") >"$sum"
"$ssl" dgst -sha256 -sign "$key" -out "$sum.sig" "$sum"
scratch=$(mktemp -d)
# The same check every install makes, into a folder of its own: nothing installed on this Mac changes.
if ! env HOME="$scratch" NERD_BINDIR="$scratch/bin" PATH="$scratch/bin:/usr/bin:/bin:/usr/sbin:/sbin" sh "$installer" "$archive" >"$scratch/out" 2>&1; then
    echo "sign-release: the release's own install.sh refuses it: $(tail -n 1 "$scratch/out")" >&2
    rm -rf "$scratch"
    exit 1
fi
rm -rf "$scratch"
trap - EXIT
echo "signed $(basename "$sum") and installed it into a scratch folder"
