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

# post_shortcuts against a stand-in server: step writes the trace line iTerm2's run gave and, like the real step
# and resolve, sets plain names of its own (an iTerm2 window's name once replaced every shortcut's name).
log=$(mktemp)
trap 'rm -f "$log"' EXIT
step() {
    name="nerd 1" code=x mods=x at=0 i=99 as=x box=x wid=x sub=x
    case $2 in
        53) echo "x DEBUG server[1] trace key Esc KeyModifiers(0x0)" >>"$log" ;;
        45 | 32) ;;
        123) echo "x DEBUG server[1] trace key Left KeyModifiers(SHIFT | ALT)" >>"$log" ;;
        124) echo "x DEBUG server[1] trace key Right KeyModifiers(SHIFT | ALT)" >>"$log" ;;
        126) echo "x DEBUG server[1] trace key Up KeyModifiers(SHIFT | ALT)" >>"$log" ;;
        125) echo "x DEBUG server[1] trace key Down KeyModifiers(SHIFT | ALT)" >>"$log" ;;
        *) echo "x DEBUG server[1] trace key char KeyModifiers(ALT)" >>"$log" ;;
    esac
}
rows=$(post_shortcuts)
n=$((n + 1))
if [ "$(printf '%s\n' "$rows" | cut -f1)" != "$(printf '%s\n' "$SHORTCUTS" | cut -d' ' -f1)" ]; then
    echo "case $n FAILED: post_shortcuts lost the names: $(printf '%s\n' "$rows" | head -3 | tr '\n' '|')" >&2
    fail=1
fi
case_ iterm 'ok 25/27, dead key (menu): N, U' "$rows
"
if [ "$fail" -eq 0 ]; then
    echo "terminals: $n cases pass"
else
    exit 1
fi
