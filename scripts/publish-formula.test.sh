#!/bin/sh
# Renders the formula through a stub gh that answers only `release download`: nothing reaches GitHub.
here=$(cd "$(dirname "$0")" && pwd)
stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
cat >"$stub/gh" <<'EOF'
#!/bin/sh
echo "$*" >>"$STUB/calls"
[ "$1 $2" = "release download" ] || exit 1
printf '%s  nerd-v1.2.3-macos-arm64.tar.gz\n' "$SHA"
EOF
chmod +x "$stub/gh"
sha=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
fail=0
check() {
    grep -qxF -- "$1" "$stub/out" || { echo "publish-formula: no line '$1'" >&2; fail=1; }
}
PATH="$stub:$PATH" STUB=$stub SHA=$sha sh "$here/publish-formula.sh" --dry-run v1.2.3 >"$stub/out" || fail=1
check '  url "https://github.com/nerdterm/nerd/releases/download/v1.2.3/nerd-v1.2.3-macos-arm64.tar.gz"'
check "  sha256 \"$sha\""
check '  license "MIT"'
check '  depends_on arch: :arm64'
check '    bin.install "nerd"'
check '    assert_match version.to_s, shell_output("#{bin}/nerd --version")'
# Homebrew reads the version from the url; a version line is one more place to forget.
! grep -q '^ *version ' "$stub/out" || { echo "publish-formula: a version line" >&2; fail=1; }
ruby -W0 -c "$stub/out" >/dev/null || fail=1
[ "$(cat "$stub/calls")" = "release download v1.2.3 --repo nerdterm/nerd -p nerd-v1.2.3-macos-arm64.tar.gz.sha256 -O -" ] || { echo "publish-formula: a dry run called $(cat "$stub/calls")" >&2; fail=1; }
for bad in "1.2.3" "v1.2" "v1.2.3;x" "v1.2.3
x"; do
    PATH="$stub:$PATH" STUB=$stub SHA=$sha sh "$here/publish-formula.sh" --dry-run "$bad" >/dev/null 2>&1 && { echo "publish-formula: took the tag $bad" >&2; fail=1; }
done
PATH="$stub:$PATH" STUB=$stub SHA="$sha
GARBAGE" sh "$here/publish-formula.sh" --dry-run v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a checksum of two lines" >&2; fail=1; }
PATH="$stub:$PATH" STUB=$stub SHA="not-a-sum" sh "$here/publish-formula.sh" --dry-run v1.2.3 >/dev/null 2>&1 && { echo "publish-formula: took a bad checksum" >&2; fail=1; }
if [ "$fail" -eq 0 ]; then
    echo "publish-formula: the formula renders"
else
    exit 1
fi
