#!/bin/sh
# Marks files over 1 MB: "big" on their tree row, a count on the pane's tab.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd
mkfifo to-nerd
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["pane_opened"]}}\n' "$NERD_PLUGIN" >&3
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            *'"event":"pane_opened"'*)
                pane=$(printf '%s\n' "$line" | sed -n 's/.*"pane":\([0-9]*\).*/\1/p')
                cwd=$(printf '%s\n' "$line" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')
                [ -n "$cwd" ] || continue
                # nerd's tree paths are canonical ("/private/tmp/x", not "/tmp/x"); match that.
                cwd=$(cd "$cwd" 2>/dev/null && pwd -P) || continue
                items=
                n=0
                oldifs=$IFS
                IFS='
'
                for f in $(find "$cwd" -maxdepth 3 -type f -size +1024k 2>/dev/null); do
                    items="$items{\"path\":\"$f\",\"text\":\"big\",\"role\":\"warn\"},"
                    n=$((n + 1))
                done
                IFS=$oldifs
                printf '{"Badges":{"target":"tree","items":[%s]}}\n' "${items%,}" >&3
                printf '{"Badges":{"target":"tab","items":[{"pane":%s,"text":"%s big","role":"warn"}]}}\n' "$pane" "$n" >&3
                ;;
        esac
    done
} 3> to-nerd
