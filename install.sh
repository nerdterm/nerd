#!/bin/sh
# Usage: curl -fsSL https://nerdterm.dev/install.sh | sh
set -eu

repo=nerdterm/nerd
# Overridable so a release can be checked into a temporary directory without replacing
# whatever build is installed on the machine doing the checking.
bindir="${NERD_BINDIR:-$HOME/.local/bin}"

say() { printf 'nerd: %s\n' "$*"; }
die() { printf 'nerd: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "macOS only"
[ "$(uname -m)" = arm64 ] || die "Apple Silicon only (this Mac is $(uname -m))"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $# -ge 1 ]; then
    [ -f "$1" ] || die "no such file: $1"
    cp "$1" "$work/"
    [ -f "$1.sha256" ] && cp "$1.sha256" "$work/"
else
    # NERD_RELEASE_API lets tests point at a local stand-in; users never set it.
    api="${NERD_RELEASE_API:-https://api.github.com/repos/$repo/releases/latest}"
    # Into a file first: piping straight to curl would lose its exit status.
    if curl -fsSL "$api" -o "$work/release.json" 2>/dev/null; then
        say "downloading the latest release"
        urls=$(grep -o '"browser_download_url": *"[^"]*macos-arm64[^"]*"' "$work/release.json" | cut -d'"' -f4 || true)
        [ -n "$urls" ] || die "the latest release of $repo has no macOS archive yet"
        for u in $urls; do
            curl -fsSL -o "$work/$(basename "$u")" "$u" || die "download failed: $u"
        done
    else
        die "cannot read the latest release of $repo"
    fi
fi

archive=$(ls "$work"/nerd-*-macos-arm64.tar.gz 2>/dev/null | head -n 1)
[ -n "$archive" ] || die "no archive was downloaded"
if [ -f "$archive.sha256" ]; then
    (cd "$work" && shasum -a 256 -c "$(basename "$archive").sha256" >/dev/null) \
        || die "checksum mismatch, nothing installed"
elif [ "${NERD_INSTALL_UNVERIFIED:-}" = 1 ]; then
    say "no checksum next to the archive, installing without verifying it (NERD_INSTALL_UNVERIFIED=1)"
else
    die "no checksum next to the archive, nothing installed; to install it unverified anyway, set NERD_INSTALL_UNVERIFIED=1"
fi

tar -xzf "$archive" -C "$work"
[ -f "$work/nerd" ] || die "the archive holds no nerd binary"
xattr -d com.apple.quarantine "$work/nerd" 2>/dev/null || true
mkdir -p "$bindir"
# install, not cp: macOS caches a signature by inode; overwriting in place gets the binary killed at launch.
install -m 755 "$work/nerd" "$bindir/nerd"
codesign --force --sign - "$bindir/nerd" 2>/dev/null

line='export PATH="$HOME/.local/bin:$PATH"'
case ":$PATH:" in
    *":$bindir:"*) ;;
    *)
        if ! grep -qsF "$line" "$HOME/.zshrc"; then
            printf '\n# nerd\n%s\n' "$line" >> "$HOME/.zshrc"
            say "added ~/.local/bin to PATH in ~/.zshrc"
        fi
        ;;
esac

say "installed $("$bindir/nerd" --version)"
case ":$PATH:" in
    *":$bindir:"*) say "run: nerd" ;;
    *) say "open a new terminal window, then run: nerd" ;;
esac
