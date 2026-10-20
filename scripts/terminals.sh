#!/bin/sh
# Starts nerd in each macOS terminal on a test server of its own and prints what reached the server from there.
# Input needs Accessibility and screenshots need Screen Recording for the terminal this runs in; without them those
# columns say "no permission". Run it from Terminal.app; input goes only to the test window it opened.
# --preset applies each terminal's nerd --preset to its test home and starts the terminal on that config alone, so
# the Option columns show the preset's effect; iTerm2 reads only the real home's settings and is not checked so.
# usage: scripts/terminals.sh [--preset] [ghostty kitty wezterm iterm]
set -eu
preset=
if [ "${1:-}" = --preset ]; then preset=1; shift; fi
cd "$(git rev-parse --show-toplevel)"
nerd=${NERD:-$PWD/target/release/nerd}
[ -x "$nerd" ] || { echo "terminals: no $nerd; run cargo build --release" >&2; exit 1; }
stamp=$(date +%Y%m%d-%H%M%S)
run=/tmp/nt/terminals/$stamp
shots=$HOME/.cache/nerd-terminals/$stamp
mkdir -p "$run" "$shots"
swiftc -O -o "$run/post" scripts/terminals/post.swift
. scripts/terminals/shortcuts.sh
# The shortcuts are made for a Latin layout: the column is measured only on one.
layout=$(defaults read com.apple.HIToolbox AppleCurrentKeyboardLayoutInputSourceID 2>/dev/null || true)
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
echo "terminal${tab}attach${tab}TERM_PROGRAM${tab}graphics${tab}stray reply${tab}plain key${tab}Option${tab}Control Latin${tab}Control Cyrillic${tab}arrows${tab}paste${tab}mouse${tab}shortcuts${tab}prefix${tab}prefix, Russian${tab}colour${tab}new window" >"$table"
notes=$run/notes
: >"$notes"
# A run stopped mid-paste still gives the clipboard back.
trap 'if [ -f "$run/clipboard" ]; then pbcopy <"$run/clipboard"; fi
for h in "$run"/*/; do $clean HOME="$h" NERD_SOCKET="${h}s.sock" "$nerd" --stop >/dev/null 2>&1 || true; done' EXIT

# A process and everything under it.
family() ( for c in $(pgrep -P "$1" || true); do family "$c"; done; echo "$1" )

# This app's windows on screen: id, pid, owner, x,y,w,h, name, tab-separated.
wins() { "$post" windows | awk -F "$tab" -v p="$pat" 'tolower($3) ~ p'; }

# The test window's id, found again before each use: a window can change its id or leave the screen.
resolve() {
    wid=
    case $t in
        ghostty | kitty | wezterm)
            wid=$(wins | awk -F "$tab" -v p="$pid" '$2 == p { n++; id = $1 } END { if (n == 1) print id }') ;;
        iterm)
            # The window AppleScript opened, only if it is new and holds the test tab alone, matched by bounds and name.
            as=$(osascript -e "tell application \"$app\" to get {count of tabs, bounds, name} of window id $asid" 2>/dev/null || true)
            box=$(echo "$as" | awk -F ', ' 'NF >= 5 { print $2 "," $3 "," $4 - $2 "," $5 - $3 }')
            asname=${as#*, *, *, *, *, }
            if [ "${as%%,*}" = 1 ] && ! grep -qx "$asid" "$run/as-before"; then
                wid=$(wins | awk -F "$tab" -v b="$box" -v n="$asname" '$4 == b && $5 == n { c++; id = $1 } END { if (c == 1) print id }')
            fi ;;
    esac
    [ -n "$wid" ]
}

lost() {
    echo "$t: $1; AppleScript window: ${as:-none}; on screen: $(wins | tr "$tab" ' ' | paste -sd';' -)" >>"$notes"
}

# Brings the test window to the front and posts one input there; posts nothing when it cannot prove that window is in front.
step() {
    resolve || { lost "no test window"; return 1; }
    if [ "$t" = iterm ]; then osascript -e "tell application \"iTerm\" to select window id $asid" >/dev/null 2>&1 || true; fi
    "$post" raise "$wid" 2>"$run/err" || { lost "$(cat "$run/err")"; return 1; }
    sub=$1; shift
    "$post" "$sub" "$wid" "$@" 2>"$run/err" || { lost "$(cat "$run/err")"; return 1; }
}


# Posts each input (post's arguments without the window), then says ok, what arrived instead of $want, or nothing.
try() {
    want=$1; shift
    at=$(wc -c <"$log")
    n=0
    for keys in "$@"; do
        step ${keys%% *} ${keys#"${keys%% *}"} || { echo "focus lost"; return; }
        case $keys in click) n=$((n + 2)) ;; *) n=$((n + 1)) ;; esac
    done
    # As many lines as inputs or 2 s, then 0.3 s with nothing new: a late line must not land in the next step.
    i=0
    while [ "$(arrived "$at")" -lt $n ] && [ $i -lt 20 ]; do sleep 0.1; i=$((i + 1)); done
    i=0 seen=-1
    while [ "$(arrived "$at")" != "$seen" ] && [ $i -lt 10 ]; do seen=$(arrived "$at"); sleep 0.3; i=$((i + 1)); done
    got=$(since "$at" "$(wc -c <"$log")")
    if [ "$got" = "$want" ]; then echo ok; else echo "${got:-nothing}"; fi
}

[ $# -gt 0 ] || set -- ghostty kitty wezterm iterm
for t in "$@"; do
    case $t in
        ghostty) app=Ghostty pat=ghostty offer=offered exe=/Ghostty.app/Contents/MacOS/ ;;
        kitty) app=kitty pat=kitty offer=offered exe=/kitty.app/Contents/MacOS/ ;;
        wezterm) app=WezTerm pat=wezterm offer=offered exe=/WezTerm.app/Contents/MacOS/ ;;
        iterm) app=iTerm pat=iterm offer="not offered" exe= ;;
        *) echo "terminals: unknown terminal $t" >&2; exit 2 ;;
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
    c="/usr/bin/env HOME=$h NERD_SOCKET=$h/s.sock NERD_TRACE=1 $nerd"
    asid='' as='' pid='' targs=''
    if [ -n "$preset" ] && [ "$t" != iterm ]; then
        # the preset's own locations under the test home, never the real ones
        echo y | $clean -u XDG_CONFIG_HOME -u KITTY_CONFIG_DIRECTORY -u WEZTERM_CONFIG_FILE HOME="$h" "$nerd" --preset "$t" >"$run/$t-preset"
        case $t in
            ghostty) targs="--config-default-files=false --config-file=$h/.config/ghostty/config.ghostty" ;;
            kitty) targs="--config=$h/.config/kitty/kitty.conf" ;;
            wezterm) targs="--config-file $h/.wezterm.lua" ;;
        esac
    fi
    case $t in
        # `open -n` starts a process of its own. Nothing but -e and, with --preset, the config file: Ghostty reads any other argument as configuration.
        ghostty | kitty | wezterm)
            pgrep -f "$exe" >"$run/pids" || true
            $clean open -na "$app.app" --args $targs -e $c ;;
        iterm)
            open -ga "$app" && sleep 2
            osascript -e "tell application \"$app\" to id of windows" 2>/dev/null | tr -s ', ' '\n' >"$run/as-before" || true
            asid=$(osascript -e "tell application \"iTerm\" to id of (create window with default profile command \"$c\")") || true ;;
    esac
    i=0
    until grep -q ' attached: ' "$log" 2>/dev/null || [ $i -ge 30 ]; do sleep 0.5; i=$((i + 1)); done
    sleep 1
    if [ -n "$exe" ]; then
        # The instance `open -n` started is a new process of the app that launchd started; its helpers are its children.
        pid=$({ pgrep -f "$exe" || true; } | grep -vxFf "$run/pids" | while read -r p; do
            [ "$(ps -o ppid= -p "$p" | tr -d ' ')" = 1 ] && echo "$p"; done || true)
        if [ "$(echo "$pid" | grep -c . || true)" != 1 ]; then
            echo "$t: $(echo "$pid" | grep -c . || true) new $app instances, wanted one" >>"$notes"
            pid=
        fi
    fi
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
        if ! resolve; then
            cells="no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window${tab}no window"
            lost "no test window (a full-screen Space or a locked screen keeps windows off it)"
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
            others=
            if [ -n "$exe" ]; then others=$({ pgrep -f "$exe" || true; } | grep -xFf "$run/pids" | paste -sd' ' - || true); fi
            if [ "$input" = ok ] && [ -n "$others" ]; then
                # Events posted to one instance of an app can reach another instance of the same app, the user's own.
                no="not checked: another $app is running"
                cells="$no${tab}$no${tab}$no${tab}$no${tab}$no${tab}$no${tab}$no${tab}$no${tab}$no${tab}$no"
                echo "$t: another $app is running (pid $others), so no input was posted; quit it to check this row's input" >>"$notes"
            elif [ "$input" = ok ]; then
                cells="$(try "char 0x0" "key 0")${tab}$(try "char ALT" "key 0 option")"
                cells="$cells${tab}$(try "Char('c') CONTROL" "key 8 control")"
                # kitty reads Control and с as Control and c, the key under it: what a shortcut needs.
                cyr=$(try "Char('с') CONTROL" "key 8 control с")
                if [ "$cyr" = "Char('c') CONTROL" ]; then cyr="ok (as c)"; fi
                cells="$cells${tab}$cyr"
                cells="$cells${tab}$(try "Left 0x0 Right 0x0 Down 0x0 Up 0x0" "key 123" "key 124" "key 125" "key 126")"
                pbpaste >"$run/clipboard"
                printf %s "$clip" | pbcopy
                cells="$cells${tab}$(try "paste ${#clip} chars" "key 9 command")"
                pbcopy <"$run/clipboard" && mv "$run/clipboard" "$run/clipboard.restored"
                cells="$cells${tab}$(try "mouse Down(Left) 0x0 mouse Up(Left) 0x0" click)"
                case $layout in
                    com.apple.keylayout.US | com.apple.keylayout.ABC)
                        if [ -n "$preset" ] && [ "$t" = iterm ]; then
                            cells="$cells${tab}not checked: iTerm2 reads the real home"
                        else
                            post_shortcuts >"$run/$t-shortcuts"
                            cells="$cells${tab}$(shortcuts_cell "$t" <"$run/$t-shortcuts")"
                        fi
                        post_prefix latin >"$run/$t-prefix"
                        cells="$cells${tab}$(prefix_cell <"$run/$t-prefix")" ;;
                    *) cells="$cells${tab}not checked: the layout is ${layout#com.apple.keylayout.}, not U.S. or ABC${tab}not checked: not a Latin layout" ;;
                esac
                # a Russian keyboard, as the Control Cyrillic column posts it: each key with the character it prints there
                post_prefix russian >"$run/$t-prefix-russian"
                cells="$cells${tab}$(prefix_cell <"$run/$t-prefix-russian")"
            else
                cells="$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input${tab}$input"
            fi
            cells="$cells${tab}$colour"
        fi
        echo "$t${tab}ok${tab}$tp${tab}$gr${tab}$stray${tab}$cells${tab}$offer" >>"$table"
    fi
    server=$(sed -n 's/.*server\[\([0-9]*\)\].*/\1/p' "$log" 2>/dev/null | tail -1)
    $clean HOME="$h" NERD_SOCKET="$h/s.sock" "$nerd" --stop >/dev/null || true
    sleep 2
    if [ -n "$server" ] && ps -p "$server" >/dev/null; then echo "$t: server $server still running" >>"$notes"; fi
    # Only the instance `open -n` started for this run and its helpers.
    case $t in ghostty | kitty | wezterm)
        [ -z "$pid" ] || family "$pid" | while read -r p; do kill "$p" 2>/dev/null || true; done ;;
    # AppleScript's own id for the window it opened: it is not a window-server id.
    iterm) [ -z "$asid" ] || osascript -e "tell application \"$app\" to close window id $asid" >/dev/null 2>&1 || true ;;
    esac
    i=0
    while [ -n "$pid$asid" ] && resolve && [ $i -lt 10 ]; do sleep 0.5; i=$((i + 1)); done
    if [ -n "$pid$asid" ] && resolve; then echo "$t: window $wid left open" >>"$notes"; fi
done
echo "Terminal.app${tab}not checked: not a target" >>"$table"
echo "Alacritty${tab}not checked: its Homebrew cask is disabled (fails Gatekeeper)" >>"$table"
echo "Warp${tab}not checked: closed source, needs an account, has its own input editor" >>"$table"

column -t -s "$tab" "$table"
echo "Control Cyrillic and prefix, Russian: synthetic events carrying the Russian layout's characters, not a Russian layout in use; check them by hand on one."
cat "$notes"
if [ "$screen" = ok ]; then echo "screenshots: $shots"; fi
echo "logs: $run"
