#!/bin/sh
# Renders the formula through a stub gh that answers only `release download`: nothing reaches GitHub.
# The release's checksum is signed with a test key that a copy of install.sh carries.
here=$(cd "$(dirname "$0")" && pwd)
stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
ssl=/usr/bin/openssl
cat >"$stub/gh" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/calls"
[ "$1 $2" = "release download" ] || exit 1
while [ $# -gt 0 ]; do case $1 in -p) p=$2; shift ;; -O) o=$2; shift ;; esac; shift; done
[ -f "$STUB/served/$p" ] && cp "$STUB/served/$p" "$o"
EOF
chmod +x "$stub/gh"
for k in main other; do
    "$ssl" ecparam -name prime256v1 -genkey -noout -out "$stub/$k.key" 2>/dev/null
    "$ssl" ec -in "$stub/$k.key" -pubout -out "$stub/$k.pub" 2>/dev/null
done
awk -v m="$stub/main.pub" '
    /^# release keys begin/ { print; printf "keys='\''"; n = 0; while ((getline l < m) > 0) { if (++n > 1) print prev; prev = l } printf "%s'\''\n", prev; skip = 1; next }
    /^# release keys end/ { skip = 0 }
    !skip' "$here/../install.sh" >"$stub/install.sh"
name=nerd-v1.2.3-macos-arm64.tar.gz
mkdir -p "$stub/served"
# Serves `line` as the checksum, signed with `key` (none for no signature).
serve() {
    rm -f "$stub/served/"*
    printf '%s\n' "$1" >"$stub/served/$name.sha256"
    [ "$2" = none ] || "$ssl" dgst -sha256 -sign "$stub/$2.key" -out "$stub/served/$name.sha256.sig" "$stub/served/$name.sha256"
}
render() { PATH="$stub:$PATH" STUB=$stub NERD_INSTALL_SH="$stub/install.sh" sh "$here/publish-formula.sh" --dry-run "$@"; }
sha=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
fail=0
check() {
    grep -qxF -- "$1" "$stub/out" || { echo "publish-formula: no line '$1'" >&2; fail=1; }
}
serve "$sha  $name" main
render v1.2.3 >"$stub/out" || fail=1
check '  url "https://github.com/nerdterm/nerd/releases/download/v1.2.3/nerd-v1.2.3-macos-arm64.tar.gz"'
check "  sha256 \"$sha\""
check '  license "MIT"'
check '  depends_on arch: :arm64'
check '    bin.install "nerd"'
check '    assert_match version.to_s, shell_output("#{bin}/nerd --version")'
# Homebrew reads the version from the url; a version line is one more place to forget.
! grep -q '^ *version ' "$stub/out" || { echo "publish-formula: a version line" >&2; fail=1; }
ruby -W0 -c "$stub/out" >/dev/null || fail=1
calls=$(grep -c "^release download v1.2.3 --repo nerdterm/nerd -p $name.sha256\(\.sig\)\? -O " "$stub/calls")
[ "$calls" = 2 ] && [ "$(wc -l <"$stub/calls" | tr -d ' ')" = 2 ] || { echo "publish-formula: a dry run called $(cat "$stub/calls")" >&2; fail=1; }
for bad in "1.2.3" "v1.2" "v1.2.3;x" "v1.2.3
x"; do
    render "$bad" >/dev/null 2>&1 && { echo "publish-formula: took the tag $bad" >&2; fail=1; }
done
serve "$sha  $name" other
render v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a signature by a key not in install.sh" >&2; fail=1; }
serve "$sha  $name" none
render v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a release with no signature" >&2; fail=1; }
serve "$sha  $name
$sha  $name" main
render v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a checksum of two lines" >&2; fail=1; }
serve "not-a-sum  $name" main
render v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a bad checksum" >&2; fail=1; }
serve "$sha  nerd-v1.2.0-macos-arm64.tar.gz" main
render v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a checksum for another archive" >&2; fail=1; }
if [ "$fail" -eq 0 ]; then
    echo "publish-formula: the formula renders only for a release signed with install.sh's keys"
else
    exit 1
fi
