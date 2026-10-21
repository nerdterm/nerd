#!/bin/sh
# Writes Formula/nerd.rb in nerdterm/homebrew-tap for a published release; --dry-run prints it and writes nothing.
set -eu
dry=
[ "${1:-}" = --dry-run ] && { dry=1; shift; }
tag=${1:?usage: scripts/publish-formula.sh [--dry-run] vX.Y.Z}
# case, not grep: grep checks line by line, so a value with a newline would pass.
case $tag in *[!v0-9.]* | '') tag_ok= ;; *) tag_ok=1 ;; esac
[ -n "$tag_ok" ] && echo "$tag" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+' || { echo "publish-formula: not a release tag: $tag" >&2; exit 1; }
name="nerd-$tag-macos-arm64.tar.gz"
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
# The published checksum, never a rebuilt one: a second archive of the same tag has another hash.
gh release download "$tag" --repo nerdterm/nerd -p "$name.sha256" -O "$t/$name.sha256" || { echo "publish-formula: $tag has no checksum" >&2; exit 1; }
gh release download "$tag" --repo nerdterm/nerd -p "$name.sha256.sig" -O "$t/$name.sha256.sig" || { echo "publish-formula: $tag has no signature" >&2; exit 1; }
# The keys of this checkout's install.sh, never fetched: GitHub is what the signature guards against.
installer=${NERD_INSTALL_SH:-$(git rev-parse --show-toplevel)/install.sh}
awk '/^# release keys begin/ { f = 1; next } /^# release keys end/ { f = 0 } f' "$installer" | sed -e "1s/^keys='//" -e "\$s/'\$//" \
    | awk -v d="$t" '/BEGIN PUBLIC KEY/ { n++ } { print > (d "/key" n ".pem") }'
signed=
for k in "$t"/key*.pem; do
    if /usr/bin/openssl dgst -sha256 -verify "$k" -signature "$t/$name.sha256.sig" "$t/$name.sha256" >/dev/null 2>&1; then signed=1; fi
done
[ -n "$signed" ] || { echo "publish-formula: the signature of $tag does not check against install.sh's release keys" >&2; exit 1; }
line=$(cat "$t/$name.sha256")
printf '%s' "$line" | grep -Eqx '[0-9a-f]{64}  [^/]+' && [ "$(printf '%s' "$line" | wc -l | tr -d ' ')" = 0 ] && [ "${line#*  }" = "$name" ] \
    || { echo "publish-formula: $tag's checksum is not one line for $name" >&2; exit 1; }
sha=${line%%  *}
formula=$(cat <<EOF
class Nerd < Formula
  desc "Terminal multiplexer for AI agents"
  homepage "https://nerdterm.dev"
  url "https://github.com/nerdterm/nerd/releases/download/$tag/$name"
  sha256 "$sha"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "nerd"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/nerd --version")
  end
end
EOF
)
if [ -n "$dry" ]; then
    printf '%s\n' "$formula"
    exit 0
fi
tap=repos/nerdterm/homebrew-tap/contents
put() {
    old=$(gh api "$tap/$1" -q .sha 2>/dev/null) || old=
    gh api -X PUT "$tap/$1" -f message="$2" -f content="$(printf '%s\n' "$3" | base64)" ${old:+-f sha="$old"} >/dev/null
}
gh api "$tap/README.md" --silent 2>/dev/null || put README.md "README" "# nerdterm/tap

Homebrew tap for [nerd](https://nerdterm.dev), a terminal multiplexer for AI agents. Apple Silicon Macs.

\`\`\`sh
brew install nerdterm/tap/nerd
brew upgrade nerd
\`\`\`

The formula is written by nerd's release script; changes go to https://github.com/nerdterm/nerd."
put Formula/nerd.rb "nerd ${tag#v}" "$formula"
echo "updated the Homebrew formula to ${tag#v}"
