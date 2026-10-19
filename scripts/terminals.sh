#!/bin/sh
# Starts nerd in each macOS terminal on a test server of its own and prints what reached the server from there.
# Input needs Accessibility and screenshots need Screen Recording for the terminal this runs in; without them those
# columns say "no permission". Run it from Terminal.app; input goes only to the test window it opened.
# usage: scripts/terminals.sh [ghostty kitty wezterm iterm terminal]
set -eu
cd "$(git rev-parse --show-toplevel)"
nerd=${NERD:-$PWD/target/release/nerd}
[ -x "$nerd" ] || { echo "terminals: no $nerd; run cargo build --release" >&2; exit 1; }
stamp=$(date +%Y%m%d-%H%M%S)
run=/tmp/nt/terminals/$stamp
shots=$HOME/.cache/nerd-terminals/$stamp
mkdir -p "$run" "$shots"
swiftc -O -o "$run/post" scripts/terminals/post.swift
post=$run/post
input=ok; "$post" can input || input="no permission"
screen=ok; "$post" can screen || screen="no permission"
if [ "$input" = ok ]; then
    echo "terminals: keep a Latin keyboard layout (ABC or U.S.) during the run; the clipboard is restored as text only" >&2
fi
# What this shell carries must not reach the terminals it opens: they set their own.
clean="env -u TERM_PROGRAM -u TERM_PROGRAM_VERSION -u TERM -u TERMINFO -u COLORTERM -u TMUX -u TMUX_PANE -u STY $(env | sed -n 's/^\(NERD_[A-Z_]*\)=.*/-u \1/p')"
if [ "$(defaults read -g AppleInterfaceStyle 2>/dev/null)" = Dark ]; then ground="19 20 23"; else ground="250 250 251"; fi
# Off the 256-colour cube: a terminal that rounds 24-bit colour misses it by more than the tolerance.
block="40 150 190"
# The same block in sRGB from a terminal that reads 24-bit colour as Display P3 (WezTerm, iTerm2).
p3="0 153 194"
clip='nerd-terminals-paste-ok'
tab=$(printf '\t')
table=$run/table
echo "terminal${tab}attach${tab}TERM_PROGRAM${tab}graphics${tab}stray reply${tab}plain key${tab}Option${tab}Control Latin${tab}Control Cyrillic${tab}arrows${tab}paste${tab}mouse${tab}colour${tab}new window" >"$table"
notes=$run/notes
: >"$notes"
trap 'for h in "$run"/*/; do $clean HOME="$h" NERD_SOCKET="${h}s.sock" "$nerd" --stop >/dev/null 2>&1 || true; done' EXIT

wins() { "$post" windows | awk -v p="$1" 'tolower($0) ~ p { print $1, $2 }'; }

# New trace lines since byte $1 of the log, shortened: "trace key char KeyModifiers(ALT)" reads "char ALT".
since() {
    tail -c +$(($1 + 1)) "$log" | sed -n 's/.*trace \(key \)\{0,1\}//p' | grep -v '^frames' | sed 's/KeyModifiers(\([^)]*\))/\1/' | paste -sd' ' -
}

# Posts with post's arguments, then says ok, what arrived instead of $want, or nothing.
try() {
    want=$1; shift
    at=$(wc -c <"$log")
    for keys in "$@"; do
        "$post" ${keys%% *} "$wid" ${keys#"${keys%% *}"} 2>"$run/err" || { echo "$t: $(cat "$run/err")" >>"$notes"; echo "focus lost"; return; }
    done
    sleep 0.7
    got=$(since "$at")
    if [ "$got" = "$want" ]; then echo ok; else echo "${got:-nothing}"; fi
}

[ $# -gt 0 ] || set -- ghostty kitty wezterm iterm terminal
for t in "$@"; do
    case $t in
        ghostty) app=Ghostty pat=ghostty offer=offered exe=/Ghostty.app/Contents/MacOS/ ;;
        kitty) app=kitty pat=kitty offer=offered exe=/kitty.app/Contents/MacOS/ ;;
        wezterm) app=WezTerm pat=wezterm offer=offered exe=/WezTerm.app/Contents/MacOS/ ;;
        iterm) app=iTerm pat=iterm offer="not offered" exe= ;;
        terminal) app=Terminal pat=terminal offer="not offered" exe= ;;
    esac
    if ! open -Ra "$app" 2>/dev/null; then
        echo "$t${tab}not installed" >>"$table"
        continue
    fi
    h=$run/$t
    log="$h/Library/Application Support/nerd/instances/1/daemon.log"
    mkdir -p "$h"
    for rc in .zshrc .bash_profile; do
        printf 'for i in 1 2 3 4 5 6; do printf "\\033[48;2;%s;%s;%sm%%20s\\033[0m\\n" ""; done\n' $block >"$h/$rc"
    done
    # Started first, so the window an app opens on its own launch is not taken for the test window.
    case $t in iterm | terminal) open -ga "$app" && sleep 2 ;; esac
    wins "$pat" >"$run/before"
    # `open -n` starts a process of its own; the app's other windows may come on screen when it activates.
    if [ -n "$exe" ]; then pgrep -f "$exe" >"$run/pids" || true; fi
    c="/usr/bin/env HOME=$h NERD_SOCKET=$h/s.sock NERD_TRACE=1 $nerd"
    asid=
    case $t in
        # Nothing but -e: Ghostty reads any other argument as configuration and opens an error window.
        ghostty | kitty | wezterm) $clean open -na "$app.app" --args -e $c ;;
        iterm) asid=$(osascript -e "tell application \"iTerm\" to id of (create window with default profile command \"$c\")") || true ;;
        terminal) asid=$(osascript -e "tell application \"Terminal\" to do script \"$c; exit\"" | sed -n 's/.*window id \([0-9]*\).*/\1/p') || true ;;
    esac
    i=0
    until grep -q ' attached: ' "$log" 2>/dev/null || [ $i -ge 30 ]; do sleep 0.5; i=$((i + 1)); done
    sleep 1
    new=$(wins "$pat" | grep -vxFf "$run/before" || true)
    pid=
    if [ -n "$exe" ]; then
        # The new processes include helpers (kitty's kittens): the app is the one that owns a window.
        { pgrep -f "$exe" || true; } | grep -vxFf "$run/pids" >"$run/new-pids" || true
        new=$(echo "$new" | awk 'NR == FNR { p[$1]; next } $2 in p' "$run/new-pids" -)
        pid=$(echo "$new" | awk 'NF { print $2 }' | sort -u)
        if [ "$(echo "$pid" | grep -c . || true)" != 1 ]; then
            echo "$t: $(echo "$pid" | grep -c . || true) new $app processes own a window, wanted one" >>"$notes"
            new='' pid=''
        fi
    fi
    wid=$(echo "$new" | awk 'NF { n++; id = $1 } END { if (n == 1) print id }')
    line=$(grep ' attached: ' "$log" 2>/dev/null | tail -1 || true)
    if [ -z "$line" ]; then
        echo "$t${tab}no attach in 15 s" >>"$table"
    else
        tp=$(sed -n 's/.*started in \(.*\), mux=.*/\1/p' "$log" | tail -1 | sed 's/^?$/none/')
        gr=$(echo "$line" | sed -n 's/.*graphics=\([a-z]*\).*/\1/p')
        at=$(grep -n ' attached: ' "$log" | tail -1 | cut -d: -f1)
        if grep -q 'graphics: the terminal answered late' "$log"; then gr="$gr, true after a late answer"; fi
        keys=$(tail -n +"$at" "$log" | grep -c 'trace key' || true)
        # A reply the server swallowed: its keys are its text plus the two that open and close it.
        sw=$(tail -n +"$at" "$log" | sed -n 's/.*terminal reply swallowed: //p')
        told=$(echo "$sw" | awk 'NF { n += ($2 == "chars" ? $1 : length($0)) + 2 } END { print n + 0 }')
        if [ -n "$sw" ]; then
            stray="swallowed: $(echo "$sw" | paste -sd' ' -)"
            if [ "$keys" -gt "$told" ]; then stray="$stray, $((keys - told)) keys more"; fi
        elif [ "$keys" = 0 ]; then stray=none
        else stray="$keys keys"; fi
        if [ -z "$wid" ]; then
            cells="no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window"
            echo "$t: $(echo "$new" | grep -c . || true) new windows on screen, wanted one (a full-screen Space or a locked screen keeps them off it)" >>"$notes"
        else
            colour=$screen
            if [ "$screen" = ok ]; then
                screencapture -x -o -l "$wid" "$shots/$t.png"
                g=$("$post" colour "$shots/$t.png" $ground)
                b=$("$post" colour "$shots/$t.png" $block)
                p=$("$post" colour "$shots/$t.png" $p3)
                if [ "$g" -ge 1000 ] && [ "$b" -ge 10 ]; then colour=ok
                elif [ "$g" -ge 1000 ] && [ "$p" -ge 10 ]; then colour="ok, Display P3"
                else colour=$(echo "$g $b" | awk '{ printf "ground %d%%, 24-bit %.1f%%", $1 / 100, $2 / 100 }'); fi
            fi
            if [ "$input" = ok ]; then
                case $t in
                    iterm) osascript -e "tell application \"iTerm\" to select window id $asid" >/dev/null 2>&1 || true ;;
                    terminal) osascript -e "tell application \"Terminal\" to set index of window id $asid to 1" >/dev/null 2>&1 || true ;;
                esac
                if "$post" raise "$wid" 2>"$run/err"; then
                    cells="$(try "char 0x0" "key 0")${tab}$(try "char ALT" "key 0 option")"
                    cells="$cells${tab}$(try "Char('c') CONTROL" "key 8 control")"
                    cells="$cells${tab}$(try "Char('с') CONTROL" "key 8 control с")"
                    cells="$cells${tab}$(try "Left 0x0 Right 0x0 Down 0x0 Up 0x0" "key 123" "key 124" "key 125" "key 126")"
                    pbpaste >"$run/clipboard"
                    printf %s "$clip" | pbcopy
                    cells="$cells${tab}$(try "paste ${#clip} chars" "key 9 command")"
                    pbcopy <"$run/clipboard"
                    cells="$cells${tab}$(try "mouse Down(Left) 0x0 mouse Up(Left) 0x0" click)"
                else
                    echo "$t: $(cat "$run/err")" >>"$notes"
                    cells="focus not obtained${tab}focus not obtained${tab}focus not obtained${tab}focus not obtained${tab}focus not obtained${tab}focus not obtained${tab}focus not obtained"
                fi
            else
                cells="$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input"
            fi
            cells="$cells${tab}$colour"
        fi
        echo "$t${tab}ok${tab}$tp${tab}$gr${tab}$stray${tab}$cells${tab}$offer" >>"$table"
    fi
    server=$(sed -n 's/.*server\[\([0-9]*\)\].*/\1/p' "$log" 2>/dev/null | tail -1)
    $clean HOME="$h" NERD_SOCKET="$h/s.sock" "$nerd" --stop >/dev/null || true
    sleep 2
    if [ -n "$server" ] && ps -p "$server" >/dev/null; then echo "$t: server $server still running" >>"$notes"; fi
    # The one process `open -n` started for this run, never one that was running before it.
    case $t in ghostty | kitty | wezterm)
        [ -z "$pid" ] || kill "$pid" 2>/dev/null || true ;;
    # AppleScript's own id for the window it opened: it is not a window-server id.
    iterm | terminal) [ -z "$asid" ] || osascript -e "tell application \"$app\" to close window id $asid" >/dev/null 2>&1 || true ;;
    esac
    i=0
    while [ -n "$wid" ] && "$post" windows | awk '{ print $1 }' | grep -qx "$wid" && [ $i -lt 10 ]; do sleep 0.5; i=$((i + 1)); done
    if [ -n "$wid" ] && "$post" windows | awk '{ print $1 }' | grep -qx "$wid"; then echo "$t: window $wid left open" >>"$notes"; fi
done
echo "Alacritty${tab}not checked: its Homebrew cask is disabled (fails Gatekeeper)" >>"$table"
echo "Warp${tab}not checked: closed source, needs an account, has its own input editor" >>"$table"

column -t -s "$tab" "$table"
cat "$notes"
if [ "$screen" = ok ]; then echo "screenshots: $shots"; fi
echo "logs: $run"
