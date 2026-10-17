#!/bin/sh
# PR_TITLE/PR_BODY arrive via the workflow's env block, never ${{ }} inside run:
# pull request text is attacker-controlled.
regex=$(cat "$(dirname "$0")/subject.regex")
status=0
if ! printf '%s\n%s\n' "$PR_TITLE" "$PR_BODY" | grep -Eq '#[0-9]+'; then
    echo "pull request names no issue: put 'Closes #N' (or a plain '#N') in the body" >&2
    status=1
fi
if ! printf '%s\n' "$PR_TITLE" | grep -Eq "$regex"; then
    echo "pull request title is not a conventional subject (type(scope): text): '$PR_TITLE'" >&2
    status=1
fi
# Squash merge writes the body into master's history: a private Claude link points at what nobody else can open.
if printf '%s\n%s\n' "$PR_TITLE" "$PR_BODY" | grep -q -E 'claude\.ai/(code|artifact|chat|share|project)'; then
    echo "pull request holds a private Claude link: remove it from the title and body" >&2
    status=1
fi
exit $status
