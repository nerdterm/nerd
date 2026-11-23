#!/bin/sh
# List, tree and detail panels over the files of the pane they were opened from, no git needed. A file in the list
# opens in the same panel, with a row back to the list.
set -eu
cd "$NERD_PLUGIN_DATA"
rm -f to-nerd
mkfifo to-nerd
nc -U "$NERD_SOCKET" < to-nerd | {
    printf '{"Connect":{"protocol":2,"role":"plugin","plugin":"%s","events":["pane_opened","pane_cwd","panel_opened","panel_item"]}}\n' "$NERD_PLUGIN" >&3
    mkdir -p cwd
    cwd=
    # Text for a JSON string: no tab or other control character, at most 256 characters (a line over 512 cells
    # refuses the whole panel), then the two characters JSON escapes.
    esc() { printf '%s' "$1" | tr '\t' ' ' | tr -d '\000-\037' | LC_ALL=en_US.UTF-8 cut -c1-256 | sed 's/\\/\\\\/g; s/"/\\"/g'; }
    # A file whose name holds a quote or a backslash is left out: its path is also the item's id, which comes back in an event.
    plain() { case $1 in *'"'* | *'\'*) return 1 ;; esac; }
    list() {
        [ -n "$cwd" ] || { printf '{"Panel":{"id":"files","kind":"list","title":"Files here","items":[]}}\n' >&3; return; }
        items=
        oldifs=$IFS
        IFS='
'
        for f in $(find "$cwd" -maxdepth 1 -type f 2>/dev/null | sort); do
            plain "$f" || continue
            size=$(wc -c < "$f" 2>/dev/null | tr -d ' ')
            items="$items{\"id\":\"$f\",\"label\":\"$(esc "$(basename "$f")")\",\"detail\":\"${size}B\",\"opens\":true},"
        done
        IFS=$oldifs
        printf '{"Panel":{"id":"files","kind":"list","title":"Files here","items":[%s]}}\n' "${items%,}" >&3
    }
    tree() {
        [ -n "$cwd" ] || { printf '{"Panel":{"id":"types","kind":"tree","title":"By type","items":[]}}\n' >&3; return; }
        top=
        oldifs=$IFS
        IFS='
'
        exts=$(find "$cwd" -maxdepth 1 -type f 2>/dev/null | sed -n 's/.*\.\([^./]*\)$/\1/p' | sort -u)
        for e in $exts; do
            kids=
            for f in $(find "$cwd" -maxdepth 1 -type f -name "*.$e" 2>/dev/null | sort); do
                plain "$f" || continue
                kids="$kids{\"id\":\"$f\",\"label\":\"$(esc "$(basename "$f")")\",\"opens\":true},"
            done
            top="$top{\"id\":\"ext-$e\",\"label\":\".$e\",\"children\":[${kids%,}]},"
        done
        IFS=$oldifs
        printf '{"Panel":{"id":"types","kind":"tree","title":"By type","items":[%s]}}\n' "${top%,}" >&3
    }
    # A file's first lines, in the panel it was chosen in, with a row back to that panel's list.
    peek() {
        panel=$1
        path=$2
        name=$(esc "$(basename "$path")")
        lines=
        oldifs=$IFS
        IFS='
'
        for l in $(head -n 20 "$path" 2>/dev/null); do
            lines="$lines[{\"text\":\"$(esc "$l")\"}],"
        done
        IFS=$oldifs
        printf '{"Panel":{"id":"%s","kind":"detail","title":"%s","items":[{"id":"back","label":"Back to the files","opens":true}],"lines":[%s]}}\n' "$panel" "$name" "${lines%,}" >&3
    }
    while IFS= read -r line; do
        printf '%s\n' "$line" >> events.log
        case $line in
            # Each pane's folder, so a panel opened from a pane's menu lists that pane's files.
            *'"event":"pane_opened"'* | *'"event":"pane_cwd"'*)
                pane=$(printf '%s\n' "$line" | sed -n 's/.*"pane":\([0-9]*\).*/\1/p')
                printf '%s\n' "$line" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p' > "cwd/$pane"
                ;;
            *'"event":"panel_opened"'*)
                pane=$(printf '%s\n' "$line" | sed -n 's/.*"pane":\([0-9]*\).*/\1/p')
                cwd=
                [ -n "$pane" ] && [ -f "cwd/$pane" ] && cwd=$(cat "cwd/$pane")
                case $line in
                    *'"id":"files"'*) list ;;
                    *'"id":"types"'*) tree ;;
                esac
                ;;
            *'"event":"panel_item"'*)
                panel=$(printf '%s\n' "$line" | sed -n 's/.*"panel":"\([^"]*\)".*/\1/p')
                item=$(printf '%s\n' "$line" | sed -n 's/.*"item":"\([^"]*\)".*/\1/p')
                case $panel:$item in
                    files:back) list ;;
                    types:back) tree ;;
                    *) peek "$panel" "$item" ;;
                esac
                ;;
        esac
    done
} 3> to-nerd
