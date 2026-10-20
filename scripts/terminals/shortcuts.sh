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
# arrive as Option and the key. kitty and iTerm2 type Option's characters unless told to send Alt: that says so.
shortcuts_cell() {
    awk -F '\t' -v t="$1" '
        { n++; letter = $1 !~ /^shift\+/; want = letter ? "char ALT" : substr($1, 7) " SHIFT | ALT" }
        letter && $2 == "char ALT" { alt++ }
        letter && $2 == "char 0x0" { plain++ }
        $2 == want { ok++; next }
        { bad = bad "; " $1 ": " $2 }
        END {
            if (alt == 0 && plain > 0 && t == "kitty") { print "Option not sent as Alt: nerd --preset kitty sets it"; exit }
            if (alt == 0 && plain > 0 && t == "iterm") { print "Option not sent as Alt: nerd --preset iterm2 sets it"; exit }
            print "ok " ok + 0 "/" n bad
        }'
}

# The character a Russian layout prints on each shortcut's key; the same key's own where it prints that.
russian() {
    case $1 in
        K) echo л ;; M) echo ь ;; /) echo . ;; T) echo е ;; N) echo т ;; ,) echo б ;; C) echo с ;; W) echo ц ;;
        Z) echo я ;; S) echo ы ;; U) echo г ;; Q) echo й ;; *) echo "$1" ;;
    esac
}

# Posts Control + B, then each shortcut's key with neither Option nor Shift, Escape after each, and prints
# "name<tab>what arrived". With $1 = russian both carry the Russian layout's characters, as a Russian keyboard does.
post_prefix() {
    printf '%s\n' "$SHORTCUTS" | while read -r sc_name sc_code; do
        sc_at=$(wc -c <"$log")
        sc_got="focus lost"
        sc_b=и sc_key=$(russian "$sc_name")
        if [ "$1" != russian ]; then sc_b='' sc_key=''; fi
        case $sc_name in shift+*) sc_key='' ;; esac
        if step key 11 control $sc_b && step key "$sc_code" "" $sc_key; then
            sc_i=0
            while [ "$(arrived "$sc_at")" -lt 2 ] && [ $sc_i -lt 20 ]; do sleep 0.1; sc_i=$((sc_i + 1)); done
            sc_got=$(since "$sc_at" "$(wc -c <"$log")")
        fi
        sc_at=$(wc -c <"$log")
        step key 53 || true
        sc_i=0
        while [ "$(arrived "$sc_at")" -lt 1 ] && [ $sc_i -lt 10 ]; do sleep 0.1; sc_i=$((sc_i + 1)); done
        printf '%s\t%s\n' "$sc_name" "${sc_got:-nothing}"
    done
}

# Reads post_prefix's lines and says "ok N/N", or which did not arrive as Control + B and then the bare key.
# Control and the B key may come as b or as и: nerd reads both off the key.
prefix_cell() {
    awk -F '\t' '
        { n++; key = $1 ~ /^shift\+/ ? substr($1, 7) " 0x0" : "char 0x0" }
        $2 == "Char(\047b\047) CONTROL " key || $2 == "Char(\047и\047) CONTROL " key { ok++; next }
        { bad = bad "; " $1 ": " $2 }
        END { print "ok " ok + 0 "/" n bad }'
}
