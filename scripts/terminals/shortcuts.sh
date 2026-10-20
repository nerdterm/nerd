#!/bin/sh
# Reading the trace, and the shortcuts column, for scripts/terminals.sh: sourced there and by scripts/terminals.test.sh.

# Input trace lines from byte $1 to byte $2 of the log, shortened: "trace key char KeyModifiers(ALT)" reads "char ALT".
since() {
    # macOS head refuses a count of 0: nothing arrived, nothing to read.
    [ "$2" -gt "$1" ] || return 0
    tail -c +$(($1 + 1)) "$log" | head -c $(($2 - $1)) | sed -n 's/.*trace \(key \)\{0,1\}//p' | grep -v '^frames' | sed 's/KeyModifiers(\([^)]*\))/\1/' | paste -sd' ' -
}

arrived() { tail -c +$(($1 + 1)) "$log" | grep -c 'trace \(key\|paste\|mouse\)' || true; }

# Every bound Option shortcut of the Keys table: its name and the U.S. key code that types it.
SHORTCUTS='K 40
M 46
/ 44
T 17
N 45
, 43
C 8
W 13
- 27
\ 42
Z 6
S 1
U 32
Q 12
1 18
2 19
3 20
4 21
5 23
6 22
7 26
8 28
9 25
shift+Left 123
shift+Right 124
shift+Up 126
shift+Down 125'

# Posts every shortcut with the caller's `step` and `$log`, Escape after each, and prints "name<tab>what arrived".
# Its variables start with sc_: `step` finds the window again on every call and sets plain names of its own.
post_shortcuts() {
    printf '%s\n' "$SHORTCUTS" | while read -r sc_name sc_code; do
        case $sc_name in shift+*) sc_mods=option,shift ;; *) sc_mods=option ;; esac
        sc_at=$(wc -c <"$log")
        sc_got="focus lost"
        if step key "$sc_code" "$sc_mods"; then
            sc_i=0
            while [ "$(arrived "$sc_at")" -lt 1 ] && [ $sc_i -lt 20 ]; do sleep 0.1; sc_i=$((sc_i + 1)); done
            sc_got=$(since "$sc_at" "$(wc -c <"$log")")
        fi
        # Escape closes what the shortcut opened, and ends a dead key; the next one starts once it has arrived.
        sc_at=$(wc -c <"$log")
        step key 53 || true
        sc_i=0
        while [ "$(arrived "$sc_at")" -lt 1 ] && [ $sc_i -lt 10 ]; do sleep 0.1; sc_i=$((sc_i + 1)); done
        printf '%s\t%s\n' "$sc_name" "${sc_got:-nothing}"
    done
}

# Reads "name<tab>what the trace showed" per shortcut posted for terminal $1 and says "ok N/N", or which did not
# arrive as Option and the key. In kitty and iTerm2 Option + N and U are dead keys on U.S. and say so.
shortcuts_cell() {
    awk -F '\t' -v t="$1" '
        { n++; want = ($1 ~ /^shift\+/) ? substr($1, 7) " SHIFT | ALT" : "char ALT" }
        $2 == want { ok++; next }
        (t == "kitty" || t == "iterm") && ($1 == "N" || $1 == "U") && $2 == "nothing" { dead = dead (dead ? ", " : "") $1; next }
        { bad = bad "; " $1 ": " $2 }
        END {
            out = "ok " ok + 0 "/" n
            if (dead) out = out ", dead key (menu): " dead
            print out bad
        }'
}
