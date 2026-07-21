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
Add a row to the history table for the machine it ran on, with the milestone/
feature and the commit (`git rev-parse --short HEAD`). New feature PRs should
append a Windows row; the macOS M3 machine gets a row when it's run there.

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

² **First meaningful FPS number.** The streaming renderer measures a ~1,350-chunk
region (10× the old grid), which pushes the frame into being **CPU-submit-bound**:
one draw call per chunk (~1,322 drawn after culling) dominates at ~0.5 ms/frame.
Because it's now bound by real work rather than pipeline overhead, FPS is far
tighter — 4 runs spanned **1,836–2,082 FPS** (±~6% vs the ±40% of the 137-chunk
rows). This is *not* comparable to the rows above (different, much heavier scene) —
it's the new baseline to optimize down from. The obvious next lever is the draw-call
count: batching chunks into fewer draws (instanced / indirect / GPU-driven) should
move this number, and it'll show up right here.

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
