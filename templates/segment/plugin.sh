#!/bin/sh
# A clickable status segment: the current time, refreshed by its own command.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd
mkfifo to-nerd
send_time() { printf '{"Segment":{"id":"clock","text":"%s","role":"text","command":"refresh"}}\n' "$(date +%H:%M)" >&3; }
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["command"]}}\n' "$NERD_PLUGIN" >&3
    send_time
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            *'"event":"command"'*'"id":"refresh"'*) send_time ;;
        esac
    done
} 3> to-nerd
