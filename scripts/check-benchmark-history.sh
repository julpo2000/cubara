#!/usr/bin/env bash
# BENCHMARKS.md stays one history, not several.
#
# This check exists because of a measured failure. Merging #255 on top of #257
# and #259 hit a conflict in BENCHMARKS.md, and the resolution kept both sides:
# the file went from 1,549 to 3,049 lines, carrying two copies of the macOS and
# Linux tables, two copies of footnotes 1 through 51, a row cut in half, and two
# different footnotes both numbered 51 -- one of which claimed the arena had been
# doubled when it had not. Nothing noticed, because a docs merge builds fine and
# passes every test. A performance history that contradicts itself is worse than
# no history: the next person reads whichever copy they scroll to first.
#
# Three properties, all of which that merge broke:
#
#   1. Every footnote marker is DEFINED exactly once.
#   2. Every footnote a table row REFERENCES has a definition.
#   3. No section header appears twice.
#
# Numbering collisions are the same class of defect as the duplication: #235's
# Linux row reused footnote 45, which the "multiplayer played" note already
# owned, so the Linux row pointed at someone else's measurement.
#
set -euo pipefail

cd "$(dirname "$0")/.."

file=BENCHMARKS.md
fail=0

# Superscript digits are the footnote alphabet: ⁰¹²³⁴⁵⁶⁷⁸⁹.
sup='⁰¹²³⁴⁵⁶⁷⁸⁹'

# 1. A definition is a line that STARTS with a marker, then a space. Every
#    marker gets exactly one.
dup_defs=$(grep -oE "^[$sup]+ " "$file" | sort | uniq -d | tr -d ' ' || true)
if [ -n "$dup_defs" ]; then
    echo "FAIL: these footnotes are defined more than once in $file:"
    for marker in $dup_defs; do
        echo "      $marker —"
        grep -nE "^$marker " "$file" | sed 's/^/        /' | cut -c1-100
    done
    echo "      Two notes under one number means a row cites the wrong measurement."
    echo "      Give the newer one the next unused number."
    fail=1
fi

# 2. Every marker a dated table row cites must be defined. A row whose footnote
#    vanished in a merge is the other half of the same failure.
missing=""
refs=$(grep -E "^\| 20[0-9][0-9]-" "$file" \
    | grep -oE "[$sup]+" | sort -u || true)
for marker in $refs; do
    if ! grep -qE "^$marker " "$file"; then
        missing="$missing $marker"
    fi
done
if [ -n "$missing" ]; then
    echo "FAIL: table rows in $file cite footnotes that are not defined:$missing"
    echo "      Either the note was lost in a merge, or the row's number is wrong."
    fail=1
fi

# 3. Two identical headers is the shape a kept-both-sides merge leaves behind.
dup_headers=$(grep -E "^#{2,3} " "$file" | sort | uniq -d || true)
if [ -n "$dup_headers" ]; then
    echo "FAIL: duplicated section headers in $file:"
    echo "$dup_headers" | sed 's/^/      /'
    echo "      The file holds one copy of the history. Merge, do not append."
    fail=1
fi

if [ "$fail" -eq 0 ]; then
    echo "OK: $file is one history — footnotes unique, cited, sections not duplicated."
fi
exit "$fail"
