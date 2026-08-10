#!/usr/bin/env bash
# What to work on next.
#
# An autonomous session should never have to decide which piece of work comes
# next -- that is a judgement, and judgements drift. Ordering lives in the issue
# titles, which carry their ROADMAP.md block number, and this script reads it.
#
# Usage:  ./scripts/next-block.sh [phase]      (default: 1)
set -euo pipefail
cd "$(dirname "$0")/.."

phase="${1:-1}"
case "$phase" in
    1) milestone="Phase 1 — The engine stands" ;;
    2) milestone="Phase 2 — The first survival world" ;;
    3) milestone="Phase 3 — Modern depth, with synergy" ;;
    *) echo "unknown phase: $phase (expected 1, 2 or 3)" >&2; exit 2 ;;
esac

if ! command -v gh >/dev/null 2>&1; then
    echo "gh (GitHub CLI) is not installed -- it is how work is tracked here." >&2
    exit 2
fi

open=$(gh issue list --milestone "$milestone" --state open --limit 100 \
        --json number,title --jq '.[] | "\(.title)\t#\(.number)"' | sort -V)

if [ -z "$open" ]; then
    cat <<EOF
No open blocks left in "$milestone".

That is not the same as the phase being finished. Run the gate:

    ./scripts/check-phase-gate.sh $phase

on **both** machines, record the BENCHMARKS.md rows, and report to the project
owner. Do not start the next phase.
EOF
    exit 0
fi

next=$(printf '%s\n' "$open" | head -1)
rest=$(printf '%s\n' "$open" | tail -n +2)

cat <<EOF
Phase $phase — next block:

    ${next}

Before writing code:
  - ROADMAP.md, the phase $phase block table -- what this block is for
  - docs/PHASE1_ARCHITECTURE.md -- the decisions it implements, already made
  - the issue itself: its Design decisions section is binding, not advisory

Then: branch, implement, test, PR, wait for CI, merge. One block, one arc.

Remaining after this one:
EOF
printf '%s\n' "$rest" | sed 's/^/    /'
