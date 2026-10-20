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
# The published checksum, never a rebuilt one: a second archive of the same tag has another hash.
sha=$(gh release download "$tag" --repo nerdterm/nerd -p "$name.sha256" -O - | cut -d' ' -f1)
case $sha in *[!0-9a-f]* | '') sha_ok= ;; *) sha_ok=1 ;; esac
[ -n "$sha_ok" ] && [ ${#sha} -eq 64 ] || { echo "publish-formula: $tag has no checksum: $sha" >&2; exit 1; }
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
