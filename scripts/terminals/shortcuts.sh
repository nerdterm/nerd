#!/bin/sh
# The shortcuts column of scripts/terminals.sh: sourced there and by scripts/terminals.test.sh.

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
