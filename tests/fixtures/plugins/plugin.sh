#!/bin/sh
# Test plugin: subscribes to the events given as arguments, logs every line it hears, and on `probe` tries what its sandbox must refuse.
set -eu
here=$(pwd)
cd "$NERD_PLUGIN_DATA"
root=${NERD_PLUGIN_DATA%/plugin-data/*}
main=${NERD_SOCKET%.psock}.sock
echo $$ > pid.tmp; mv pid.tmp pid
if [ -e /dev/fd/3 ]; then echo open > fd3; else echo closed > fd3; fi
sleep 1000 &
echo $! > sleeper.tmp; mv sleeper.tmp sleeper
case ${1:-} in
    never-connect) exec sleep 1000 ;;
    exit-after-connect)
        # The socket stays open past the exit, so only the group kill ends what it left.
        { printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":[]}}\n' "$NERD_PLUGIN"; sleep 30; } | /usr/bin/nc -U "$NERD_SOCKET" > /dev/null &
        sleep 1
        exit 3 ;;
    drop-then-exit)
        # The other order: its connection dies first, then it exits.
        { printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":[]}}\n' "$NERD_PLUGIN"; sleep 30; } | /usr/bin/nc -U "$NERD_SOCKET" > /dev/null &
        nc=$!
        sleep 1
        kill -9 "$nc"
        sleep 1
        exit 3 ;;
esac
try() {
    what=$1
    shift
    r=0
    "$@" > /dev/null 2>&1 < /dev/null || r=$?
    echo "$what $r" >> results.log
}
probe() {
    try open /usr/bin/open -a TextEdit
    try launchctl /bin/launchctl submit -l "dev.nerd.probe.$$" -- /usr/bin/true
    try osascript /usr/bin/osascript -e 'return 1'
    try tcp /usr/bin/nc -z -w 2 1.1.1.1 80
    try main-socket /usr/bin/nc -w 2 -U "$main"
    try write-manifest /bin/sh -c "echo x >> '$here/plugin.json'"
    try write-outside /bin/sh -c "echo x > '$root/outside'"
    try read-ssh /bin/cat "$HOME/.ssh/id_test"
    try read-fish /bin/cat "$HOME/.local/share/fish/fish_history"
    try read-zsh-new /bin/cat "$HOME/.zsh_history.new"
    try read-link /bin/cat "$HOME/.local/share/fish/linked_history"
    try read-session /bin/cat "$root/session.json"
    try list-root /bin/ls "$root"
    try read-other /bin/cat "$root/plugin-data/other/secret"
    try write-data /bin/sh -c "echo x > '$NERD_PLUGIN_DATA/inside'"
    try read-own /bin/cat "$here/plugin.json"
    try read-home /bin/cat "$HOME/.zshrc"
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":[]}}\n' "$NERD_PLUGIN" | /usr/bin/nc -U "$NERD_SOCKET" > again.out 2>&1 || true
    # A detached grandchild: launchd is its parent now, and it is still inside the sandbox.
    ( (
        printf '"Shutdown"\n' | /usr/bin/nc -U "$NERD_SOCKET" > shutdown.out 2>&1 || true
        printf '{"Hello":{"cwd":"/","cols":80,"rows":24}}\n' | /usr/bin/nc -U "$NERD_SOCKET" > hello.out 2>&1 || true
        r=0
        /usr/bin/nc -w 2 -U "$main" < /dev/null > /dev/null 2>&1 || r=$?
        echo "$r" > detached.out
    ) & )
    echo done >> results.log
}
events=$(printf '"%s",' "$@")
rm -f to-nerd
mkfifo to-nerd
/usr/bin/nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":[%s]}}\n' "$NERD_PLUGIN" "${events%,}" >&3
    # Slot messages the test left in the data folder: send.jsonl at every start, once.jsonl at the first.
    if [ -f send.jsonl ]; then cat send.jsonl >&3; fi
    if [ -f once.jsonl ]; then mv once.jsonl sent.jsonl; cat sent.jsonl >&3; fi
    printf '{"ReportState":{"pane":1,"state":"working"}}\n' >&3
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            *'"id":"probe"'*) probe ;;
            # A preview answered with the lines the test left as preview.json, unless it left none.
            *'"event":"preview_request"'*) if [ -f preview.json ]; then req=${line#*'"req":'}; printf '{"Preview":{"req":%s,"lines":%s}}\n' "${req%%,*}" "$(cat preview.json)" >&3; fi ;;
            # A panel filled with what the test left as panel.jsonl, each time it is opened.
            *'"event":"panel_opened"'*) if [ -f panel.jsonl ]; then cat panel.jsonl >&3; fi ;;
            # A second connection of the same plugin, refused, whose report must not count.
            *'"event":"agent_state"'*) [ -e stale.out ] || printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":[]}}\n{"ReportState":{"pane":1,"state":"blocked"}}\n' "$NERD_PLUGIN" | /usr/bin/nc -U "$NERD_SOCKET" > stale.out 2>&1 || true ;;
        esac
    done
} 3> to-nerd
