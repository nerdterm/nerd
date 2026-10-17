#!/bin/sh
# Claude Code PreToolUse hook for Bash: denies (exit 2) a command that holds a private Claude link, and a gh or
# close-issue.sh call whose text fails `privacy.sh command` or whose posted files fail `privacy.sh text`,
# before anything reaches GitHub.
here=$(dirname "$0")
input=$(cat)
if printf '%s' "$input" | grep -q -E 'claude\.ai/(code|artifact|chat|share|project)'; then
    echo "privacy: the command holds a private Claude link; remove it" >&2
    exit 2
fi
cmd=$(printf '%s' "$input" | plutil -extract tool_input.command raw -o - - 2>/dev/null) || exit 0
case $cmd in
*"gh pr "* | *"gh issue "* | *"gh api "*body* | *close-issue.sh*) ;;
*) exit 0 ;;
esac
status=0
printf '%s\n' "$cmd" | sh "$here/privacy.sh" command || status=2
# ceiling: words split on blanks, so a quoted path with spaces, or a file written by the same command, goes unchecked.
set -f
prev=
for t in $cmd; do
    t=${t#[\"\']}
    t=${t%[\"\']}
    f=
    case $prev in --body-file | -F) f=$t ;; esac
    case $t in --body-file=*) f=${t#--body-file=} ;; esac
    case $t in *=@*) f=${t#*=@} ;; esac
    if [ -n "$f" ] && [ -f "$f" ]; then
        sh "$here/privacy.sh" text < "$f" || { echo "privacy: in $f" >&2; status=2; }
    fi
    prev=$t
done
exit $status
