#!/bin/sh
# Marks files over 1 MB: "big" on their tree row, a count on each pane's tab.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd tree.* tab.*
mkfifo to-nerd
# A Badges message replaces everything this plugin showed on that target, so every pane's items go out each time.
send() {
    printf '{"Badges":{"target":"tree","items":[%s]}}\n' "$(cat tree.* 2>/dev/null | sort -u | head -n 5000 | paste -sd, -)" >&3
    printf '{"Badges":{"target":"tab","items":[%s]}}\n' "$(cat tab.* 2>/dev/null | paste -sd, -)" >&3
}
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["pane_opened","pane_closed"]}}\n' "$NERD_PLUGIN" >&3
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            *'"event":"pane_opened"'*)
                pane=$(printf '%s\n' "$line" | sed -n 's/.*"pane":\([0-9]*\).*/\1/p')
                cwd=$(printf '%s\n' "$line" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')
                [ -n "$pane" ] && [ -n "$cwd" ] || continue
                # nerd's tree paths are canonical ("/private/tmp/x", not "/tmp/x"); match that.
                cwd=$(cd "$cwd" 2>/dev/null && pwd -P) || continue
                : > "tree.$pane"
                n=0
                oldifs=$IFS
                IFS='
'
                for f in $(find "$cwd" -maxdepth 3 -type f -size +1024k 2>/dev/null); do
                    # A name with a quote or a backslash would need escaping in JSON: left out, to keep this short.
                    case $f in *'"'* | *'\'*) continue ;; esac
                    printf '{"path":"%s","text":"big","role":"warn"}\n' "$f" >> "tree.$pane"
                    n=$((n + 1))
                done
                IFS=$oldifs
                printf '{"pane":%s,"text":"%s big","role":"warn"}\n' "$pane" "$n" > "tab.$pane"
                send
                ;;
            *'"event":"pane_closed"'*)
                pane=$(printf '%s\n' "$line" | sed -n 's/.*"pane":\([0-9]*\).*/\1/p')
                [ -n "$pane" ] || continue
                rm -f "tree.$pane" "tab.$pane"
                send
                ;;
        esac
    done
} 3> to-nerd
