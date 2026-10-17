#!/bin/sh
# privacy.sh and privacy-hook.sh against hits planted in a throwaway repository. Every hit is built at run
# time, so this file holds none of them.
set -u
here=$(cd "$(dirname "$0")" && pwd)
p="$here/privacy.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
cd "$work" || exit 1
git init -q
printf 'private@example.org\nsecret-project\n' > .git/info/privacy
fail=0
n=0

# want, label, command...
expect() {
    want=$1 label=$2
    shift 2
    n=$((n + 1))
    "$@" > out.txt 2>&1
    got=$?
    [ "$got" -eq "$want" ] || { echo "privacy case $n FAILED: $label: want $want, got $got" >&2; sed 's/^/    /' out.txt >&2; fail=1; }
}
rep() { printf "%0${2}d" 0 | tr 0 "$1"; }
tree_with() { printf '%s\n' "$1" > f.txt && git add f.txt && expect "$2" "tree: $3" sh "$p" tree; }
text_with() { printf '%s\n' "$1" > in.txt && expect "$2" "text: $3" sh -c 'sh "$1" text < in.txt' - "$p"; }
hook_with() { printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$1" > in.json && expect "$2" "hook: $3" sh -c 'sh "$1" < in.json' - "$here/privacy-hook.sh"; }
link="https://claude.ai/code/sess""ion_01abc"
artifact="https://claude.ai/arti""fact/0123abcd"
# A real file named by an absolute path under this account's home folder.
home_body="/Users/$(id -un)/../..$work/body.md"

tree_with 'nothing to see here' 0 clean
tree_with "-----BEGIN RSA PRIV""ATE KEY-----" 1 'private key'
tree_with "token ghp_$(rep a 30)" 1 'GitHub token'
for k in o u s r; do tree_with "token gh${k}_$(rep a 30)" 1 "GitHub token gh${k}_"; done
tree_with "token github_pat_$(rep b 30)" 1 'GitHub fine-grained token'
tree_with "key sk-ant-$(rep c 30)" 1 'Anthropic key'
tree_with "key AKIA$(rep D 16)" 1 'AWS key'
tree_with "token xoxb-$(rep 1 12)" 1 'Slack token'
tree_with "see $link" 1 'session link'
tree_with "see $artifact" 1 'artifact link'
tree_with '"hosts": ["api.anthropic.com", "claude.ai"]' 0 'the bare claude.ai host'
tree_with "// pony""tail: a shortcut" 1 'old marker'
tree_with "/Users/$(id -un)/dev" 1 'home path'
tree_with "token ghp_$(rep a 30) // privacy"": test" 0 'exempt line'
tree_with 'the secret-project plan' 1 'private name'
tree_with 'SECRET-PROJECT' 1 'private name, any case'
tree_with 'the secret-projects' 0 'private name inside a longer word'
tree_with 'mail private@example.org' 1 'private address'
mv .git/info/privacy list
tree_with 'the secret-project plan' 0 'no list'
grep -q 'private-name check skipped: no info/privacy' out.txt || { echo "privacy: no list, no note" >&2; fail=1; }
mv list .git/info/privacy

printf 'clean\n' > f.txt
git add f.txt && git commit -q -m 'chore: clean'
clean=$(git rev-parse HEAD)
expect 0 'push: clean commit' sh "$p" push "$clean"
GIT_AUTHOR_EMAIL=private@example.org git commit -q --allow-empty -m 'chore: author'
expect 1 'push: private author' sh "$p" push HEAD
expect 0 'push: nothing new' sh "$p" push HEAD --not HEAD
GIT_COMMITTER_EMAIL=Private@Example.org git commit -q --allow-empty -m 'chore: committer'
expect 1 'push: private committer, any case' sh "$p" push HEAD --not HEAD~1
git commit -q --allow-empty -m 'chore: link' -m "$link"
expect 1 'push: session link in the message' sh "$p" push HEAD --not HEAD~1
git commit -q --allow-empty -m 'chore: on top'
git replace --graft HEAD
expect 1 'push: a graft does not hide the commits under it' sh "$p" push HEAD --not "$clean"
git replace -d HEAD > /dev/null
GIT_COMMITTER_EMAIL=private@example.org git tag -a -m 'v1' t1 "$clean"
expect 1 'push: private tagger' sh "$p" push "$(git rev-parse t1)"
git tag -a -m 'v2' t2 "$clean"
expect 0 'push: clean tag' sh "$p" push "$(git rev-parse t2)"
printf '\000\001' > b.bin
git add b.bin && git commit -q -m 'chore: binary'
expect 0 'push: a binary is listed, not refused' sh "$p" push HEAD --not HEAD~1
grep -q 'check by eye: b.bin' out.txt || { echo "privacy: binary not listed" >&2; fail=1; }

text_with 'a plain body' 0 clean
text_with "see $link" 1 'session link'
text_with 'about secret-project' 1 'private name'
text_with "see $artifact" 1 'artifact link'
text_with "see $link // privacy"": test" 1 'the test marker exempts only tree'

hook_with "gh pr create --title x --body 'see $link'" 2 'session link in a gh call'
hook_with "echo $link" 2 'session link in any call'
hook_with "echo $artifact" 2 'artifact link in any call'
hook_with "gh pr create --title x --body 'about secret-project'" 2 'name in a gh call'
hook_with "gh pr create --title x --body fine" 0 'clean gh call'
hook_with "grep secret-project notes" 0 'a name outside a call that posts'
printf 'about secret-project\n' > body.md
hook_with "gh issue comment 1 --body-file body.md" 2 'name in a posted file'
hook_with "gh api repos/o/r/issues/1/comments -F body=@body.md" 2 'name in a posted file, gh api'
printf 'fine\n' > body.md
hook_with "gh issue comment 1 --body-file body.md" 0 'clean posted file'
hook_with "gh issue comment 1 --body-file $home_body" 0 'clean posted file under the home folder'
hook_with "/Users/$(id -un)/x/close-issue.sh 1 --no-close --body-file $home_body" 0 'close-issue.sh by its path under the home folder'
printf 'see %s\n' "$artifact" > body.md
hook_with "gh issue comment 1 --body-file $home_body" 2 'link in a posted file under the home folder'
hook_with "/Users/$(id -un)/x/close-issue.sh 1 --no-close --body-file $home_body" 2 'link in a file close-issue.sh posts'

if [ "$fail" -eq 0 ]; then
    echo "privacy: $n cases pass"
else
    exit 1
fi
