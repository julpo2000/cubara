#!/usr/bin/env bash
# ROADMAP.md's exit gate, answered by a script instead of a judgement call.
#
# One line per criterion, PASS or FAIL, exactly the list under "Exit gate" for
# the given phase in ROADMAP.md -- no more, no fewer. A criterion whose test
# does not exist yet prints FAIL (not implemented); it never prints PASS and is
# never skipped. As each later phase-1 block lands its own test (determinism
# harness, golden images, save round-trip, ...), it should wire its criterion in
# here rather than leaving it a permanent "not implemented".
#
# Usage:  ./scripts/check-phase-gate.sh <phase>
set -uo pipefail
cd "$(dirname "$0")/.."

phase="${1:-1}"

pass=0
fail=0
out=$(mktemp)
trap 'rm -f "$out"' EXIT

# run <label> <cmd...> -- PASS if <cmd> exits 0, FAIL (with output) otherwise.
run() {
    local label="$1"
    shift
    if "$@" >"$out" 2>&1; then
        echo "PASS  $label"
        pass=$((pass + 1))
    else
        echo "FAIL  $label"
        sed 's/^/      /' "$out" | tail -20
        fail=$((fail + 1))
    fi
}

# not_implemented <label> -- always FAIL, explicitly, for a criterion whose test
# does not exist in the repo yet.
not_implemented() {
    echo "FAIL  $1 (not implemented)"
    fail=$((fail + 1))
}

if [ "$phase" != "1" ]; then
    echo "phase $phase's gate is not wired up yet -- only phase 1 is." >&2
    echo "(ROADMAP.md's phase $phase exit gate exists on paper; this script doesn't answer it yet.)" >&2
    exit 2
fi

echo "Phase 1 exit gate (ROADMAP.md) ------------------------------------------"
echo

run "cargo test --all" cargo test --all
run "cargo clippy --all-targets --all-features" cargo clippy --all-targets --all-features
run "cargo fmt --all --check" cargo fmt --all --check
run "architecture rules (check-architecture.sh)" ./scripts/check-architecture.sh
run "single render path (check-single-render-path.sh)" ./scripts/check-single-render-path.sh

# The perf criterion: parse --bench 64's SUMMARY line rather than trusting its
# own exit code (bench always exits 0 whether or not the gate FPS was met).
bench_out=$(cargo run --release -- --bench 64 2>&1)
summary=$(printf '%s\n' "$bench_out" | grep 'SUMMARY:' | tail -1)
fps=$(printf '%s\n' "$summary" | sed -nE 's/.*SUMMARY: ([0-9]+) FPS.*/\1/p')
if [ -n "$fps" ] && [ "$fps" -ge 1000 ]; then
    echo "PASS  --bench 64 reports >= 1000 FPS sustained ($fps FPS)"
    pass=$((pass + 1))
elif [ -n "$fps" ]; then
    echo "FAIL  --bench 64 reports >= 1000 FPS sustained (measured $fps FPS)"
    fail=$((fail + 1))
else
    echo "FAIL  --bench 64 reports >= 1000 FPS sustained (no SUMMARY line -- see output below)"
    printf '%s\n' "$bench_out" | tail -20 | sed 's/^/      /'
    fail=$((fail + 1))
fi

run "determinism replay test: single- vs multi-threaded, identical world-state hash" \
    cargo test -p cubara-sim --test determinism the_fixture_reaches_a_known_hash_regardless_of_worker_count
run "golden images: all three block types textured, a cave mouth, an LOD boundary" \
    cargo test -p cubara-render --test golden -- distinct_materials_render_with_distinct_textures a_cave_mouth_is_visible no_crack_at_a_real_lod_boundary
run "unit test: a player AABB never passes through a solid voxel" \
    cargo test -p cubara-sim --lib never_tunnels_through_the_floor_across_a_spread_of_velocities
run "unit test: the same seed produces a bit-identical chunk on both platforms" \
    cargo test -p cubara-world --lib cross_platform_block_array_is_bit_identical
run "isolation test: a chunk generated alone == the same chunk after shuffled neighbours" \
    cargo test -p cubara-world --lib generation_is_isolated_from_neighbours
# The "identically on Windows and macOS" half isn't this one command's job --
# it's the same fixture-hash test passing in CI on both windows-latest and
# macos-latest runners (every merged PR's required checks), not something a
# single local run can prove either way.
run "save round-trip test, plus a fixture world hashing identically on Windows and macOS" \
    cargo test -p cubara-sim --test save_load the_committed_fixture_loads_to_a_known_hash

echo
echo "$pass passed, $fail failed."
if [ "$fail" -gt 0 ]; then
    exit 1
fi
echo "OK: phase 1 exit gate met."
