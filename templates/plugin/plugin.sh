#!/bin/sh
# A minimal nerd plugin: it connects, subscribes, and answers its menu command.
# nerd starts it on first use, from its own folder, with NERD_SOCKET, NERD_PLUGIN and NERD_PLUGIN_DATA set.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd
mkfifo to-nerd
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["command","pane_opened","pane_closed"]}}\n' "$NERD_PLUGIN" >&3
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            refused:*) exit 1 ;;
            *'"event":"command"'*'"id":"hello"'*) echo "hello, said $NERD_PLUGIN" >> events.log ;;
        esac
    done
} 3> to-nerd
