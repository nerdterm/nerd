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

# The public halves of nerd's release keys, the main one and its backup: a release installs only if one of them signed its .sha256.
# release keys begin
keys='-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE2uFWy9Gu9UQ39oSBJ/Q7q2BscGdm
DjoSR3aJXNlr1vrqoc+CRMh/1xd91/h0xke9oDPqJw1MNEz/ZlL794RtLg==
-----END PUBLIC KEY-----
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEMfyVQkWW1y/wroeYI/cRd7uQJBp8
ShA+LTUymRoqK08IRKf7isD1xa04XkiG4nuQE61urBbeFrsaSmLmjpW9Mw==
-----END PUBLIC KEY-----'
# release keys end

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# "x.y.z" is below "a.b.c".
older() { awk -v a="$1" -v b="$2" 'BEGIN { split(a, x, "."); split(b, y, "."); for (i = 1; i <= 3; i++) if (x[i] + 0 != y[i] + 0) exit !(x[i] + 0 < y[i] + 0); exit 1 }'; }

given=
if [ $# -ge 1 ]; then
    [ -f "$1" ] || die "no such file: $1"
    for f in "$1" "$1.sha256" "$1.sha256.sig"; do
        [ ! -e "$f" ] || [ -f "$f" ] || die "$f is not a file, nothing installed"
        if [ -f "$f" ]; then cp "$f" "$work/"; fi
    done
    archive="$work/$(basename "$1")"
    given=1
else
    # NERD_RELEASE_API lets tests point at a local stand-in; users never set it.
    api="${NERD_RELEASE_API:-https://api.github.com/repos/$repo/releases/latest}"
    # Into a file first: piping straight to curl would lose its exit status.
    curl -fsSL "$api" -o "$work/release.json" 2>/dev/null || die "cannot read the latest release of $repo"
    tag=$(grep -o '"tag_name": *"[^"]*"' "$work/release.json" | head -n 1 | cut -d'"' -f4 || true)
    printf '%s' "$tag" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+' || die "the latest release's tag is not a version: $tag"
    # ceiling: a first install has no version to compare with, and this script itself comes from nerdterm.dev; an update from nerd's menu has neither limit.
    have=$("$bindir/nerd" --version 2>/dev/null | awk '{ print $2 }') || have=
    if [ -n "$have" ] && older "${tag#v}" "$have"; then
        die "the latest release, $tag, is older than the installed nerd $have, nothing installed"
    fi
    say "downloading $tag"
    # The three files of that tag, by name: nothing else in the release is looked at.
    name="nerd-$tag-macos-arm64.tar.gz"
    for n in "$name" "$name.sha256" "$name.sha256.sig"; do
        curl -fsSL -o "$work/$n" "https://github.com/$repo/releases/download/$tag/$n" || die "download failed: $n"
    done
    archive="$work/$name"
fi

base=$(basename "$archive")
sum="$archive.sha256"
sig="$sum.sig"
signed=
if [ -e "$sig" ]; then
    [ -f "$sum" ] || die "a signature but no checksum next to the archive, nothing installed"
    printf '%s\n' "$keys" | awk -v d="$work" '/BEGIN PUBLIC KEY/ { n++ } { print > (d "/key" n ".pem") }'
    for k in "$work"/key*.pem; do
        if /usr/bin/openssl dgst -sha256 -verify "$k" -signature "$sig" "$sum" >/dev/null 2>&1; then signed=1; fi
    done
    [ -n "$signed" ] || die "the signature does not check against nerd's release keys, nothing installed"
elif [ -n "$given" ] && [ "${NERD_INSTALL_UNVERIFIED:-}" = 1 ]; then
    say "no signature next to the archive, installing without verifying who made it (NERD_INSTALL_UNVERIFIED=1)"
else
    die "no signature next to the archive, nothing installed; to install it unverified anyway, set NERD_INSTALL_UNVERIFIED=1"
fi
if [ -f "$sum" ]; then
    # One line, exactly what shasum writes for this archive: shasum -c would also take other formats and other names.
    line=$(cat "$sum")
    printf '%s' "$line" | grep -Eqx '[0-9a-f]{64}  [^/]+' && [ "$(printf '%s' "$line" | wc -l | tr -d ' ')" = 0 ] \
        || die "the checksum file is not one line of shasum, nothing installed"
    [ "${line#*  }" = "$base" ] || die "the checksum is for ${line#*  }, not $base, nothing installed"
    [ "$(shasum -a 256 "$archive" | cut -d' ' -f1)" = "${line%%  *}" ] || die "checksum mismatch, nothing installed"
else
    say "no checksum next to the archive either"
fi

tar -xzf "$archive" -C "$work"
[ -f "$work/nerd" ] || die "the archive holds no nerd binary"
xattr -d com.apple.quarantine "$work/nerd" 2>/dev/null || true
mkdir -p "$bindir"
# Anyone who can write a folder on the way to nerd can rename it and put their own nerd in its place. Symlinks: ls on "$d/." reads the folder a link points to, the one the install writes through; the folders above that target are not checked.
d=$bindir
case $bindir/ in
    "$HOME"/*)
        while :; do
            case $(ls -ld "$d/." | cut -c6,9) in
                *w*) say "~${d#"$HOME"} can be written by other users, so one of them could replace nerd; to fix: chmod go-w ~${d#"$HOME"}" ;;
            esac
            [ "$d" = "$HOME" ] || [ "$d" = / ] && break
            d=$(dirname "$d")
        done
        ;;
esac
# Signed beside the old one, then renamed over it: never a moment with no nerd, or an unsigned one.
# A new file, not cp: macOS caches a signature by inode; overwriting in place gets the binary killed at launch.
new=$(mktemp -d "$bindir/.nerd-install.XXXXXX")
trap 'rm -rf "$work" "$new"' EXIT
install -m 755 "$work/nerd" "$new/nerd"
codesign --force --sign - "$new/nerd" 2>/dev/null
mv -f "$new/nerd" "$bindir/nerd"

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
