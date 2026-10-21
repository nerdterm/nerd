#!/bin/sh
# install.sh with its key block swapped for test keys: every way a release is refused, and the ways it installs.
here=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
ssl=/usr/bin/openssl
fail=0
bad() { echo "install.test: $*" >&2; fail=1; }

# The real block: two P-256 public keys.
awk '/^# release keys begin/ { f = 1; next } /^# release keys end/ { f = 0 } f' "$here/install.sh" | sed -e "1s/^keys='//" -e "\$s/'\$//" >"$t/real.pem"
n=$(grep -c 'BEGIN PUBLIC KEY' "$t/real.pem")
[ "$n" = 2 ] || bad "install.sh holds $n release keys, not 2"
awk -v d="$t" '/BEGIN PUBLIC KEY/ { n++ } { print > (d "/real" n ".pem") }' "$t/real.pem"
for k in "$t"/real[0-9].pem; do
    "$ssl" ec -pubin -in "$k" -noout -text 2>/dev/null | grep -q prime256v1 || bad "a release key in install.sh is not a P-256 public key"
done

# Test keys: main and backup in the block, a third one outside it.
for k in main backup other; do
    "$ssl" ecparam -name prime256v1 -genkey -noout -out "$t/$k.key" 2>/dev/null
    "$ssl" ec -in "$t/$k.key" -pubout -out "$t/$k.pub" 2>/dev/null
done
awk -v m="$t/main.pub" -v b="$t/backup.pub" '
    /^# release keys begin/ { print; printf "keys='\''"; while ((getline l < m) > 0) print l; n = 0; while ((getline l < b) > 0) { if (++n > 1) print prev; prev = l } printf "%s'\''\n", prev; skip = 1; next }
    /^# release keys end/ { skip = 0 }
    !skip' "$here/install.sh" >"$t/install.sh"

# A release of `version` in `dir`: its archive, the shasum line, and that line signed with `key` (none for no .sig).
release() {
    dir=$1 version=$2 key=$3
    mkdir -p "$dir/build"
    printf '#!/bin/sh\necho "nerd %s (test)"\n' "$version" >"$dir/build/nerd"
    chmod 755 "$dir/build/nerd"
    name="nerd-v$version-macos-arm64.tar.gz"
    tar -czf "$dir/$name" -C "$dir/build" nerd
    (cd "$dir" && shasum -a 256 "$name" >"$name.sha256")
    [ "$key" = none ] || sign "$dir/$name.sha256" "$key"
    echo "$dir/$name"
}
sign() { "$ssl" dgst -sha256 -sign "$t/$2.key" -out "$1.sig" "$1"; }

# install.sh into a fresh bin; prints the installed version, or "none".
run() {
    bin=$t/bin.$1
    shift
    rm -rf "$bin"
    mkdir -p "$bin" "$t/home"
    [ -n "${HAVE:-}" ] && printf '#!/bin/sh\necho "nerd %s (old)"\n' "$HAVE" >"$bin/nerd" && chmod 755 "$bin/nerd"
    env HOME="$t/home" NERD_BINDIR="$bin" PATH="$t/stub:$bin:/usr/bin:/bin:/usr/sbin:/sbin" "$@" sh "$t/install.sh" ${A:+"$A"} >"$t/out" 2>&1
    v=$("$bin/nerd" --version 2>/dev/null | awk '{ print $2 }')
    echo "${v:-none}"
}
expect() {
    got=$1 want=$2 case=$3
    [ "$got" = "$want" ] || bad "$case: installed $got, wanted $want ($(tail -n 1 "$t/out"))"
}

g=$t/given
a=$(release "$g/good" 0.99.0 main)
expect "$(A=$a run good)" 0.99.0 "signed by the main key"
a=$(release "$g/backup" 0.99.0 backup)
expect "$(A=$a run backup)" 0.99.0 "signed by the backup key"
a=$(release "$g/byte" 0.99.0 main)
printf x >>"$a"
expect "$(A=$a run byte)" none "a byte added to the archive"
a=$(release "$g/edited" 0.99.0 main)
printf '%064d  %s\n' 0 "$(basename "$a")" >"$a.sha256"
expect "$(A=$a run edited)" none "the checksum changed after signing"
a=$(release "$g/other" 0.99.0 other)
expect "$(A=$a run other)" none "signed by a key not in install.sh"
a=$(release "$g/nosig" 0.99.0 none)
expect "$(A=$a run nosig)" none "no signature"
expect "$(A=$a run nosig-unverified NERD_INSTALL_UNVERIFIED=1)" 0.99.0 "no signature, NERD_INSTALL_UNVERIFIED=1"
a=$(release "$g/badsig" 0.99.0 main)
printf 'not a signature' >"$a.sha256.sig"
expect "$(A=$a run badsig NERD_INSTALL_UNVERIFIED=1)" none "a bad signature, NERD_INSTALL_UNVERIFIED=1"
a=$(release "$g/nosum" 0.99.0 main)
rm "$a.sha256"
expect "$(A=$a run nosum)" none "a signature but no checksum"
for form in bsd star comment two; do
    a=$(release "$g/$form" 0.99.0 none)
    h=$(cut -d' ' -f1 "$a.sha256") n=$(basename "$a")
    case $form in
        bsd) printf 'SHA256 (%s) = %s\n' "$n" "$h" ;;
        star) printf '%s *%s\n' "$h" "$n" ;;
        comment) printf '# made by hand\n%s  %s\n' "$h" "$n" ;;
        two) printf '%s  %s\n%s  %s\n' "$h" "$n" "$h" "$n" ;;
    esac >"$a.sha256"
    sign "$a.sha256" main
    expect "$(A=$a run "$form")" none "a signed checksum file in the $form form"
done
a=$(release "$g/named" 0.99.0 none)
printf '%s  nerd-v0.98.0-macos-arm64.tar.gz\n' "$(cut -d' ' -f1 "$a.sha256")" >"$a.sha256"
sign "$a.sha256" main
expect "$(A=$a run named)" none "a signed checksum naming another archive"
a=$(release "$g/down" 0.98.0 main)
expect "$(HAVE=0.99.0 A=$a run down)" 0.98.0 "an older archive given by hand"

# The download path, through a stand-in curl serving $SERVED by the last part of the url.
mkdir -p "$t/stub"
cat >"$t/stub/curl" <<'EOF'
#!/bin/sh
out= url=
while [ $# -gt 0 ]; do case $1 in -o) out=$2; shift ;; -*) ;; *) url=$1 ;; esac; shift; done
echo "$url" >>"$SERVED/asked"
[ -f "$SERVED/${url##*/}" ] && cp "$SERVED/${url##*/}" "$out"
EOF
chmod 755 "$t/stub/curl"
s=$t/served
release "$s" 1.2.3 main >/dev/null
release "$s" 1.2.0 main >/dev/null
printf '{"tag_name": "v1.2.3", "assets": []}\n' >"$s/latest"
dl() { SERVED=$s run "$@" NERD_RELEASE_API=https://api.invalid/latest SERVED="$s"; }
rm -f "$s/asked"
expect "$(dl latest)" 1.2.3 "the latest release"
[ "$(grep -c 'v1\.2\.0' "$s/asked")" = 0 ] || bad "the download path fetched another release's files"
[ "$(grep -c '/releases/download/v1\.2\.3/' "$s/asked")" = 3 ] || bad "the download path did not fetch exactly the tag's three files"
expect "$(HAVE=1.3.0 dl older)" 1.3.0 "a latest release older than the installed nerd"
expect "$(HAVE=1.2.3 dl same)" 1.2.3 "the installed version again"
mv "$s/nerd-v1.2.3-macos-arm64.tar.gz.sha256.sig" "$t/saved.sig"
expect "$(dl stripped NERD_INSTALL_UNVERIFIED=1)" none "the signature stripped from the release, NERD_INSTALL_UNVERIFIED=1"
mv "$t/saved.sig" "$s/nerd-v1.2.3-macos-arm64.tar.gz.sha256.sig"
# A signed release served under the bad tag too, so only the tag's own check refuses it.
release "$s" 1.2 main >/dev/null
printf '{"tag_name": "v1.2", "assets": []}\n' >"$s/latest"
expect "$(dl tag)" none "a tag that is not a version"

if [ "$fail" -eq 0 ]; then
    echo "install.test: install.sh refuses every unsigned or altered release"
else
    exit 1
fi
