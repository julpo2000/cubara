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
| 2026-07-21 | M3.5 — shared arena + indirect multi-draw (Step 1) | 1,349 | 217,550 | ~4,711³ | 0.177 ms | 1.017 ms | `1869300` |

### macOS — Apple M3, 8 GB (integrated GPU, Metal)

| Date | Milestone / feature | Chunks | Tris | FPS | CPU/frame avg | CPU/frame p99 | Commit |
|---|---|---|---|---|---|---|---|
| 2026-07-18 | M2 — frustum culling (baseline) | 137 | 22,788 | 9242 | 0.070 ms | 0.246 ms | `c6921e9` |

¹ FPS at this scene is submit-bound and noisy. 4 back-to-back runs on `7a249d2`
climbed **monotonically 9,732 → 10,471 → 11,719 → 13,657 FPS** — not random
scatter but CPU/GPU clock ramp: the 200-frame warmup (~20 ms at these rates) ends
long before boost clocks settle, and each launch inherits a warmer GPU from the
last, so successive runs aren't independent samples. CPU/frame stayed tight at
0.065–0.083 ms throughout. The M3 foundation is behaviour-unchanged (same
137-chunk scene), so this is a same-scene re-baseline, not a real speedup; treat
CPU/frame as the comparable number, and take first-run-after-idle FPS over a
warmed-up burst when comparing across features.

² **First meaningful FPS number.** The streaming renderer measures a ~1,350-chunk
region (10× the old grid), which pushes the frame into being **CPU-submit-bound**:
one draw call per chunk (~1,322 drawn after culling) dominates at ~0.5 ms/frame.
Because it's now bound by real work rather than pipeline overhead, FPS is far
tighter — 4 runs spanned **1,836–2,082 FPS** (±~6% vs the ±40% of the 137-chunk
rows). This is *not* comparable to the rows above (different, much heavier scene) —
it's the new baseline to optimize down from. The obvious next lever is the draw-call
count: batching chunks into fewer draws (instanced / indirect / GPU-driven) should
move this number, and it'll show up right here.

³ **The draw-call lever, pulled.** Same 1,349-chunk scene as the M3 heavy row —
directly comparable — but all chunk geometry now lives in one shared vertex/index
buffer and the ~1,322 visible chunks draw in a **single `multi_draw_indexed_indirect`**
instead of ~1,322 separate draw calls. First-run-after-idle: **4,711 FPS** (vs M3's
~1,980, **+138%**) at **0.177 ms/frame** avg (vs ~0.49 ms, **−64%**). As always at
this submit-bound scene the warmed burst ramps on clock boost — the three
back-to-back runs went 4,711 → 6,645 → 8,246 FPS with CPU/frame settling to
**~0.08 ms** (p50 was already 0.051 ms on the cold run), so the steady-state CPU cost
is ~6× below M3. CPU frustum culling stays on the CPU here; moving it to a compute
shader (Step 2, #28) is what should make CPU/frame flat regardless of chunk count.

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
