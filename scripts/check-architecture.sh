#!/usr/bin/env bash
# Mechanical enforcement of ARCHITECTURE.md.
#
# Every check here corresponds to a numbered rule. A rule without a check is a
# defect in ARCHITECTURE.md; a check that is disabled to make a build pass is a
# defect in judgement.
#
# STATUS: this FAILS on the current tree, by design. It is the executable audit
# of what has to be demolished. Each violation below is owned by an issue, and the
# check is wired into CI by the PR that clears the last one.
set -uo pipefail
cd "$(dirname "$0")/.."

# A pipeline's last stage runs in a subshell, so a `fail=1` set inside `report`
# would be discarded. Record failures in a file instead.
failures=$(mktemp)
trap 'rm -f "$failures"' EXIT

report() { # report <rule> <message>; reads offenders on stdin
    local rule="$1" msg="$2" found
    found=$(cat)
    if [ -n "$found" ]; then
        echo "FAIL [$rule] $msg"
        echo "$found" | sed 's/^/       /'
        echo x >>"$failures"
    fi
}

# ── Rule 1 — deterministic simulation ────────────────────────────────────────
# The sim advances by tick, never by elapsed seconds. Wall-clock belongs to the
# renderer and the profiler. (SIM_CRATES grows as the sim lands.)
SIM_CRATES="crates/voxel/src crates/world/src"
for d in $SIM_CRATES; do [ -d "$d" ] || continue
    grep -rn --include='*.rs' -E "Instant::now|SystemTime::now" "$d"
done | report "Rule 1" "wall-clock time in a simulation crate — advance by tick instead"

for d in $SIM_CRATES; do [ -d "$d" ] || continue
    grep -rn --include='*.rs' -E "thread_rng|random\(\)" "$d"
done | report "Rule 1" "unseeded RNG in a simulation crate — RNG state belongs to the world"

# ── Rule 2 — no ambient state ────────────────────────────────────────────────
# A system that cannot be instantiated twice cannot be tested in isolation.
grep -rn --include='*.rs' -E "static +[A-Z_]+ *: *(OnceLock|Mutex|RwLock|LazyLock)|static mut |lazy_static" crates \
    | report "Rule 2" "global mutable state — pass state in, do not reach for it"

# ── Rule 3 / 4 — dependency direction, and the sim runs without a GPU ────────
# Data and simulation crates must not know the GPU exists.
for c in voxel world; do [ -f "crates/$c/Cargo.toml" ] || continue
    grep -n -E "^(wgpu|winit|pollster)" "crates/$c/Cargo.toml" | sed "s|^|crates/$c/Cargo.toml:|"
done | report "Rule 3/4" "GPU/windowing dependency in a data or simulation crate"

for c in voxel world; do [ -d "crates/$c/src" ] || continue
    grep -rn --include='*.rs' -E "\bwgpu::|\bwinit::" "crates/$c/src"
done | report "Rule 3/4" "GPU/windowing types in a data or simulation crate"

# The renderer renders. It does not own input or gameplay.
grep -n -E "pub fn (key_input|mouse_look|set_cursor_captured|edit_block)" crates/render/src/render.rs \
    | report "Rule 3" "input/gameplay on the renderer — if it can place a block, the boundary is wrong"

if [ -s "$failures" ]; then
    echo
    echo "$(wc -l <"$failures" | tr -d ' ') architecture rule(s) violated."
    exit 1
fi
echo "OK: architecture rules hold."
