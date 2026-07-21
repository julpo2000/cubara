#!/usr/bin/env bash
# Enforces CONTRIBUTING.md: "One scene, one render path".
#
# There must be exactly ONE implementation that encodes the scene. The window,
# --bench and --screenshot are thin callers of it. When those paths forked, the
# text overlay and the block-outline landed in the window path only, and
# --screenshot silently stopped rendering what the game renders — which made it
# useless as verification. A paragraph did not prevent that; this check does.
#
# Adding another CALLER is fine. Adding a second scene-render IMPLEMENTATION is
# what fails here.
#
set -euo pipefail

cd "$(dirname "$0")/.."

fail=0

# The scene-render entry point is defined once, in the render crate.
def_count=$({ grep -rn "pub fn encode_scene" crates/render/src --include=*.rs || true; } | wc -l | tr -d ' ')
if [ "$def_count" -ne 1 ]; then
    echo "FAIL: expected exactly 1 definition of \`encode_scene\`, found $def_count."
    grep -rn "pub fn encode_scene" crates/render/src --include=*.rs || true
    fail=1
fi

# Nobody outside that one function may build the scene pipeline themselves.
# `build_pipeline` is the tell: three call sites is how the paths forked before.
offenders=$(grep -rn "build_pipeline(" crates --include=*.rs \
    | grep -v "crates/render/src/scene.rs" \
    | grep -v "pub fn build_pipeline" || true)
if [ -n "$offenders" ]; then
    echo "FAIL: \`build_pipeline\` called outside crates/render/src/scene.rs."
    echo "      Call the shared scene-render path instead of standing up your own."
    echo "$offenders"
    fail=1
fi

# Each render pass belongs to the shared path, so a new feature reaches every
# caller at once. A begin_render_pass outside the render crate means a fork.
stray_pass=$(grep -rn "begin_render_pass" crates --include=*.rs \
    | grep -v "^crates/render/src/scene.rs" || true)
if [ -n "$stray_pass" ]; then
    echo "FAIL: \`begin_render_pass\` outside crates/render/src/scene.rs."
    echo "      The scene is encoded in one place; callers pass a target, not a pass."
    echo "$stray_pass"
    fail=1
fi

if [ "$fail" -eq 0 ]; then
    echo "OK: single scene-render path intact."
fi
exit "$fail"
