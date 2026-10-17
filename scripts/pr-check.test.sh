#!/bin/sh
here=$(dirname "$0")
fail=0
n=0
case_() {
    n=$((n + 1))
    want=$3
    PR_TITLE="$1" PR_BODY="$2" sh "$here/pr-check.sh" >/dev/null 2>&1
    got=$?
    if [ "$got" -ne "$want" ]; then
        echo "case $n FAILED: title='$1' body='$2' want=$want got=$got" >&2
        fail=1
    fi
}
case_ 'fix: a thing' 'Closes #12' 0
case_ 'feat(tree): full screen' 'see #3 for the plan' 0
case_ 'chore(master): release 0.3.0' 'Release pull request, see #1' 0
case_ 'feat!: breaking' '#7' 0
case_ 'fix: number in title #9' '' 0
case_ 'docs: readme' 'Fixes #100 and #101' 0
case_ 'Revert "fix: a thing"' 'Closes #4' 0
case_ 'fix: a thing' '' 1
case_ 'fix: a thing' 'no number here' 1
case_ 'fix: a thing' 'issue 12' 1
case_ 'fixed stuff' 'Closes #12' 1
case_ 'Fix: capital type' 'Closes #12' 1
case_ 'fix:no space' 'Closes #12' 1
case_ 'wip' '#1' 1
case_ 'stuff' '' 1
link="https://claude.ai/code/session_$n" # privacy: test
case_ 'fix: a thing' "Closes #12

$link" 1
case_ "fix: a thing $link" 'Closes #12' 1
art="https://claude.ai/artifact/$n" # privacy: test
case_ 'fix: a thing' "Closes #12 see $art" 1
case_ 'feat: the bare claude.ai host' 'Closes #12' 0
if [ "$fail" -eq 0 ]; then
    echo "pr-check: $n cases pass"
else
    exit 1
fi
