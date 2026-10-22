#!/bin/sh
# Once, from a plain terminal: the release signing key and its backup, encrypted with a passphrase you generate.
set -eu
dir="${NERD_RELEASE_DIR:-$HOME/.config/nerd-release}"
ssl=/usr/bin/openssl
[ -t 0 ] || { echo "release-key: run it from a terminal" >&2; exit 1; }
[ ! -e "$dir/release.pem" ] && [ ! -e "$dir/backup.pem" ] || { echo "release-key: $dir already holds a key; nothing written" >&2; exit 1; }
echo "Generate a passphrase of at least 24 random characters in your password manager and store it there."
# Echo back on whatever ends the prompts, Control + C included.
trap 'stty echo 2>/dev/null; echo; exit 130' INT
ask() { stty -echo 2>/dev/null || true; printf '%s' "$1" >&2; IFS= read -r v || v=; stty echo 2>/dev/null || true; printf '\n' >&2; printf '%s' "$v"; }
pass=$(ask "passphrase: ")
[ "$(ask "again: ")" = "$pass" ] || { echo "release-key: the two differ; nothing written" >&2; exit 1; }
[ ${#pass} -ge 24 ] || { echo "release-key: shorter than 24 characters; nothing written" >&2; exit 1; }
mkdir -p "$dir"
chmod 700 "$dir"
umask 077
# Both or neither: a half-made pair would make the next run refuse.
trap 'rm -f "$dir/release.pem" "$dir/backup.pem"' EXIT
trap 'stty echo 2>/dev/null; exit 130' INT
# ceiling: LibreSSL 3.3.6 fixes PBKDF2 at 2048 iterations (no -iter); the passphrase's length is what protects the file, a stronger KDF when the system openssl offers one.
for k in release backup; do
    "$ssl" ecparam -name prime256v1 -genkey -noout | NERD_KEY_PASS=$pass "$ssl" pkcs8 -topk8 -v2 aes-256-cbc -passout env:NERD_KEY_PASS -out "$dir/$k.pem"
done
pubs=$(for k in release backup; do NERD_KEY_PASS=$pass "$ssl" pkey -in "$dir/$k.pem" -passin env:NERD_KEY_PASS -pubout; done)
trap - EXIT INT
echo
echo "Public keys, for install.sh's key block:"
printf '%s\n' "$pubs"
cat <<EOF

Next:
1. Store $dir/backup.pem in your password manager, next to its passphrase, then delete it from this Mac.
2. Keep $dir out of any sync; Time Machine copies only the encrypted file.
3. Give the two public keys above to whoever updates install.sh.
EOF
