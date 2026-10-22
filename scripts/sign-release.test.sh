#!/bin/sh
# sign-release.sh with a test key: a key in install.sh's block signs and self-installs; any other key leaves nothing.
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
ssl=/usr/bin/openssl
fail=0
for k in main other; do
    "$ssl" ecparam -name prime256v1 -genkey -noout -out "$t/$k.key" 2>/dev/null
    "$ssl" ec -in "$t/$k.key" -pubout -out "$t/$k.pub" 2>/dev/null
done
awk -v m="$t/main.pub" '
    /^# release keys begin/ { print; printf "keys='\''"; n = 0; while ((getline l < m) > 0) { if (++n > 1) print prev; prev = l } printf "%s'\''\n", prev; skip = 1; next }
    /^# release keys end/ { skip = 0 }
    !skip' "$here/../install.sh" >"$t/install.sh"
mkdir -p "$t/build"
printf '#!/bin/sh\necho "nerd 0.99.0 (test)"\n' >"$t/build/nerd"
chmod 755 "$t/build/nerd"
a="$t/nerd-v0.99.0-macos-arm64.tar.gz"
tar -czf "$a" -C "$t/build" nerd
NERD_RELEASE_KEY="$t/main.key" sh "$here/sign-release.sh" "$a" "$t/install.sh" >/dev/null || { echo "sign-release: refused a key in the block" >&2; fail=1; }
[ -f "$a.sha256" ] && [ -f "$a.sha256.sig" ] || { echo "sign-release: wrote no checksum or signature" >&2; fail=1; }
"$ssl" dgst -sha256 -verify "$t/main.pub" -signature "$a.sha256.sig" "$a.sha256" >/dev/null 2>&1 || { echo "sign-release: the signature does not verify" >&2; fail=1; }
rm -f "$a.sha256" "$a.sha256.sig"
NERD_RELEASE_KEY="$t/other.key" sh "$here/sign-release.sh" "$a" "$t/install.sh" >/dev/null 2>&1 && { echo "sign-release: took a key not in the block" >&2; fail=1; }
[ ! -e "$a.sha256" ] && [ ! -e "$a.sha256.sig" ] || { echo "sign-release: left files after a refusal" >&2; fail=1; }
if [ "$fail" -eq 0 ]; then
    echo "sign-release: signs with a release key and refuses any other"
else
    exit 1
fi
