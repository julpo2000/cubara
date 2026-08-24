# Cubara — Performance History

A per-feature record of `cargo run --release -- --bench` (see [`README.md`](README.md)),
so we can **keep optimizing**: every feature that lands gets a row, and we watch
how FPS and CPU-per-frame move as the scene grows. The M1 gate from
[`PLAN.md`](PLAN.md) (**1000+ FPS** in the benchmark scene) is now just a
trailing tag on each run — the point of this file is the *trend*, not a pass/fail.

**Reading the numbers.** At small scenes the frame is *submit-bound* (dominated
by pipeline/submit overhead, not the GPU), so **FPS is noisy** — repeated runs on
the same build can swing by several thousand. Until scenes get heavy enough to be
CPU- or GPU-bound, **`CPU/frame` is the more reliable signal to optimize against**;
raw FPS becomes meaningful once a feature makes the scene genuinely heavier. Chunk
and triangle counts are recorded per row because they drift as worldgen changes.

## How to record a run

```bash
cargo run --release -- --bench
```

The run ends with a `SUMMARY:` line (FPS, CPU/frame avg + p99, chunks, gate).
Add a row to the history table **for the machine it actually ran on**, with the
milestone/feature and the commit (`git rev-parse --short HEAD`) — the two tables
are different hardware, so a row in the wrong one turns a machine difference into
a phantom regression or speedup (this happened once; see footnote ³⁰). A session
records a row for whichever machine it is on; the other machine's row lands when
the work is next run there.

## Performance history

FPS is the sustained pipelined throughput; CPU/frame is the per-frame CPU submit
cost (the stabler metric — see above). All runs are 1920×1080, 2000 measured
frames after 200 warmup.

### Windows 11 — i7-12650H / RTX 4060 Laptop GPU (Vulkan)

| Date | Milestone / feature | Chunks | Tris | FPS | CPU/frame avg | CPU/frame p99 | Commit |
|---|---|---|---|---|---|---|---|
| 2026-07-18 | M2 — frustum culling (baseline) | 137 | 22,788 | 8097 | 0.083 ms | 0.350 ms | `0ab6034` |
| 2026-07-18 | M3 — streaming foundation (no scene change) | 137 | 22,788 | ~11,100¹ | 0.077 ms | ~0.29 ms | `7a249d2` |
| 2026-07-19 | M3 — streaming renderer (heavy scene) | 1,349 | 217,550 | ~1,980² | ~0.49 ms | ~1.16 ms | `ae0ebea` |
| 2026-08-13 | Block 1.10 complete — node-tree closeout / phase-gate verification [#38], radius 64³⁰ | 1,585 | 829,608 | ~3,300 | 0.126 ms | ~0.49 ms | `851e639` |
| 2026-08-24 | **Phase 1 exit gate — 12/12 on Windows** [#38], radius 12³¹ | 957 | 367,026 | ~4,888 | **0.088 ms** | ~0.39 ms | `e60e9c2` |
| 2026-08-24 | **Phase 1 exit gate — 12/12 on Windows** [#38], radius 64³¹ | **1,585** | 829,608 | **~3,579** | **0.102 ms** | ~0.38 ms | `e60e9c2` |
| 2026-08-24 | Skirt no longer overlaps a real face [#125], radius 12³² | 957 | **331,510** | ~4,771 | 0.089 ms | ~0.42 ms | `3af8d3b` |
| 2026-08-24 | **Skirt no longer overlaps a real face** [#125], radius 64³² | 1,585 | **758,754** | ~3,665 | 0.112 ms | ~0.45 ms | `3af8d3b` |

### macOS — Apple M3, 8 GB (integrated GPU, Metal)

| Date | Milestone / feature | Chunks | Tris | FPS | CPU/frame avg | CPU/frame p99 | Commit |
|---|---|---|---|---|---|---|---|
| 2026-07-18 | M2 — frustum culling (baseline) | 137 | 22,788 | 9242 | 0.070 ms | 0.246 ms | `c6921e9` |
| 2026-07-19 | M3 — streaming renderer (heavy scene) | 1,349 | 217,550 | ~2,860³ | 0.317 ms | 0.599 ms | `8b5467e` |
| 2026-07-19 | **M3.5 — chunk arena + `multi_draw_indirect`** [#27] | 1,349 | 217,550 | ~3,330³ | **0.199 ms** | 0.535 ms | `41e38f5` |
| 2026-07-19 | M4 — ambient occlusion [#45] | 1,349 | 361,326 | ~1,900⁴ | 0.363 ms | ~0.78 ms | `4086db1` |
| 2026-07-19 | **M4 — distance LOD streaming** [#39] | 1,182 | 46,920 | ~8,500⁵ | 0.083 ms | 0.23 ms | `4229513` |
| 2026-07-20 | M4 — LOD retuned: 12-chunk full-res core, radius 28⁶ | 6,561 | 538,846 | ~1,450 | 0.49 ms | ~1.0 ms | `0f65a49` |
| 2026-07-21 | Rule 2 — world state owned, not global⁷ | 1,349 | 361,326 | ~1,780 | 0.388 ms | ~1.0 ms | `refactor/world-owned-state` |
| 2026-07-21 | Rule 5 — one scene-render path⁸ | 1,349 | 361,326 | ~1,875 | 0.372 ms | ~0.87 ms | `refactor/single-scene-render-path` |
| 2026-07-21 | **Rule 3 — renderer renders only; all rules green**⁹ | 1,349 | 361,326 | ~1,920 | **0.361 ms** | ~0.90 ms | `refactor/renderer-renders-only` |
| 2026-07-22 | Deterministic draw order (BTreeMap) [#81]¹⁰ | 1,349 | 361,326 | ~1,865 | 0.364 ms | ~0.92 ms | `fix/deterministic-draw-order` |
| 2026-08-10 | **Radius-64 baseline — the phase 1 gate, first measured** [#89]¹¹ | 25,131 | 762,516 | ~996¹¹ | 0.715 ms | ~1.14 ms | `49146ef` |
| 2026-08-10 | `BlockId` + per-chunk palette compression [#46]¹² | 25,131 | 762,516 | ~991 | 0.720 ms | ~1.23 ms | `174a2ce` |
| 2026-08-10 | Packed vertex + texture array [#43], radius 12 — `first_instance` mechanism, superseded¹³ | 1,349 | 361,326 | ~2,050 | 0.328 ms | ~0.96 ms | *(superseded, not merged)* |
| 2026-08-10 | Packed vertex + texture array [#43], radius 64 — `first_instance` mechanism, superseded¹³ | 25,131 | 762,516 | ~728 | 1.317 ms | ~1.49 ms | *(superseded, not merged)* |
| 2026-08-11 | **Packed vertex + texture array, node_index-in-vertex (final)** [#43], radius 12¹⁴ | 1,349 | 361,326 | ~2,460 | **0.273 ms** | ~0.57 ms | `218eb41` |
| 2026-08-11 | **Packed vertex + texture array, node_index-in-vertex (final)** [#43], radius 64¹⁴ | 25,131 | 762,516 | ~1,016 | **0.697 ms** | ~1.05 ms | `218eb41` |
| 2026-08-11 | Per-face material appearance [#44], radius 12¹⁵ | 1,349 | 361,326 | ~2,452 | 0.275 ms | ~0.56 ms | `a13200d` |
| 2026-08-11 | Per-face material appearance [#44], radius 64¹⁵ | 25,131 | 762,516 | ~1,016 | 0.707 ms | ~1.31 ms | `a13200d` |
| 2026-08-11 | **The three phase-1 materials + textures, depth-layered terrain** [#55], radius 12¹⁶ | 1,349 | 439,816 | ~2,012 | 0.329 ms | ~0.81 ms | `9174d84` |
| 2026-08-11 | **The three phase-1 materials + textures, depth-layered terrain** [#55], radius 64¹⁶ | 25,131 | 899,840 | ~877 | 0.821 ms | ~2.06 ms | `9174d84` |
| 2026-08-11 | **Seeded noise terrain with caves** [#48], radius 12¹⁷ | 1,282 | 424,352 | ~2,224 | 0.302 ms | ~0.72 ms | `c67086c` |
| 2026-08-11 | **Seeded noise terrain with caves** [#48], radius 64¹⁷ | 26,789 | 890,774 | ~891 | 0.780 ms | ~1.51 ms | `c67086c` |
| 2026-08-11 | Fixed-timestep tick loop + world RNG [#57], radius 12¹⁸ | 1,282 | 424,352 | ~2,014 | 0.313 ms | ~1.54 ms | `1283002` |
| 2026-08-11 | Fixed-timestep tick loop + world RNG [#57], radius 64¹⁸ | 26,789 | 890,774 | ~903 | 0.783 ms | ~1.61 ms | `1283002` |
| 2026-08-11 | Player AABB collision, gravity and walking [#53], radius 12¹⁹ | 1,282 | 424,352 | ~2,221 | 0.301 ms | ~0.71 ms | `3ba4c2d` |
| 2026-08-11 | Player AABB collision, gravity and walking [#53], radius 64¹⁹ | 26,789 | 890,774 | ~898 | 0.785 ms | ~1.43 ms | `3ba4c2d` |
| 2026-08-11 | Selected-block outline [#52], radius 12²⁰ | 1,282 | 424,352 | ~2,194 | 0.304 ms | ~0.81 ms | `1d16c5d` |
| 2026-08-11 | Selected-block outline [#52], radius 64²⁰ | 26,789 | 890,774 | ~905 | 0.774 ms | ~1.55 ms | `1d16c5d` |
| 2026-08-11 | Determinism harness [#90], radius 12²¹ | 1,282 | 424,352 | ~2,173 | 0.307 ms | ~0.96 ms | `71d739d` |
| 2026-08-11 | Determinism harness [#90], radius 64²¹ | 26,789 | 890,774 | ~903 | 0.771 ms | ~1.82 ms | `71d739d` |
| 2026-08-11 | Save/load — regions + world header [#60], radius 12²² | 1,282 | 424,352 | ~2,215 | 0.303 ms | ~0.69 ms | `41a1152` |
| 2026-08-11 | Save/load — regions + world header [#60], radius 64²² | 26,789 | 890,774 | ~896 | 0.785 ms | ~1.60 ms | `41a1152` |
| 2026-08-11 | Node addressing + streaming policy [#105], radius 12²³ | 1,282 | 424,352 | ~2,249 | 0.300 ms | ~0.66 ms | `44070c1` |
| 2026-08-11 | LOD-native node generation [#106], radius 12²⁴ | 1,282 | 424,352 | ~2,211 | 0.303 ms | ~0.75 ms | `a463f2a` |
| 2026-08-11 | **Node meshing on the worker pool, one mesh per node** [#107], radius 12²⁵ | 690 | 250,982 | ~3,217 | **0.192 ms** | ~0.73 ms | `444045a` |
| 2026-08-11 | **Node meshing on the worker pool, one mesh per node** [#107], radius 64²⁵ | 1,238 | 625,258 | **~1,673** | **0.408 ms** | ~1.01 ms | `444045a` |
| 2026-08-11 | Skirts to hide LOD seams [#108], radius 12²⁶ | 690 | 281,274 | ~2,920 | 0.214 ms | ~0.90 ms | `d518f53` |
| 2026-08-11 | Skirts to hide LOD seams [#108], radius 64²⁶ | 1,238 | 689,436 | ~1,558 | 0.444 ms | ~0.89 ms | `d518f53` |
| 2026-08-11 | **Ring schedule tuned to the <2,000-draw budget** [#109], radius 12²⁷ | 957 | 367,026 | ~2,357 | 0.280 ms | ~0.88 ms | `1e84478` |
| 2026-08-11 | **Ring schedule tuned to the <2,000-draw budget** [#109], radius 64²⁷ | **1,585** | 829,608 | ~1,295 | 0.526 ms | ~1.12 ms | `1e84478` |
| 2026-08-11 | `cubara-render` drops its `cubara-world` dependency [#110], radius 12²⁸ | 957 | 367,026 | ~2,402 | 0.265 ms | ~1.10 ms | `0fe7e7d` |
| 2026-08-11 | `cubara-render` drops its `cubara-world` dependency [#110], radius 64²⁸ | 1,585 | 829,608 | ~1,291 | 0.531 ms | ~1.11 ms | `0fe7e7d` |
| 2026-08-11 | Arena capacity re-sized for the node tree [#111], radius 12²⁹ | 957 | 367,026 | ~2,474 | 0.270 ms | ~0.92 ms | `da55704` |
| 2026-08-11 | Arena capacity re-sized for the node tree [#111], radius 64²⁹ | 1,585 | 829,608 | ~1,275 | 0.535 ms | ~1.50 ms | `da55704` |

¹ FPS at this scene is submit-bound and noisy. 4 back-to-back runs on `7a249d2`
climbed **monotonically 9,732 → 10,471 → 11,719 → 13,657 FPS** — not random
scatter but CPU/GPU clock ramp: the 200-frame warmup (~20 ms at these rates) ends
long before boost clocks settle, and each launch inherits a warmer GPU from the
last, so successive runs aren't independent samples. CPU/frame stayed tight at
0.065–0.083 ms throughout. The M3 foundation is behaviour-unchanged (same
137-chunk scene), so this is a same-scene re-baseline, not a real speedup; treat
CPU/frame as the comparable number, and take first-run-after-idle FPS over a
warmed-up burst when comparing across features.

³ **M3.5 Step 1 — draw-call collapse.** Same 1,349-chunk scene, same machine,
measured back-to-back on `8b5467e` (one draw call per chunk) vs `41e38f5` (all
geometry in a shared arena, drawn with **one** `multi_draw_indexed_indirect`). The
draw list goes from ~1,349 submits to a single indirect one, so the per-frame CPU
submit cost drops **0.317 → 0.199 ms (~37%)** — the reliable signal here — and FPS
rises ~2,860 → ~3,330 (3 runs each spanned 2,856–2,871 and 3,230–3,421). CPU/frame
is now dominated by the CPU frustum cull (still ~1,322 AABB tests/frame writing the
indirect list), which is exactly what **#28** moves onto the GPU next. Both figures
are tight (±<1% and ±~3%) because the scene is bound by real work, not pipeline
noise. The arena's high-water mark on this scene is 435k/4M vertices and 653k/6M
indices — ample headroom, negligible fragmentation.

⁴ **Ambient occlusion — a visual feature, not a perf one.** Baking per-vertex AO
means AO-varying cells can no longer greedy-merge, so the same 1,349-chunk scene
goes **217,550 → 361,326 triangles (~+66%)** and vertices grow 24→28 bytes. The
frame is now **GPU-bound** on that heavier mesh, so FPS drops ~3,330 → ~1,900 (3
runs 1,849–1,940) — still ~1.9× the 1000-FPS gate. CPU/frame rises to ~0.36 ms too,
but that's mostly back-pressure (the CPU stalls in `submit` once the GPU is the
bottleneck), not extra CPU work. A worthwhile trade for the depth AO adds; triangle
count is a lever LOD (#37–#40) and denser-mesh optimizations can pull back later.

⁵ **Distance LOD — render distance for cheap.** Each chunk is now meshed at a LOD
chosen by its distance from the camera (`streaming::lod_for`), so at radius 12 most
chunks are coarse: **361,326 → 46,920 triangles (~87% fewer)** and FPS jumps ~1,900
→ ~8,500. The real point is scaling — with LOD the same M3 sustains far larger
radii (via `--bench <radius>`):

| radius | chunks | tris | FPS | CPU/frame |
|---|---|---|---|---|
| 12 | 1,182 | 46,920 | ~8,500 | 0.083 ms |
| 24 | 3,627 | — | ~5,900 | 0.149 ms |
| 32 | 6,094 | — | ~3,685 | 0.234 ms |

Radius 32 draws **5× the chunks** of the full-res radius-12 scene yet still runs
~2× faster than it did (~1,900 FPS). Chunk count dips vs full-res (1,349 → 1,182 at
r12) because majority-downsampling drops sparse far features. LOD boundaries show
small cracks for now — seam fixing is #40.

⁶ **LOD retuned for looks.** The first pass coarsened after 3 chunks (48 blocks),
so detail visibly popped up close. Reworked so the whole 12-chunk (192-block) core
stays full-resolution and LOD only kicks in beyond it, with STREAM_RADIUS pushed to
28 (448-block horizon) for the rings to fill. This row is the live-representative
scene (radius 28): 6,561 chunks / 539k tris at ~1,450 FPS on M3 — heavier than the
aggressive-LOD row above (the core is now genuinely full-res), still above the gate,
and the detailed core follows the camera so nearby pop-in is gone.

⁷ **Architecture Rule 2, measured as flat.** The world's edit overlay moved from a
global `OnceLock<RwLock<HashMap>>` to owned data on a `World` value, with meshing
jobs carrying an `Arc<World>` snapshot instead of workers reading shared state.
Compared same-machine, back-to-back, against `a4de4b3` rather than against the
older row (which was recorded on a different day): baseline **0.375 / 0.383 /
0.364 ms** (mean 0.374) vs **0.388 / 0.389 / 0.388 ms** (mean 0.388) — +3.7%, at
the edge of this machine's run-to-run band, and the bench's per-frame loop does
not touch `World` at all (it is read once at scene construction). Recorded as
flat; the row exists so the claim is checkable rather than asserted.

⁸ **One render path, and the noise question settled.** The window, `--bench` and
`--screenshot` had three separate copies of pipeline + camera + depth + render
pass; they now all call `SceneRenderer::encode_scene`. Runs: **0.374 / 0.381 /
0.361 ms** (mean 0.372) — level with the `a4de4b3` baseline (mean 0.374) and
*below* the row above, which retroactively confirms that row's +3.7% was
run-to-run noise rather than a cost of owning world state. The bench gained a
function call per frame and lost nothing else.

⁹ **Boundaries cost nothing.** World, camera, input handling and block editing
moved off `Renderer` into `app::Game`; the renderer now receives what it draws.
Runs: **0.362 / 0.359 / 0.362 ms** (mean 0.361) — the best of the refactor
series and level with the `a4de4b3` baseline (0.374). Passing two references per
frame instead of reading owned fields is free, which is worth recording: the
architecture work has now been measured four times and has not cost a
millisecond. All of `scripts/check-architecture.sh` and
`scripts/check-single-render-path.sh` pass, and both are required CI checks as
of this commit.

¹⁰ **Determinism is free here.** `ChunkArena.slots` moved from `HashMap` to
`BTreeMap` so the per-frame indirect draw list is built in `ChunkCoord` order
(#81). The concern was that this is a hot loop — ~1,349 entries iterated every
frame — and `BTreeMap` iteration is pointer-chasing where `HashMap` is not.
Measured same-machine A/B: baseline **0.365 / 0.365 / 0.367** (median 0.365) vs
**0.360 / 0.367 / 0.364 / 0.375 / 0.364 / 0.364** (median 0.364). No measurable
cost — the iteration is trivial next to the frustum test and the buffer write.
Two early branch samples of 0.374/0.397 were thermal outliers, which is why nine
runs were taken rather than three; a 3-sample read here would have reported a
false 3% regression.

² **First meaningful FPS number.** The streaming renderer measures a ~1,350-chunk
region (10× the old grid), which pushes the frame into being **CPU-submit-bound**:
one draw call per chunk (~1,322 drawn after culling) dominates at ~0.5 ms/frame.
Because it's now bound by real work rather than pipeline overhead, FPS is far
tighter — 4 runs spanned **1,836–2,082 FPS** (±~6% vs the ±40% of the 137-chunk
rows). This is *not* comparable to the rows above (different, much heavier scene) —
it's the new baseline to optimize down from. The obvious next lever is the draw-call
count: batching chunks into fewer draws (instanced / indirect / GPU-driven) should
move this number, and it'll show up right here.

¹¹ **The radius-64 baseline (issue #89) — the gate is red, and now for a legible
reason.** `PHASE1_ARCHITECTURE.md` §2 derived its whole design budget from
radius-12 numbers and flagged its triangle ceiling as an estimate; nobody had
run `--bench 64` before this. It does not hang or crash — it settles in ~1s and
prints a `SUMMARY:` line — but it exposes exactly the resource §2 predicted
would run out first: **`MAX_DRAWS` (16,384), not vertex/index memory.** The
region streams **25,131** resident chunks (762,516 triangles, 1,525,032
vertices, 2,287,548 indices — comfortably inside the 4M-vertex/6M-index arena),
but `ChunkArena::prepare` caps the per-frame visible set at `MAX_DRAWS`, so
every measured frame draws exactly **16,384/25,131** chunks and the rest are
silently dropped from whichever frame's visible set overflows first (now
reported explicitly — see `ArenaUsage::exhausted` in
`crates/render/src/arena.rs`, added in this PR). FPS lands right on the 1000
line and swings with it: 9 back-to-back runs spanned **986–1,006 FPS** (median
996) while CPU/frame stayed tight at 0.710–0.719 ms (mean 0.715) — the familiar
submit-bound noise pattern (see¹), not a real margin either way. **Gate: NOT
MET**, and per this issue's scope, this PR does not attempt to raise it — it
measures and reports. §6's design already anticipated this: today's per-chunk
draws (one draw per resident chunk) cannot reach the ≤~2,000-draw radius-64
budget no matter how far LOD downsamples triangle counts, because draw count
scales with chunk count, not triangle count. 25,131 resident chunks is **~12.6×**
that budget; block 1.10's region node tree (one draw per 2^L³-chunk node) is
what's meant to close this gap, not this block. The CI smoke test added in this
PR (`crates/world/tests/radius_64_smoke.rs`) reproduces the same streamed
region without a GPU and asserts it settles within a 120s bound and stays under
the arena's vertex/index capacities — the substitute for the perf gate on
GPU-less CI runners, per this issue's design decisions.

¹² **`BlockId` + palette compression — memory down ~94%, one-time generation up
~5-6×, draw path untouched.** `Chunk` moved from one `bool` per voxel (a flat
4096-byte `Vec<bool>`, allocated for *every* chunk regardless of content) to
`ChunkStorage`: `Uniform(BlockId)` with no allocation at all, or `Palette` — a
small id table plus a packed index per voxel at the narrowest width that fits
(1/2/4/8/16 bits). This row's scene is bit-for-bit the same geometry as the
row above (25,131 chunks, 762,516 triangles, identical golden images) because
representation is orthogonal to what gets drawn — so **FPS and CPU/frame are
unchanged** (~991 FPS / 0.720 ms vs ~996 FPS / 0.715 ms, within the same
noise band as¹) — this row only touches the one-time region-build step
(`World::chunk_at` → `Chunk::from_fn`), not the per-frame draw loop.

**Memory, measured directly** over the radius-64 region's 49,923 candidate
chunk coordinates: 26,833 stay `Uniform` (**0 bytes** each — mostly the fully-
air chunks above the terrain, plus fully-solid ones fully underground) and
23,090 promote to `Palette` (516 bytes each at 1-bit packing — 2 distinct ids
in phase 1). Total chunk-storage bytes: **11.9 MB, down from 195 MB** the old
flat representation would have cost for the same set (**94.2% reduction**) —
this is exactly the radius-64 memory budget `docs/PHASE1_ARCHITECTURE.md` §2/§4
named this block as load-bearing for.

**Generation time, measured directly** (`Chunk::from_fn`/`from_solid_fn` alone,
same real terrain closure, no meshing): **old ~70-95 ms → new ~420 ms** for the
same 49,923 chunks (~5-6×). Root cause, found by splitting the pipeline: it is
*not* the palette bookkeeping itself (a synthetic worst case — every one of
4096 cells a new value — costs only ~10 µs/chunk, ~480 ms total for all 49,923).
The first version of this routed every cell through the promote/repack state
machine (`ChunkStorage::set`) even for a chunk that never leaves `Uniform`,
which cost **10×** on its own (fixed by splitting `set` into a tiny `#[inline]`
fast path and an `#[inline(never)]` cold path — a large function with a rare
slow branch was silently blocking inlining of the common no-op case). The
remaining ~5-6× came from routing the *sampling* loop itself (which calls back
into real worldgen — trig, not free) through that same per-cell state machine;
restructuring `Chunk::from_fn` to sample into a flat buffer first and build the
final `ChunkStorage` in one pass (`ChunkStorage::from_ids`, no incremental
promotion/repack) let the sampling loop optimise the same way the old flat
`Vec<bool>` fill did — isolated, it now costs ~97 ms, matching the old
baseline. What's left is genuinely the palette-building pass, paid once per
chunk at load time, not per frame: the CI smoke test (`radius_64_smoke.rs`)
settles in ~1.3-1.4 s (was ~250-270 ms), still under 1% of its 120 s bound.
This block's scope was the representation change, not chasing generation speed
further; a future block is free to revisit if chunk-load time becomes the
binding cost somewhere.

¹³ **Superseded by¹⁴ — the mechanism this footnote describes did not survive
CI.** These two rows were measured against a real commit in this PR's history
that requested `wgpu::Features::INDIRECT_FIRST_INSTANCE` and used
`@builtin(instance_index)` to look up each chunk's world origin, which worked
correctly *on this M3* — the numbers below are honest measurements of that
build, not fabricated. What turned out not to hold up is the "confirmed on
both backends" claim: CI later found `multi_draw_indexed_indirect` +
`first_instance` broken on Windows' software DX12 adapter, and the
`draw_indexed` fallback broken on macOS CI's own *virtualized* Metal adapter
(not the same thing as this real M3) — see `docs/PHASE1_ARCHITECTURE.md` §5.3
for the full investigation. The rows are kept rather than deleted, per this
file's own rule of recording the trend rather than only the final state; the
commit they were measured against was never merged, so there's nothing to look
up for the hash. Superseded by the vertex-embedded `node_index` rows below,
which is the actual shipped mechanism.

**Packed vertex + texture array (issue #43) — smaller and faster at radius
12, slower at radius 64, and both are real.** `Vertex` moved from
`position: [f32;3], normal: [f32;3], ao: f32` (28 bytes, world-space) to two
packed `u32`s (8 bytes, node-local — `docs/PHASE1_ARCHITECTURE.md` §5.2): a
**71% cut in vertex bytes**, and at the arena's fixed 4M-vertex capacity that's
the §2 budget itself — 112 MB down to the targeted **32 MB**. Placing a chunk
moved from a CPU-side `Mesh::translate` to a GPU-side per-node origin add,
read via `@builtin(instance_index)` off each draw's `first_instance`
(`INDIRECT_FIRST_INSTANCE`). Worked immediately on Metal; the first Windows
CI run showed every chunk piled at one origin (`first_instance` silently
ignored) because the feature was never in `required_features` at device
creation -- `docs/PHASE1_ARCHITECTURE.md` §5.3 has the full story. One-line
fix (request `INDIRECT_FIRST_INSTANCE` explicitly), confirmed on both
backends afterward by golden images matching their reference **exactly**
(0.0000% differing pixels; a wrong node index shows geometry at the wrong
world position, not just a colour difference, so this is a real correctness
check, not a coincidence). The fragment shader also gained a real
`texture_2d_array` sample, replacing the flat green constant.

Two real, opposite deltas, both measured back-to-back on identical scenes:

- **Radius 12** (1,349 chunks, 361,326 tris) — **faster**: ~1,865 → ~2,050
  FPS, CPU/frame **0.364 → 0.328 ms (-10%)**, against the last radius-12 row
  (footnote 10). Smaller vertices mean less GPU vertex-fetch bandwidth, and at
  this scene size that's the effect that shows up.
- **Radius 64** (25,131 chunks, 762,516 tris, still `MAX_DRAWS`-capped per
  #89) — **slower**: ~991 → ~728 FPS, CPU/frame **0.720 → 1.317 ms (+83%)**,
  against the previous row. The believable cause: at radius 64 the camera
  frame is essentially wall-to-wall geometry, so the fragment shader runs on
  close to every pixel of a 1920×1080 frame, and it now does a real texture
  fetch instead of a flat multiply -- work that was simply absent before.
  Vertex bandwidth improved at both scales; fragment cost is new at both
  scales, but only dominates once pixel coverage is this high.

The gate was already **NOT MET** at radius 64 before this PR (see the block-
1.0 baseline); it's still not met, with a wider margin. That's not something
this block was scoped to fix — texturing has to sample *something* once it's
real (per-face material selection is #44, next), and 1.10's region node tree
is what's actually meant to close radius 64's draw-count gap (§6). Recorded
here so the fragment-side cost is a data point on the table rather than a
surprise discovered later: worth a profiler pass if it's still the binding
constraint once #44/#55 land real art and 1.10 changes what "resident
geometry" means.

¹⁴ **The node_index-in-vertex fallback (§5.3) — the mechanism that actually
shipped, and it measures faster than the abandoned one at both scales.**
`Vertex` grew a third packed `u32` (12 bytes total, not the 8 or the
originally-hoped-for 10 — WebGPU requires a 4-byte-aligned stride, so a 16-bit
node index costs a full word) carrying the arena `node_index` directly,
resolved in `mesh.wgsl` as a plain vertex read instead of any
instance-indexing mechanism. `first_instance` is now always `0` and the
`draw_indexed` fallback's instance range is always `0..1` — both kept only
because `multi_draw_indexed_indirect` is still worth having for collapsing
draw calls, independent of how a vertex finds its origin.

Three back-to-back runs each, same M3, same scenes as¹³:

| radius | FPS (3 runs) | CPU/frame avg (3 runs) |
|---|---|---|
| 12 | 2,436 / 2,476 / 2,481 | 0.273 / 0.276 / 0.271 ms |
| 64 | 1,017 / 1,012 / 1,019 | 0.698 / 0.696 / 0.698 ms |

Both tighter and faster than¹³'s numbers: radius 12 **0.328 → ~0.273 ms
(-17%)**, radius 64 **1.317 → ~0.697 ms (-47%)**. This is a real change (the
band is tight — ±2% at both scales — not overlapping noise), but the two
builds differ in more than just the origin-lookup mechanism (¹³'s build
requested `INDIRECT_FIRST_INSTANCE`, wrote `first_instance` per draw, and was
measured in an earlier session at a different thermal/clock state), so the
full mechanism is not isolated here — recorded honestly as "faster, cause not
fully separated" rather than attributed to a specific saving. What *is* clear:
moving `node_index` into vertex data was not a performance tax for the
correctness it buys — if anything the opposite showed up. Vertex memory is
48 MB at the fixed 4M-vertex capacity (§2), not the 32 MB originally targeted;
that budget was never the binding constraint at any measured scene (draws
are), so the deviation is paid for without a measured cost.

¹⁵ **Per-face material appearance (issue #44) — flat within noise, as
expected.** The mesher now resolves each quad's texture via
`registry.texture_for_face(block, face)` instead of one name per block id, and
the shader is unchanged (still one `texture_2d_array` sample per fragment,
just now reading a layer that can vary by face instead of only by block). The
extra work is a `HashMap` lookup plus a six-way match, paid once per quad at
mesh-build time on a worker thread -- not in the per-frame draw path this
table measures -- so no shift was expected, and none showed up: radius 12
**0.273 → 0.275 ms** and radius 64 **0.697 → 0.707 ms**, both inside the
run-to-run noise band established in¹⁴ (±2%). Recorded to keep the "every
feature is measured" rule honest, not because a delta was anticipated.

**A real, pre-existing bug surfaced by this block, not introduced by it:**
`World::chunk_at` filled every solid voxel with the hardcoded constant
`BlockId::STONE` (`BlockId(1)`), left over from before the block registry
existed (block 1.3, issue #54). Block ids are assigned by sorted material
name (§3.4) -- and in the real `assets/blocks` registry, `"cubara:grass"`
sorts before `"cubara:soil"` and `"cubara:stone"`, so id 1 is actually
**grass**, not stone. Block 1.4a's single-texture-per-block-id resolution
couldn't reveal this (every face of "id 1" got grass's *top* texture
uniformly, which read as an oddly-olive but otherwise unremarkable flat
colour); per-face resolution immediately did, because grass is a `Sided`
material -- the terrain rendered with a visibly mottled tan/blue pattern
where AO-darkened slopes happened to pick up `grass_side`'s blue-ish
placeholder colour depending on which of the six directions each quad faced.
Fixed by having `World::chunk_at` take its solid id as a parameter
(`chunk_at(coord, solid: BlockId)`), resolved by the caller from its actual
loaded registry (`registry.id_of("cubara:stone")`) instead of a hardcoded
number -- consistent with §3.4's own rule that consumers must never assume a
specific numeric id. `terrain.png` and `materials.png` are both re-blessed:
`terrain.png` because the whole scene is stone and now correctly renders
stone's tan-gold placeholder colour instead of grass's olive one everywhere;
`materials.png` because its grass chunk (which *was* the real `cubara:grass`
id, resolved by name, not the buggy constant) now shows a visibly different
colour on its side face (`grass_side`, blue-ish) than its top
(`grass_top`, olive) -- the actual point of this block, now visible in the
reference image. Both changes were inspected by eye before blessing.

**Why the golden coverage stops at top vs. side, not top vs. side vs.
bottom in one frame:** top and bottom are opposite faces of a convex block,
so no single camera position can ever see both at once, with or without a
custom camera -- back-face culling removes whichever one faces away. Proving
the bottom resolves correctly needed a deterministic check instead of a
screenshot: `cubara_voxel::voxel::tests::sided_material_gives_each_face_its_own_layer`
meshes an isolated `Sided` block and asserts each of the six emitted quads'
`tex_layer` against the exact face it should carry (`PosY` → top, `NegY` →
bottom, the four horizontal directions → side) -- strictly more precise than
a pixel-diff against a flat placeholder colour could be, and it doesn't need
a new camera-override mechanism in the shared headless render path to get
there.

¹⁶ **Real art and depth-layered terrain (issue #55) — slower, and it's the
same cause as footnote 4's AO jump: more triangles, not more expensive
ones.** Two independent changes land together: `materials::build` loads the
four real 16×16 PNGs from `assets/textures/` instead of a flat placeholder
colour per name, and `World::chunk_at` stamps unedited terrain by depth
below the surface (`cubara:grass` at the surface, `cubara:soil` for
`SOIL_DEPTH` (3) blocks under it, `cubara:stone` below that) instead of one
material everywhere. The first is texture-sampling cost, unchanged in shape
from block 1.4b (still one `texture_2d_array` sample per fragment); the
second is what actually moves the numbers, because `MaskCell` merge
equality already requires the same block id (block 1.4a), so a
material-layer boundary splits a greedy-merged quad exactly like an AO
discontinuity does. Radius 12: **361,326 → 439,816 triangles (+22%)**,
FPS ~2,452 → ~2,012, CPU/frame **0.275 → 0.329 ms (+20%)** -- proportional
to the triangle increase, not a new per-triangle cost. Radius 64: **762,516
→ 899,840 triangles (+18%)**, FPS ~1,016 → ~877 (dropping the bench's
generic 1000-FPS tag to NOT MET, which is not the same thing as the formal
radius-64 exit gate from issue #89 -- that gate is bound by `MAX_DRAWS`, was
already NOT MET before this row, and stays exactly as NOT MET, unmoved by a
triangle-count change), CPU/frame **0.697 → 0.821 ms (+18%)**. Both deltas
track their triangle-count deltas closely, which is the tell that this is
real layered geometry doing real work, not a regression to chase.

`assets/textures/{stone,soil,grass_top,grass_side}.png` are original,
procedurally-generated 16×16 pixel art authored for this PR -- not traced,
recoloured, or sampled from any existing game (`REQUIREMENTS.md` #6). See
the PR description for how they were made.

¹⁷ **Seeded noise terrain with caves (issue #48) — flat-to-slightly-faster at
both scales, and a real generation-time regression found and fixed along
the way.** `World`'s terrain moved from a fixed, unseeded sin/cos formula to
`WorldGen`: a seeded 2D height field (fractal value noise, §8) for the
surface, plus a second 3D noise field subtracted from density for caves
(§8.3) -- caves are what block 1.0's own §2 flagged as the thing that would
make the radius-64 gate honest, since a smooth heightmap flatters the
renderer.

**Render-side numbers, back-to-back against the previous row:** radius 12
1,349 → 1,282 chunks (caves hollow some fully-underground chunks down to
nothing), 439,816 → 424,352 triangles (net *fewer*, despite caves adding
wall geometry -- fewer solid chunks overall dominates), FPS ~2,012 → ~2,224,
CPU/frame **0.329 → 0.302 ms**. Radius 64: 25,131 → 26,789 chunks (opposite
direction here -- caves carve internal surfaces into chunks that were
previously fully enclosed and had nothing to mesh, so more chunks now have
*some* visible geometry even though fewer are solid throughout), 899,840 →
890,774 triangles (about flat), FPS ~877 → ~891, CPU/frame **0.821 → 0.780
ms**. Both scenes are within normal run-to-run noise of "unchanged" -- caves
redistribute where geometry is, they don't add a large net amount of it at
these radii, so this isn't the "expected to cost" delta the issue's own
"Done when" checklist anticipated. Recorded anyway, honestly, rather than
assumed.

**Where the real cost showed up instead: generation time, not render time.**
The CI-facing regression guard for this (`crates/world/tests/
radius_64_smoke.rs`, issue #89 -- a debug-build, GPU-less scan of a
radius-64 region, budgeted at 120s) went from finishing in ~1.3-1.4s to not
finishing in 120s at all once real per-voxel noise sampling replaced three
`sin`/`cos` calls per column. Root-caused to two stacked, fixable causes,
not a fundamental cost of noise-based terrain:

1. **Redundant work**: the naive per-voxel implementation computed
   `surface_height` (an expensive multi-octave 2D noise sample) up to twice
   per voxel -- once for `density`, once for material selection -- 8,192
   calls per chunk for what is only ever 256 *distinct* values (one per
   `(x, z)` column). Fixed by having `WorldGen::generate` precompute the
   16×16 column grid once and thread it through.
2. **Unnecessary work**: cave noise (three octaves of 3D value noise, 8
   hashed lattice corners each -- by far the most expensive term) was
   sampled for every voxel, including the roughly half of any region that's
   plainly above the terrain surface already. Caves only ever *subtract*
   from density, so they can never turn an already-air cell solid --
   `density_at` now returns early for those cells without touching the cave
   field at all.

Those two together, measured in isolation before any other change: smoke
test still did not finish in 120s (reached further, but not all the way).
The remaining gap was closed by tuning `CAVE_OCTAVES` down from 3 to 1 --
one octave of 3D value noise still reads as real, organic-looking caves
(see the `cave_mouth` golden and `terrain.png`, both of which now show
visible cave openings), just cheaper per sample. With both fixes and the
octave reduction: the smoke test settled in **~87s locally** (M3, debug
build) -- read as real headroom under the 120s budget, and it was, *for
this machine*.

**It wasn't enough on either CI runner.** First CI push: both macOS and
Windows timed out at exactly 120s, ~40-41k/49,923 coordinates scanned on
each -- consistent, not a flake, and both runners were measurably slower
than the M3 for this workload (a software/virtualized-adapter story similar
to block 1.4a's, this time about raw CPU rather than GPU). Squeezing more
out of the noise itself (fewer octaves, cheaper hashing) was the wrong next
lever: it trades further into visual quality for a machine-speed problem,
not an algorithmic one. The actual fix was making the *smoke test* use more
than one core: it was scanning its 49,923 coordinates on a single thread,
which is not how the live game generates terrain at all (that's a worker
pool, `cubara_render::mesher::MeshPool`, specifically so streaming doesn't
block a frame) -- the single-threaded scan was measuring a workload shape
nothing downstream of it actually has. Splitting the same scan across
`std::thread::available_parallelism()` workers, each generating + meshing
its own slice against its own `World` (no coordination needed at all, by
§8.1's own pure-function contract), took the local M3 time from ~87s to
**~19.5s** (6-way parallel, 607% CPU) -- comfortable headroom even against
a CI runner meaningfully slower per core, and a more honest measurement of
what the smoke test is supposed to be a stand-in for.

¹⁸ **Fixed-timestep tick loop and world RNG (issue #57) — render path
untouched, numbers recorded for the trend anyway.** This block added
`cubara-sim` (`Sim`/`Player`/`WorldRng`/`InputFrame`) and replaced
`cubara-render`'s `FlyCamera` with a data-only `CameraPose`; nothing in it
changes geometry, meshing, or the draw path, so chunks and triangles are
identical to the previous row (same seed, same radius). FPS/CPU moved
within normal small-scene noise (radius 12: ~2,224 → ~2,014 FPS, 0.302 →
0.313 ms; radius 64: ~891 → ~903 FPS, 0.780 → 0.783 ms) -- consistent with
"unchanged," not a regression signal.

¹⁹ **Player AABB collision, gravity and walking (issue #53) — another
render-path-untouched block.** Added a swept-AABB-vs-voxel physics module
(`crates/sim/src/physics.rs`): gravity, jump, one-block step-up, resolved
axis by axis in a fixed Y, X, Z order, running inside `Sim::tick` instead of
the renderer. `World::is_solid_at` (already registry-resolved to a plain
bool, block 1.5's edit overlay) is the only thing physics reads from the
world -- no new dependency on `cubara-voxel`/`BlockRegistry`, and nothing
about meshing, the arena, or the draw path changed. Chunks/triangles
identical to the previous row; FPS/CPU within normal small-scene noise
(radius 12: ~2,014 → ~2,221 FPS, 0.313 → 0.301 ms; radius 64: ~903 → ~898
FPS, 0.783 → 0.785 ms).

²⁰ **Selected-block outline (issue #52) — a real, if small, addition to the
render path this time.** A second pipeline (line list, `crates/render/src/
shaders/outline.wgsl`), drawn in the same pass right after the arena's
indirect submit when a block is targeted -- gravity/walking (#53) made
"which block is targeted" sim state worth showing, computed once per tick
from the player's own raycast (`Sim::target`), never in the renderer
(`ARCHITECTURE.md` Rule 3).

**These numbers don't exercise it.** `--bench` deliberately renders with no
selected block (it measures the world, not a UI highlight -- see its own
comment in `crates/app/src/bench.rs`), so the outline's `if
selected_block.is_some()` branch is untaken here and chunks/triangles/FPS/CPU
are, as expected, within normal small-scene noise of the previous row
(radius 12: ~2,221 → ~2,194 FPS, 0.301 → 0.304 ms; radius 64: ~898 → ~905
FPS, 0.785 → 0.774 ms). The golden test (`the_selected_block_shows_an_outline`)
is what actually exercises the new pipeline; it's a correctness check, not
a perf one, and this row is recorded per the "every feature is measured"
rule rather than because it's expected to move anything.

²¹ **Determinism harness (issue #90) — sim/world crates only, render path
untouched.** Adds `WorldHash` (FNV-1a over tick/RNG/player state and every
chunk in an explicit region, in fixed ascending-`ChunkCoord` order) plus a
committed replay fixture (`crates/sim/tests/determinism.rs`) that reaches
the same known-constant hash whether its chunk hashing runs on one thread
or several. None of this runs anywhere near `cargo run --release --
--bench`, which never touches `cubara-sim` at all -- chunks/triangles/FPS/CPU
are within normal small-scene noise of the previous row (radius 12: ~2,194 →
~2,173 FPS, 0.304 → 0.307 ms; radius 64: ~905 → ~903 FPS, 0.774 → 0.771 ms),
recorded per the "every feature is measured" rule. The real bar this block
clears is `cargo test -p cubara-sim --test determinism`, not this table --
see the PR for the manual verification that a deliberately-reintroduced
merge-order bug actually fails it.

²² **Save/load — region files and the world header (issue #60) — voxel/world/sim
crates only, render path untouched.** Chunk payload (de)serialisation lives with
`ChunkStorage` in `cubara-voxel`; region files (`.cbr`, §7.1/§7.3) live in
`cubara-world`; `level.ron` (the RON header -- seed, tick, RNG, player, the
block id table, §7.2) lives in `cubara-sim`, next to `WorldHash` (block 1.8)
for the same reason: `cubara-world` must never know about the player. Only
edited chunks are ever written (§7.4) -- `cargo run --release -- --bench`
never touches `cubara-sim`/save-load at all, so chunks/triangles/FPS/CPU are
within normal small-scene noise of the previous row (radius 12: ~2,173 →
~2,215 FPS, 0.307 → 0.303 ms; radius 64: ~903 → ~896 FPS, 0.771 → 0.785 ms).
The real bar is `cargo test -p cubara-sim --test save_load`: round trip
(edit → hash → save → load → hash, equal), a committed fixture
(`tests/fixtures/save_fixture/`) that loads to the same known hash on macOS
and Windows CI, an unedited chunk regenerating bit-identical after a real
save/load round trip (not just calling `WorldGen` twice), saving the same
state twice producing byte-identical files, and the two hard-error guards
(an unknown block name; a `worldgen_version` mismatch) each firing with a
message that names the problem.

²³ **Node addressing and streaming policy (issue #38's tracking arc, sub-issue
#105) — genuinely zero runtime effect.** Adds `NodeKey`, node/chunk conversion
math, a ring-schedule table, and `desired_nodes`/`plan_node_updates` to
`cubara-world`, all pure library code with unit tests of their own — nothing
in the live game, `--bench`, or `--screenshot` calls any of it yet, and won't
until sub-issue #107 (node meshing) and later wire the renderer off the
existing chunk-based `streaming::desired_chunks`/`lod_for` and onto this. No
radius-64 row this time: that number would be mechanically identical to the
previous row (issue #60's `41a1152`), since not one byte of the render/mesh
path changed. Radius 12 confirms the same, within normal small-scene noise
(~2,215 → ~2,249 FPS, 0.303 → 0.300 ms). Recorded per the "every feature is
measured" rule; the actual bar this sub-issue clears is
`cargo test -p cubara-world node::`.

²⁴ **LOD-native node generation (issue #38's tracking arc, sub-issue #106) —
also zero runtime effect.** Adds `World::node_at`, wiring `WorldGen::generate`
to run at a node's real step (`2^level`) instead of the unit step every
production call site still uses today. Still nothing calls it outside its own
tests — `World::chunk_at`/`build_chunk` (what the live renderer actually
streams through) are untouched, so radius 12's numbers are, as expected,
within normal small-scene noise of the previous row (~2,249 → ~2,211 FPS,
0.300 → 0.303 ms). No radius-64 row, same reasoning as ²³. The real bar is
`cargo test -p cubara-world world::` (the new `node_at`-specific cases:
matches `chunk_at` at level 0, matches `WorldGen::generate` directly above
it, and does not reflect an edit at level > 0, per §4).

²⁵ **Node meshing on the worker pool, one mesh per node (issue #38's tracking
arc, sub-issue #107) — the first row where draw count actually drops.**
`MeshPool`'s job identity becomes `NodeKey`; `mesh_node` calls `World::node_at`
+ `Chunk::build_mesh` (no downsampling — the sampling itself is already
coarse for level > 0). `ChunkArena` is re-keyed from `ChunkCoord` to
`NodeKey`, and the per-node origin storage buffer gained a `scale` in its
previously-spare `.w` component (1.0 at level 0, `2^level` above it) so a
node's `16³` lattice can represent `2^level` chunks per axis without a new
vertex format. `ChunkArena::from_region` (what `--bench`, `--screenshot` and
every golden test build their scene through) now streams
[`DEFAULT_RING_SCHEDULE`](crates/world/src/node.rs), truncated at the
requested radius, instead of a flat full-resolution region — **this is a
real, intended change to what `--bench <radius>` measures, not just a
relabelling**: radius 12 used to mean "1,282 chunks, all full-resolution
(`FULL_RES` = 12 covered the whole region)"; it now means "the same ring
schedule the live renderer uses, truncated at 12" (`[(0, 8), (1, 12)]`),
which is why radius 12's own node/triangle count drops (1,282 chunks → 690
nodes) rather than holding flat like sub-issues #105/#106 did. Radius 6 and
below (every golden test) still resolves to a single `[(0, radius)]` — level
0 only, one node per chunk — so all 6 golden images passed byte-identical,
with no re-blessing, proving level 0 is pixel-identical to the pre-node path
rather than merely assumed to be.

The headline number is radius 64 against the previous row (issue #60's
`41a1152`, still the accurate baseline since #105/#106 had zero runtime
effect): **26,789 drawn chunks → 1,238 drawn nodes, a ~21.6× reduction** —
CPU/frame **0.785 → 0.408 ms (-48%)**, throughput **~896 → ~1,673 FPS**, and
radius 64 **clears the 1000-FPS gate for the first time** since it was first
measured (²²). This is with `DEFAULT_RING_SCHEDULE`'s illustrative,
*untuned* radii (§6.3) and no skirts yet (LOD-boundary cracks are expected
and accepted at this stage, issue #108's job) — both real headroom still on
the table, not this row's ceiling. Tuning the schedule against the real
`<2,000`-draws budget is sub-issue #109's job once there's a number to tune
against; this row is that number.

²⁶ **Skirts to hide LOD seams (issue #38's tracking arc, sub-issue #108).**
§6.4's decision: each node/chunk extends its own border wall quads downward
by one lattice cell, purely from that node's own data (no neighbour lookup —
see `push_skirt` in `crates/voxel/src/voxel.rs`), rather than stitching
transition geometry matched to a neighbour's resolution. Node/draw count is
unchanged at both radii (690 and 1,238) — skirts add triangles inside
existing draws, never a new draw — which is exactly the cost profile §6.4
promises ("a handful of quads" per border, not a second meshing pass).

Triangles rose radius 12: 250,982 → 281,274 (+12.1%), radius 64: 625,258 →
689,436 (+10.3%). CPU/frame moved with it — radius 12: 0.192 → 0.214 ms,
radius 64: 0.408 → 0.444 ms — and radius 64 throughput eased ~1,673 → ~1,558
FPS, still clearing the 1000-FPS gate with headroom. This is the real,
accepted cost of hiding LOD-boundary cracks: applied to *every* border wall
regardless of level, since a node can't know whether a given edge actually
meets a different-level neighbour or a same-level one without a cross-node
lookup, which the design explicitly forbids. A same-level neighbour's
lattice already lines up exactly, so its skirt is simply never visible
(confirmed by the golden images: `terrain`/`cave_mouth`, both dense with
same-level chunk seams, show no new artifacts) — the added cost is genuinely
uniform per-border overhead, not concentrated at real seams alone.

`crates/world/src/world.rs`'s `region_mesh_output_is_stable` (a fixed-region,
no-GPU regression guard) moved 13,510 → 14,068 triangles for the same
reason; updated with the same "why did this pinned number move" comment
trail that test already keeps.

²⁷ **Ring schedule tuned to the <2,000-draw budget (issue #38's tracking arc,
sub-issue #109).** §6.3's placeholder table (`[(0,8),(1,16),(2,32),(3,64)]`,
sub-issue #105) was never tuned against a real measurement; this is that
pass. Widened the full-resolution near field from chunk-radius 8 to 10 and
level 1 from 16 to 18 (levels 2/3 and the radius-64 ceiling untouched — far
enough out that widening them buys much less visible quality per node than
widening the near field does), landing at **1,585 resident nodes at radius
64**, measured consistently (1,585-1,613) across four different world
positions, not just the origin. A tighter table (11/19/33/64, 1,935-1,956
resident) was tried and rejected: technically still under 2,000, but only a
2-3% margin — too close to trust across world positions and seeds it wasn't
measured against, and §6.3/issue #109 are explicit that the 2,000 ceiling is
not the agent's to relax under any circumstance, so margin against
measurement noise matters more than squeezing out the last few hundred
nodes. The near field is **not** visibly coarser than before this PR — it's
wider than the placeholder table gave it, the opposite finding from the one
issue #109 asks to be reported honestly if it occurred.

Against the immediately preceding row (issue #108, `d518f53`, same 1000-FPS
gate check): radius 64 nodes **1,238 → 1,585** (+28%, using headroom the
skirts row didn't touch), tris 689,436 → 829,608, CPU/frame 0.444 → 0.526
ms, throughput ~1,558 → ~1,295 FPS — still clearing the gate.

Against the **original issue #89 baseline** (`49146ef`, the number this
whole sub-arc exists to fix): radius-64 draws **25,131 → 1,585, a 15.9×
reduction** (not yet the full ~25× §6.1 projects, since skirts add geometry
but no draws, and the schedule still has ~20% margin left below 2,000 that
favours robustness over squeezing out the theoretical maximum), CPU/frame
**0.715 → 0.526 ms (-26%)**, throughput **~996 → ~1,295 FPS (+30%)**. Radius
64 now clears the 1000-FPS gate with real margin, not just barely, on the
same scene issue #89 first measured it failing on.

Windows numbers not yet recorded for this row — per this file's own
convention, the macOS M3 row lands with the PR and the Windows row is added
when next run there.

²⁸ **`cubara-render` drops its `cubara-world` dependency (issue #38's
tracking arc, sub-issue #110) — a pure relocation, and the numbers confirm
it.** Node meshing (`MeshPool`/`mesh_node`/`sort_batch`, the ring-schedule
streaming policy) moved into a new `cubara_world::mesh` module; `cubara-render`
now takes already-meshed geometry (`MeshedNode`, keyed by an opaque `NodeId`
it defines itself) and never imports `cubara_world` in production code again
(`crates/render/Cargo.toml`'s `[dependencies]` — enforced by a new
`scripts/check-architecture.sh` check; `cubara-world` stays a legitimate
`[dev-dependencies]` entry for golden-image tests, which build real scenes).
`cubara-app` is the new glue (`crates/app/src/streaming.rs`), since it's the
one crate meant to depend on both.

Both radii land within measurement noise of the immediately preceding row
(#109, `1e84478`): radius 12 nodes/tris **unchanged** (957/367,026), FPS
~2,357 → ~2,402, CPU/frame 0.280 → 0.265 ms; radius 64 nodes/tris
**unchanged** (1,585/829,608), FPS ~1,295 → ~1,291, CPU/frame 0.526 → 0.531
ms. Node/triangle counts matching exactly is the real evidence here, more
than the FPS figures — this PR could not have changed the meshed scene at
all without a bug, and it didn't. All 7 golden-image tests pass byte-for-byte
unmodified (no `CUBARA_BLESS`), the strongest evidence available that this
is a true no-op relocation, not just numerically close.

²⁹ **Arena capacity re-sized for the node tree (issue #38's tracking arc,
sub-issue #111, deferred from #89).** `VERTEX_CAPACITY`/`INDEX_CAPACITY`/
`MAX_DRAWS`/`MAX_NODES` were sized in block 1.0 for a per-*chunk* resident
set (25,131 chunks at radius 64); #89 explicitly deferred re-sizing until
real node-tree numbers existed. They do now (#109):

| Constant | #89-era | New | Measured peak it's sized against | Headroom |
|---|---|---|---|---|
| `VERTEX_CAPACITY` | 4,000,000 | **4,000,000 (unchanged)** | 1,659,216 vertices used | ~2.4× |
| `INDEX_CAPACITY` | 6,000,000 | **6,000,000 (unchanged)** | 2,488,824 indices used | ~2.4× |
| `MAX_DRAWS` | 16,384 | **4,096** | 1,341/1,585 visible from a wide-open orbit (~85%) | ~2.5× |
| `MAX_NODES` | 65,536 | **16,384** | 1,585-1,613 resident (4 world positions) | ~10× |

The honest finding here, stated plainly rather than assumed: **node/draw
count dropped 4-16×, but vertex/index memory did not drop at all.** Total
triangle volume is a property of how much terrain is visible, not how many
draws it takes to submit — the node tree's whole point (§6.1) is fewer,
larger draws covering the *same* geometry, not less geometry. `VERTEX_CAPACITY`/
`INDEX_CAPACITY` were never really "sized for radius 12" in any binding
sense (that was just the reference scene available in block 1.0); measured
against the real, current radius-64 worst case for the first time here, the
#89-era numbers turn out to already be correctly sized (~2.4× headroom) and
don't move.

GPU memory footprint (vertex 12 B/vertex, index 4 B/index, indirect-args 20
B/entry, origins 16 B/entry): **70.0 → 69.0 MiB total** (~1.4% smaller,
because vertex/index dominate and didn't shrink) — but the two buffers that
actually scale with draw/node count shrink **75% each**: indirect-args 0.31
→ 0.08 MiB, origins 1.00 → 0.25 MiB. Modest in absolute bytes (both were
already tiny), but real, and it tightens the worst-case bound instead of
leaving 4-16× more slack than the measured peak justifies.

Re-verified `ArenaUsage::exhausted`'s warning path is still correct and
reachable at the new sizes: temporarily set `MAX_DRAWS` to 1,000 (below the
measured 1,585 peak) and confirmed `--bench 64` logs the expected `WARN
region exceeds arena capacity: draws` line and correctly clamps the drawn
set to 1,000/1,585 rather than silently corrupting anything — then reverted.
`--bench 64` at the real (4,096) capacity logs zero exhaustion warnings, as
required.

Both radii land within measurement noise of the immediately preceding row
(#110, `0fe7e7d`): radius 12 nodes/tris unchanged (957/367,026), FPS ~2,402 →
~2,474; radius 64 nodes/tris unchanged (1,585/829,608), FPS ~1,291 → ~1,275.
As expected for a sizing-only change — the constants only bind when a scene
is *close to* the old capacities, and this scene never was.

³⁰ **Block 1.10 closeout — the phase-1 gate verified end to end (issue #38,
all 7 sub-issues merged).** Not new code — HEAD (`851e639`) is `da55704`
plus one CI-script fix (#119), so the scene is byte-identical to the #111
rows above. This row records the acceptance run for the tracking issue:

- `./scripts/check-phase-gate.sh 1` → **12 passed, 0 failed** (`cargo test
  --all`, clippy, fmt, both architecture checks, determinism replay, all
  three golden images incl. the LOD boundary, player-AABB, cross-platform
  bit-identical chunk, neighbour isolation, save round-trip).
- **Drawn-node count at radius 64: 1,341 / 1,585 — decisively under the
  2,000-draw budget** that §2/§6 named as the whole point of the block
  (down 16× from the #89 baseline's 25,131 per-chunk draws).
- Golden `no_crack_at_a_real_lod_boundary` green → skirts hide the LOD seam.

**The "unexplained 4× discrepancy" this footnote originally flagged is
resolved: it was a different machine.** As first written, this row sat in the
macOS M3 table and reported a *tight* 3,153–3,580 FPS / 0.123–0.129 ms
CPU/frame — ~4× faster on CPU/frame than the #111 radius-64 row (0.535 ms) two
days earlier on effectively identical code — which the footnote attributed,
honestly but wrongly, to a warm-burst-vs-cold measurement regime. It was not a
measurement regime. PR #121's own body records the run as *"this machine —
Win11, i7-12650H / RTX 4060"*: it is a **Windows** measurement that was
appended to the **macOS** table, where the M3 rows around it made it look like
a 4× speedup out of nowhere. The row has been moved to the Windows table above,
and the 2026-08-24 gate rows confirm the reading — a *cold* Windows run at the
same scene and effectively the same code lands at 3,311–3,739 FPS / 0.102–0.156
ms, i.e. exactly the "warm burst" regime, first run after idle. Windows/RTX 4060
is simply ~4-5× cheaper per frame than M3 on this scene. Nothing regressed and
nothing sped up. The load-bearing number for the block — drawn nodes < 2,000 —
held under every run regardless, which is why the block's conclusion is
unaffected.

³¹ **Phase 1's exit gate, run on the Windows machine — the half that was
missing.** Every phase-1 feature row above sits in the macOS M3 table because
that is where the work was done; the Windows table had nothing newer than July.
ROADMAP.md's gate says *run on both machines, with a `BENCHMARKS.md` row for
each*, so these two rows are that second machine, at HEAD `e60e9c2`.

```
./scripts/check-phase-gate.sh 1  →  12 passed, 0 failed
GPU: NVIDIA GeForce RTX 4060 Laptop GPU (Vulkan, driver 581.42)
SUMMARY: 3579 FPS | CPU/frame avg 0.102 ms (p99 0.384) | 1341/1585 nodes | 1000-FPS gate MET
```

All twelve criteria pass: `cargo test --all`, clippy, fmt, both architecture
checks, `--bench 64` ≥ 1000 FPS, the determinism replay (single- vs
multi-threaded, identical hash), all three golden images including the LOD
boundary, player-AABB tunnelling, the cross-platform bit-identical chunk,
neighbour isolation, and the save round-trip.

**Spread (cold, first-run-after-idle, then back-to-back):** radius 64 read
3,311–3,739 FPS / 0.102–0.156 ms CPU/frame across four runs (median ~3,579 FPS; the
recorded run is 0.102 ms / p99 0.384); radius 12 read 4,774–5,053 FPS / 0.086–0.090 ms across
three (recorded ~4,888 / 0.088 ms). The one 0.156 ms outlier carries a p99 of
1.398 ms against ~0.39 ms elsewhere — a scheduling hiccup in that run, not a
regime.

**Against the macOS M3 rows for the same commit-era scene** (`da55704`, byte-identical
scene: 1,585 nodes / 829,608 tris): M3 ~1,275 FPS / 0.535 ms → Windows ~3,579 FPS
/ 0.102 ms. That is a **machine** difference (discrete RTX 4060 vs integrated M3
at a scene that is submit-bound), not a change in the engine — see ³⁰, where the
same gap was briefly mistaken for a speedup. Both machines clear the 1000-FPS
gate at radius 64: M3 with ~1.3× margin, Windows with ~3.6×.

Scene is unchanged from `da55704` (1,585 nodes, 829,608 triangles, 1,341 drawn
after frustum cull — under the 2,000-draw budget). The only code since is
`e60e9c2`, the sub-tick mouse-look fix (#122), which touches input handling in
the app and not the render or streaming path; the identical node/triangle counts
confirm it.

³² **The skirt overlap fix (#125) — ~9% of the geometry was redundant, and the
frame cost did not notice.** Reported in-game as dirt and stone flickering
through each other along node boundaries: skirts were being emitted over cells
that already had a real face, so two coplanar same-facing quads z-fought. Those
skirts hid no crack; they were duplicates.

| Radius | Tris before | Tris after | Delta | FPS | CPU/frame |
|---|---|---|---|---|---|
| 12 | 367,026 | **331,510** | **-35,516 (-9.7%)** | ~4,888 → ~4,771 | 0.088 → 0.089 ms |
| 64 | 829,608 | **758,754** | **-70,854 (-8.5%)** | ~3,579 → ~3,665 | 0.102 → 0.112 ms |

**The triangle counts are the real number here; the FPS and CPU/frame columns
are not.** Triangle count is deterministic — it reads identically on every run,
and ≈8-10% of the scene's geometry was duplicate quads. CPU/frame moved by
0.001-0.010 ms, which is inside the spread the same binary produces run to run
(radius 64 read 0.105-0.113 ms across three runs here, against 0.102-0.156 ms
for the pre-fix rows), so **no speedup is claimed** — this scene is submit-bound
and 70k triangles is not what binds it. Node and draw counts are unchanged
(1,585 / 1,341 drawn), which is the expected shape: the fix removes geometry
*within* nodes, and draw count is what this scene is actually sensitive to.

The correctness result is the point, and it is pinned by a test rather than by
this row: `a_skirt_never_covers_a_cell_that_already_has_its_own_face` fails on
the old code with 18 doubly-covered cells. No golden image was regenerated —
`no_crack_at_a_real_lod_boundary` passes against the committed reference
unchanged, on both CI backends, so the crack-hiding the skirts exist for is
intact.

## Detailed run logs

Kept for the notable/first runs; the tables above are the quick trend view.

### 2026-07-18 — Windows 11 desktop/laptop (RTX 4060 Laptop GPU), commit `0ab6034`

```
GPU: AdapterInfo { name: "NVIDIA GeForce RTX 4060 Laptop GPU", vendor: 4318, device: 10400, device_type: DiscreteGpu, driver: "NVIDIA", driver_info: "581.42", backend: Vulkan }
world: 137 chunks meshed, 22788 triangles
rendering 1920x1080, 137 chunk draw calls
=========== BENCHMARK RESULT ===========
frames            : 2000
throughput        : 8097 FPS (sustained, pipelined)
CPU submit / frame: avg 0.083 ms | p50 0.064 | p99 0.350
chunks drawn      : avg 137.0 / 137 (frustum-culled)
========================================
```

**Notes:** first benchmark run after setting up the toolchain (Git + rustup)
fresh on this machine. 8.1k FPS is ~8x the M1 gate — CPU submit cost is
essentially noise at 0.08 ms/frame, so at this scene size we're nowhere near
CPU- or GPU-bound.

### 2026-07-18 — macOS, Apple M3 (8 GB, integrated GPU, Metal), commit `c6921e9`

```
GPU: AdapterInfo { name: "Apple M3", vendor: 0, device: 0, device_type: IntegratedGpu, driver: "", driver_info: "", backend: Metal }
world: 137 chunks meshed, 22788 triangles
rendering 1920x1080, 137 chunk draw calls
=========== BENCHMARK RESULT ===========
frames            : 2000
throughput        : 9242 FPS (sustained, pipelined)
CPU submit / frame: avg 0.070 ms | p50 0.050 | p99 0.246
chunks drawn      : avg 137.0 / 137 (frustum-culled)
========================================
```

**Notes:** the integrated M3 GPU actually edges out the RTX 4060 laptop at this
scene size (9.2k vs 8.1k FPS), confirming we're bound by neither GPU here: the
frame is dominated by pipeline/submit overhead, and the M3's lower CPU submit
cost (0.070 vs 0.083 ms) is what shows up. Discrete-GPU advantage should only
appear once the scene gets meaningfully heavier.

### 2026-07-19 — macOS, Apple M3, M3.5 Step 1 (chunk arena + indirect), commit `41e38f5`

Before/after on the same machine, heavy 1,349-chunk scene, one representative run
of each (see footnote ³ for the run spreads):

```
# BEFORE — one draw call per chunk (8b5467e)
rendering 1920x1080, 1349 chunk draw calls
throughput        : 2844 FPS (sustained, pipelined)
CPU submit / frame: avg 0.317 ms | p50 0.285 | p99 0.599
chunks drawn      : avg 1321.9 / 1349 (frustum-culled)

# AFTER — one multi_draw_indexed_indirect over the shared arena (41e38f5)
multi_draw_indirect: true
region radius 12: 1349 chunks meshed, 217550 triangles (arena v 435100/4000000, i 652650/6000000)
rendering 1920x1080, 1349 chunks via 1 multi_draw_indirect
throughput        : 3297 FPS (sustained, pipelined)
CPU submit / frame: avg 0.199 ms | p50 0.175 | p99 0.535
chunks drawn      : avg 1321.9 / 1349 (frustum-culled)
```

**Notes:** collapsing ~1,349 draw calls into one indirect submit cut CPU/frame by
~37% (0.317 → 0.199 ms) with identical rendered output. What's left of CPU/frame is
mostly the CPU frustum cull writing the indirect list — the work **#28** hands to a
compute shader. The `--caps` spike (#26) confirmed both target backends support
`MULTI_DRAW_INDIRECT`; Metal lacks only `MULTI_DRAW_INDIRECT_COUNT`, which Step 2
will need a fallback for.
