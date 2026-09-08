#!/usr/bin/env bash
# Do the tests for this branch's new code actually notice when it breaks?
#
# This exists because of a measured, repeated failure. Over one day of work on
# phase 2, *six* separate tests were written that could not fail:
#
#   - a gate criterion that went green while multiplayer did not work
#   - a bandwidth criterion blind to players standing still
#   - a test that checked the state *before* the crash it was written for
#   - a registry fingerprint that was sent and never verified
#   - a prediction test that passed with the replay deleted
#   - a furnace test that gave both players the same item, so reading the
#     wrong one looked identical to reading the right one
#
# Every one was caught the same way: break the code on purpose, and see whether
# anything goes red. That worked, and it was a *habit* -- done by hand, ten
# times, remembered each time. `ARCHITECTURE.md` is explicit about what to do
# with a rule in that state:
#
#   > If a new rule has no answer to "what fails when someone breaks this?",
#   > write a check rather than a paragraph.
#
# So this is the habit, mechanised.
#
# HOW IT WORKS
#
# It takes the lines this branch *added* to `crates/*/src`, applies a small
# mutation to one of them, and runs that crate's tests. If they still pass, the
# mutation **survived**: nothing you wrote can see that line being wrong.
# Survivors are reported with the exact change that went unnoticed.
#
# WHAT IT IS NOT
#
# Not a coverage tool. Coverage says a line ran; this says a line *mattered*.
# The six failures above all had coverage -- they ran the code and asserted
# something true about the wrong thing.
#
# Not in CI, deliberately: it recompiles and re-tests once per mutation, which
# is minutes rather than seconds. It is a thing to run before opening a PR, like
# the benchmark, not a thing to run on every push.
#
# Not exhaustive, and it must not be read as a pass mark. A clean run means the
# mutations it *tried* were caught. A test suite can still be checking the wrong
# thing in a way no line edit reveals.
#
# Usage:
#   ./scripts/check-tests-can-fail.sh [base-ref] [max-mutations]
#
# Defaults: base `origin/main`, at most 8 mutations.
set -uo pipefail
cd "$(dirname "$0")/.."

base="${1:-origin/main}"
budget="${2:-8}"

if ! git rev-parse --verify --quiet "$base" >/dev/null; then
    echo "unknown base ref: $base" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# The mutations.
#
# Deliberately small and local: each turns one true thing into a different true
# thing that compiles. A mutation that does not compile proves nothing -- the
# compiler caught it, which is not what is being asked.
# ---------------------------------------------------------------------------
mutate_line() {
    # $1 = the source line. Echoes the mutated line, or nothing if no rule fits.
    local line="$1"
    case "$line" in
        # A guard that always lets through, or never does.
        *"if "*" {"*)
            if [[ "$line" == *"if !"* ]]; then
                echo "${line/if !/if false \&\& !}"
            else
                echo "${line/if /if true || }"
            fi
            return
            ;;
    esac
    # A comparison, flipped.
    case "$line" in
        *" < "*) echo "${line/ < / <= }"; return ;;
        *" > "*) echo "${line/ > / >= }"; return ;;
        *" == "*) echo "${line/ == / != }"; return ;;
        *" != "*) echo "${line/ != / == }"; return ;;
        *".max("*) echo "${line/.max(/.min(}"; return ;;
        *".min("*) echo "${line/.min(/.max(}"; return ;;
        *" + 1"*) echo "${line/ + 1/ + 2}"; return ;;
        *" - 1"*) echo "${line/ - 1/ - 2}"; return ;;
    esac
    echo ""
}

# The cargo package that owns a path under crates/<dir>/src.
package_of() {
    local dir
    dir="$(echo "$1" | cut -d/ -f1-2)"
    sed -n 's/^name *= *"\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1
}

# ---------------------------------------------------------------------------
# Collect candidates: lines this branch added, in order, so two runs on the
# same branch try the same mutations (Rule 1's habit, applied to the tooling).
# ---------------------------------------------------------------------------
candidates=()
current=""
lineno=0
while IFS= read -r diffline; do
    case "$diffline" in
        "+++ b/"*)
            current="${diffline#+++ b/}"
            ;;
        "@@"*)
            # @@ -a,b +c,d @@ -- c is the first new line number.
            lineno="$(echo "$diffline" | sed -n 's/^@@ [^+]*+\([0-9]*\).*/\1/p')"
            ;;
        "+"*)
            body="${diffline#+}"
            trimmed="${body#"${body%%[![:space:]]*}"}"
            case "$trimmed" in
                ""|"//"*|"///"*|"#["*|"}"*|"use "*) ;;
                *)
                    mutated="$(mutate_line "$body")"
                    if [ -n "$mutated" ] && [ "$mutated" != "$body" ]; then
                        candidates+=("$current	$lineno	$body	$mutated")
                    fi
                    ;;
            esac
            lineno=$((lineno + 1))
            ;;
        "-"*) ;;
        *) lineno=$((lineno + 1)) ;;
    esac
done < <(git diff --unified=0 "$base"...HEAD -- 'crates/*/src/*.rs')

total="${#candidates[@]}"
if [ "$total" -eq 0 ]; then
    echo "No mutable lines added under crates/*/src since $base — nothing to check."
    exit 0
fi

echo "Mutation check ---------------------------------------------------------"
echo
echo "  $total mutable lines added since $base; trying up to $budget of them."
echo "  A mutation that SURVIVES is a line your tests cannot see break."
echo

survivors=0
tried=0
step=$(( (total + budget - 1) / budget ))
[ "$step" -lt 1 ] && step=1

restore_all() { git checkout -- 'crates/*/src/*.rs' 2>/dev/null || true; }
trap restore_all EXIT INT TERM

i=0
while [ "$i" -lt "$total" ] && [ "$tried" -lt "$budget" ]; do
    IFS=$'\t' read -r file line before after <<<"${candidates[$i]}"
    i=$((i + step))

    [ -f "$file" ] || continue
    # The line must still be where the diff said it was.
    actual="$(sed -n "${line}p" "$file")"
    [ "$actual" = "$before" ] || continue

    pkg="$(package_of "$file")"
    [ -n "$pkg" ] || continue
    tried=$((tried + 1))

    python3 - "$file" "$line" "$after" <<'PY'
import io, sys
path, n, new = sys.argv[1], int(sys.argv[2]), sys.argv[3]
lines = io.open(path, encoding="utf-8").read().split("\n")
lines[n - 1] = new
io.open(path, "w", encoding="utf-8").write("\n".join(lines))
PY

    if RUSTFLAGS="-D warnings" cargo test -p "$pkg" --quiet >/dev/null 2>&1; then
        survivors=$((survivors + 1))
        echo "SURVIVED  $file:$line  ($pkg)"
        echo "    was: $(echo "$before" | sed 's/^[[:space:]]*//')"
        echo "    now: $(echo "$after" | sed 's/^[[:space:]]*//')"
        echo
    else
        echo "caught    $file:$line  ($pkg)"
    fi
    restore_all
done

echo
if [ "$survivors" -eq 0 ]; then
    echo "$tried mutations, all caught."
    echo
    echo "That is not a pass mark: it means the mutations tried were noticed."
    echo "A test can still assert the wrong thing in a way no line edit reveals."
    exit 0
fi

echo "$tried mutations, $survivors survived."
echo
echo "Each survivor is a line you changed that no test can see break. Either"
echo "write the assertion that would notice, or decide the line does not need"
echo "one -- and say which, rather than leaving it ambiguous."
exit 1
