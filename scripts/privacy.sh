#!/bin/sh
# What must never be published: key material and tokens, private Claude links, `ponytail` markers, this
# account's home path, and the names and addresses in info/privacy under the git common dir: a local list,
# one per line, matched as whole words, never tracked. Refusals name the place, never what matched.
#   privacy.sh tree              tracked files
#   privacy.sh push <rev-list>   messages and author, committer and tagger mail of what a push sends
#   privacy.sh text              stdin, such as an issue or pull request body
#   privacy.sh command           stdin, a command line: only private links and info/privacy names
set -eu
# A replace ref hides commits from rev-list that push still sends.
export GIT_NO_REPLACE_OBJECTS=1

list="$(git rev-parse --path-format=absolute --git-common-dir)/info/privacy"
if [ ! -s "$list" ]; then
    echo "private-name check skipped: no info/privacy" >&2
    list=
fi
me=$(id -un | sed 's/[.]/[.]/g')
# The manifests' bare "claude.ai" host is not one of these.
link='claude\.ai/(code|artifact|chat|share|project)'
# Written so that none of them matches its own line.
patterns="private key	-----BEGIN [A-Z ]*PRIVATE KEY-----
GitHub token	gh[pousr]_[A-Za-z0-9]{20,}
GitHub token	github_pat_[A-Za-z0-9_]{20,}
Anthropic key	sk-ant-[A-Za-z0-9_-]{20,}
AWS key	AKIA[0-9A-Z]{16}
Slack token	xox[bp]-[A-Za-z0-9-]{10,}
private Claude link	$link
ponytail marker, use ceiling:	ponytail[:]
home path	/Users/$me([^A-Za-z0-9._-]|\$)"
# Only in tree: a test may hold a hit; a message or body never may.
exempt='privacy: test'

# grep -n lines on stdin -> one refusal per place; $1 the fields that make the place, $2 a prefix, $3 what it is.
report() { cut -d: -f"$1" | sed "s|^|privacy: $2|; s|\$|: $3|"; }

# stdin; $1 says where it came from.
check_text() {
    t=$(cat)
    printf '%s\n' "$patterns" | while IFS='	' read -r name re; do
        printf '%s\n' "$t" | grep -n -E -e "$re" | report 1 "$1 line " "$name"
    done
    if [ -n "$list" ]; then
        printf '%s\n' "$t" | grep -n -i -w -F -f "$list" | report 1 "$1 line " "a name from info/privacy"
    fi
}

private_mail() { [ -n "$list" ] && printf '%s\n' "$1" | grep -q -i -w -F -f "$list"; }

# Refusals on stdout; notes that are not refusals on stderr.
main() {
    case "${1:-}" in
    tree)
        cd "$(git rev-parse --show-toplevel)"
        set -- -- . ':!scripts/privacy.sh' ':!scripts/privacy.test.sh'
        printf '%s\n' "$patterns" | while IFS='	' read -r name re; do
            git grep -n -I -E -e "$re" "$@" | grep -v -F "$exempt" | report 1,2 "" "$name"
        done
        if [ -n "$list" ]; then
            git grep -n -I -i -w -F -f "$list" "$@" | grep -v -F "$exempt" | report 1,2 "" "a name from info/privacy"
        fi
        ;;
    push)
        shift
        commits=$(git rev-list "$@")
        for c in $commits; do
            s=$(git rev-parse --short "$c")
            git log -1 --format=%B "$c" | check_text "commit $s message"
            if private_mail "$(git log -1 --format=%ae "$c")"; then echo "privacy: commit $s: author mail is in info/privacy"; fi
            if private_mail "$(git log -1 --format=%ce "$c")"; then echo "privacy: commit $s: committer mail is in info/privacy"; fi
        done
        for a; do
            case $a in -* | ^*) continue ;; esac
            [ "$(git cat-file -t "$a" 2>/dev/null)" = tag ] || continue
            git cat-file tag "$a" | sed '1,/^$/d' | check_text "tag $a message"
            if private_mail "$(git cat-file tag "$a" | sed -n 's/^tagger [^<]*<\([^>]*\)>.*/\1/p')"; then
                echo "privacy: tag $a: tagger mail is in info/privacy"
            fi
        done
        git log --format= --numstat "$@" | awk -F'\t' '$1 == "-" { print $3 }' | sort -u | while read -r f; do
            echo "privacy: binary, check by eye: $f" >&2
        done
        ;;
    text)
        check_text stdin
        ;;
    command)
        patterns="private Claude link	$link"
        check_text stdin
        ;;
    *)
        echo "usage: privacy.sh tree | push <rev-list arguments> | text < file | command < file" >&2
        exit 2
        ;;
    esac
}

out=$(main "$@")
[ -z "$out" ] || { printf '%s\n' "$out" >&2; exit 1; }
