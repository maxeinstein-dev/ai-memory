#!/usr/bin/env bash
# Fail unless `.github/workflows/ci.yml` runs on push to `alfama/main`.
#
# GitHub Actions only lets a pull request restore caches saved on its own
# ref, its base branch or the default branch. Upstream's `ci.yml` runs on
# push to `main` only, so in this fork no run ever saves a cache on
# `alfama/main` (the base of every fork PR) and every PR compiles the whole
# workspace cold (~9 min for `test`, ~8 min for `release-build`).
#
# The fix is one line in `ci.yml`, which a rebase onto upstream can silently
# drop when a conflict in that file is resolved with upstream's side. This
# check lives in its own workflow (`alfama-guard.yml`), a file upstream does
# not have, so the rebase that drops the line cannot drop the guard too.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CI_YML="${1:-$REPO_ROOT/.github/workflows/ci.yml}"
REQUIRED_BRANCH="alfama/main"

if [[ ! -f "$CI_YML" ]]; then
    echo "check-fork-ci: $CI_YML not found" >&2
    exit 2
fi

# Collect the branches listed under the top-level `on:` -> `push:` ->
# `branches:` key, in either flow (`[a, b]`) or block (`- a`) style.
branches="$(awk '
    function strip(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); gsub(/^["\x27]|["\x27]$/, "", s); return s }
    /^[^ \t#]/ { in_on = ($0 ~ /^"?on"?:/); in_push = 0; in_br = 0; next }
    !in_on { next }
    /^  [^ \t#]/ { in_push = ($0 ~ /^  push:/); in_br = 0; next }
    !in_push { next }
    /^    [^ \t#]/ {
        in_br = 0
        if ($0 ~ /^    branches:/) {
            rest = $0; sub(/^    branches:[ \t]*/, "", rest); sub(/[ \t]+#.*$/, "", rest)
            if (rest ~ /^\[/) {
                gsub(/[\[\]]/, "", rest); n = split(rest, parts, ",")
                for (i = 1; i <= n; i++) print strip(parts[i])
            } else { in_br = 1 }
        }
        next
    }
    in_br && /^[ \t]+- / { item = $0; sub(/^[ \t]+- /, "", item); sub(/[ \t]+#.*$/, "", item); print strip(item) }
' "$CI_YML")"

if grep -qxF "$REQUIRED_BRANCH" <<<"$branches"; then
    echo "check-fork-ci: ci.yml runs on push to $REQUIRED_BRANCH"
    exit 0
fi

echo "check-fork-ci: $CI_YML no longer runs on push to $REQUIRED_BRANCH." >&2
echo "  push branches found: ${branches:-<none>}" >&2
echo "  Without it every fork PR builds with a cold cache. Re-add" >&2
echo "  \`$REQUIRED_BRANCH\` to \`on.push.branches\` (probably dropped while" >&2
echo "  resolving a rebase conflict with upstream)." >&2
exit 1
