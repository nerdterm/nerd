#!/bin/sh
# The parts of scripts/terminals.sh that read what arrived, without any terminal.
here=$(dirname "$0")
. "$here/terminals/shortcuts.sh"
fail=0
n=0
tab=$(printf '\t')
case_() {
    n=$((n + 1))
    got=$(printf '%s' "$3" | shortcuts_cell "$1")
    if [ "$got" != "$2" ]; then
        echo "case $n FAILED: $1: want '$2' got '$got'" >&2
        fail=1
    fi
}
case_ ghostty 'ok 3/3' "K${tab}char ALT
1${tab}char ALT
shift+Left${tab}Left SHIFT | ALT
"
case_ iterm 'ok 2/4, dead key (menu): N, U' "K${tab}char ALT
N${tab}nothing
U${tab}nothing
shift+Up${tab}Up SHIFT | ALT
"
case_ ghostty 'ok 1/2; N: nothing' "K${tab}char ALT
N${tab}nothing
"
case_ kitty 'ok 0/2; C: char 0x0; shift+Down: Down ALT' "C${tab}char 0x0
shift+Down${tab}Down ALT
"
case_ kitty 'ok 0/1; T: focus lost' "T${tab}focus lost
"
[ "$(printf '%s\n' "$SHORTCUTS" | wc -l | tr -d ' ')" = 27 ] || { echo "SHORTCUTS: want 27 rows" >&2; fail=1; }
if [ "$fail" -eq 0 ]; then
    echo "terminals: $n cases pass"
else
    exit 1
fi
