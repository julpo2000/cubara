#!/usr/bin/env bash
# Mechanical enforcement of ARCHITECTURE.md.
#
# Every check here corresponds to a numbered rule. A rule without a check is a
# defect in ARCHITECTURE.md; a check that is disabled to make a build pass is a
# defect in judgement.
#
# Runs in CI as a required check.
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
SIM_CRATES="crates/voxel/src crates/world/src crates/sim/src crates/server/src"
# `crates/server/src/clock.rs` is the dedicated server's wall-clock -> tick
# boundary -- the headless equivalent of the client's `main.rs`
# (docs/PHASE1_ARCHITECTURE.md §9). Something has to turn seconds into ticks or
# a server would run the world as fast as the CPU allows. It is excluded **by
# name**, so a second file reaching for the clock fails here rather than
# quietly becoming precedent.
for d in $SIM_CRATES; do [ -d "$d" ] || continue
    grep -rn --include='*.rs' --exclude='clock.rs' -E "Instant::now|SystemTime::now" "$d"
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
# `server` is in this list for the reason Rule 4 exists: a dedicated server on a
# headless host must not need a GPU stack installed for a process that will
# never draw a pixel. `cubara-server` linking `wgpu` would not fail to compile —
# it would fail to *run*, on someone else's machine, months later.
for c in voxel world sim server; do [ -f "crates/$c/Cargo.toml" ] || continue
    grep -n -E "^(wgpu|winit|pollster)" "crates/$c/Cargo.toml" | sed "s|^|crates/$c/Cargo.toml:|"
done | report "Rule 3/4" "GPU/windowing dependency in a data or simulation crate"

for c in voxel world sim server; do [ -d "crates/$c/src" ] || continue
    grep -rn --include='*.rs' -E "\bwgpu::|\bwinit::" "crates/$c/src"
done | report "Rule 3/4" "GPU/windowing types in a data or simulation crate"

# The renderer renders. It does not own input or gameplay.
grep -n -E "pub fn (key_input|mouse_look|set_cursor_captured|edit_block)" crates/render/src/render.rs \
    | report "Rule 3" "input/gameplay on the renderer — if it can place a block, the boundary is wrong"

# The renderer's own dependency graph must not include the world -- its inputs
# are meshes, origins and a camera, never a `World` (§1 of the phase 1 design
# doc). `cubara-world` is a legitimate *dev*-dependency (golden-image tests
# build real scenes through it), so this scans only the `[dependencies]`
# table -- from that header to the next `[...]` one -- not the whole file,
# and prints real file line numbers (not the filtered stream's) via `NR`.
awk '
    /^\[dependencies\]/ { in_deps = 1; next }
    /^\[/ { in_deps = 0 }
    in_deps && /^cubara-world/ { print NR": "$0 }
' crates/render/Cargo.toml | sed 's|^|crates/render/Cargo.toml:|' \
    | report "Rule 3" "cubara-render depends on cubara-world — its inputs must stay meshes, origins and a camera"

if [ -s "$failures" ]; then
    echo
    echo "$(wc -l <"$failures" | tr -d ' ') architecture rule(s) violated."
    exit 1
fi
echo "OK: architecture rules hold."
