#!/bin/sh
# Previews .log files as only their ERROR and WARN lines, coloured by which they are.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd
mkfifo to-nerd
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["preview_request"]}}\n' "$NERD_PLUGIN" >&3
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            *'"event":"preview_request"'*)
                req=$(printf '%s\n' "$line" | sed -n 's/.*"req":\([0-9]*\).*/\1/p')
                path=$(printf '%s\n' "$line" | sed -n 's/.*"path":"\([^"]*\)".*/\1/p')
                rows=$(printf '%s\n' "$line" | sed -n 's/.*"rows":\([0-9]*\).*/\1/p')
                [ "${rows:-0}" -gt 0 ] 2>/dev/null || rows=20
                total=$(grep -cE 'ERROR|WARN' "$path" 2>/dev/null || true)
                lines=
                oldifs=$IFS
                IFS='
'
                for h in $(grep -E 'ERROR|WARN' "$path" 2>/dev/null | head -n "$rows"); do
                    role=warn
                    case $h in *ERROR*) role=bad ;; esac
                    esc=$(printf '%s' "$h" | sed 's/\\/\\\\/g; s/"/\\"/g')
                    lines="$lines[{\"text\":\"$esc\",\"role\":\"$role\"}],"
                done
                IFS=$oldifs
                cut=false
                [ "${total:-0}" -gt "$rows" ] 2>/dev/null && cut=true
                printf '{"Preview":{"req":%s,"lines":[%s],"cut":%s}}\n' "$req" "${lines%,}" "$cut" >&3
                ;;
        esac
    done
} 3> to-nerd
