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

**Measure on an idle machine, and do not trust a number taken straight after a
build.** On the 8 GB M3 the same binary on the same commit reads **1,540-1,587
FPS cold and 623-861 FPS within a minute of `cargo test --all`** -- a factor of
2.3 on identical geometry, with CPU/frame moving 0.441 -> 0.674 ms alongside it.
That is throttling plus memory pressure, not work; footnote ⁵⁵ has the table.
It matters because `check-phase-gate.sh` runs `--bench 64` immediately after the
test suite, so on this machine the gate's perf line can report a failure the
engine does not have.

Footnotes carry the detail, and each one is **numbered once** — take the next
unused number rather than the next one in reading order, because two notes under
one number make a row cite someone else's measurement. This file is also the one
a merge is most likely to duplicate rather than combine, so:

```bash
./scripts/check-benchmark-history.sh
```

It is a grep, it runs in CI as part of `architecture rules`, and its header
comment records the merge that made it necessary — this file was briefly two
contradictory copies of itself.

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
| 2026-08-24 | Reversed-Z depth [#129], radius 64³³ | 1,585 | 758,754 | ~3,952 | 0.100 ms | ~0.40 ms | `5277ddf` |
| 2026-08-24 | Texture mip chain [#128], radius 64³⁴ | 1,585 | 758,754 | ~3,990 | 0.100 ms | ~0.38 ms | *(this PR)* |
| 2026-09-10 | Windows caught up to `main` — phase 2 complete, radius 64, band ±2⁴⁴ | 3,138 | 912,964 | ~2,506 | 0.136 ms | ~0.43 ms | `b82e051` |
| 2026-09-11 | Gate re-run after six PRs, radius 64, band ±2⁴⁵ | 3,138 | 912,964 | ~2,774 | 0.124 ms | ~0.385 ms | `9dee4a6` |
| 2026-09-13 | Seven play-test PRs (#240–#246), radius 64, band ±2⁴⁶ | 3,138 | 912,964 | ~2,674 | 0.131 ms | ~0.48 ms | `4002754` |
| 2026-09-13 | **Covered border faces left out** — nodes meshed against their neighbours, radius 64, band ±2⁴⁷ | **1,969** | **674,484** | **~4,160** | **0.102 ms** | ~0.44 ms | *(this PR)* |
| 2026-09-13 | LOD rings tile exactly (octree) — the gaps are drawn now, radius 64, band ±2⁴⁸ | 2,174 | 738,078 | ~3,890 | 0.107 ms | ~0.35 ms | *(this PR)* |
| 2026-09-13 | **No vertical band** — 3D octree, vertical LOD squash 2, radius 64 (orbit)⁴⁹ | 4,377 | 1,015,554 | ~2,950 | 0.125 ms | ~0.41 ms | *(this PR)* |
| 2026-09-13 | **Visibility culling** — only what a line of sight can reach is generated, meshed and drawn; eye at y=40⁵⁰ | 1,555 | 615,408 | ~7,310 | 0.069 ms | — | *(this PR)* |
| 2026-09-13 | **Caves near the surface at every level of detail**; coarse cells take the majority of their blocks; visibility in sub-blocks, off-thread; radius 64 (orbit)⁵¹ | 2,219 | 901,932 (480,547 drawn) | ~3,920 | 0.125 ms | ~0.41 ms | *(this PR)* |
| 2026-09-13 | **Faces turned away from the camera left out** — meshes grouped by direction, radius 64 (orbit)⁵² | 4,377 | 1,015,554 (573,298 drawn) | ~3,592 | 0.168 ms | ~0.48 ms | *(this PR)* |
| 2026-09-13 | **Mountains** — ridged ranges up to ~y 240, radius 64 (orbit)⁵³ | 4,506 | 1,110,830 (592,540 drawn) | ~3,451 | 0.169 ms | ~0.43 ms | *(this PR)* |

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
| 2026-08-30 | macOS baseline caught up to `main` — skirt fix, reversed-Z, mips, blocks 2.1–2.2, radius 64³⁵ | 1,585 | 758,754 | ~1,462 | **0.483 ms** | ~0.89 ms | `f9080f7` |
| 2026-08-30 | **Oak trees via the structure pass** [#154], radius 64³⁵ | **1,600** | **766,476** | ~1,442 | **0.484 ms** | ~0.89 ms | `89ca231` |
| 2026-08-31 | **Iron ore as a density-pass threshold** [#156], radius 64³⁶ | 1,600 | **822,420** | ~1,381 | 0.509 ms | ~0.86 ms | `a0b8108` |
| 2026-08-31 | Drops and tool tiers from data [#158], radius 64³⁷ | 1,600 | 822,420 | ~1,385 | 0.509 ms | ~0.86 ms | `b78ab56` |
| 2026-08-31 | Mining takes time [#159], radius 64³⁷ | 1,600 | 822,420 | ~1,380 | 0.502 ms | ~0.92 ms | `d4ee268` |
| 2026-08-31 | The furnace: block entities, wood fuel, smelting [#160], radius 64³⁷ | 1,600 | 822,420 | ~1,381 | 0.499 ms | ~0.90 ms | `19f0fa9` |
| 2026-08-31 | ECS + dropped items [#56], radius 64³⁷ | 1,600 | 822,420 | ~1,381 | 0.505 ms | ~0.86 ms | `9ec31bb` |
| 2026-08-31 | Chunk state machine + dormancy [#47], radius 64³⁷ | 1,600 | 822,420 | ~1,375 | 0.498 ms | ~0.87 ms | `511f58e` |
| 2026-08-31 | Bounded dormant catch-up [#58], radius 64³⁷ | 1,600 | 822,420 | ~1,367 | 0.496 ms | ~0.95 ms | `55ce3e5` |
| 2026-08-31 | Save format covers phase 2 state [#170], radius 64³⁷ | 1,600 | 822,420 | ~1,387 | 0.513 ms | ~0.85 ms | `4b4b131` |
| 2026-08-31 | Health, fall damage, regeneration [#172], radius 64³⁷ | 1,600 | 822,420 | ~1,379 | 0.496 ms | ~0.93 ms | `3df7b28` |
| 2026-08-31 | Caves at level 0 only [#175], radius 64, **old 3-layer slab**³⁸ | 1,600 | **675,962** | **~1,553** | **0.447 ms** | ~0.78 ms | *(this PR)* |
| 2026-08-31 | **World with no height limit** [#175], radius 64, band ±2³⁸ | **3,138** | 912,964 | ~1,115 | 0.613 ms | ~1.08 ms | `4da25da` |
| 2026-08-31 | Fixed-point positions, radius 64, band ±2³⁹ | 3,138 | 912,964 | ~1,109 | 0.622 ms | ~1.04 ms | `24e4789` |
| 2026-09-01 | Client-side replica world, radius 64, band ±2⁴⁰ | 3,138 | 912,964 | ~1,101 | 0.628 ms | ~1.22 ms | `a9f238e` |
| 2026-09-01 | Fixed-point angles, radius 64, band ±2⁴¹ | 3,138 | 912,964 | ~1,096 | 0.630 ms | ~1.38 ms | `bbb09c2` |
| 2026-09-05 | macOS caught up to `main` — many players, per-client views, the transport [#199, #201, #203], radius 64, band ±2⁴² | 3,138 | 912,964 | ~1,108 | 0.628 ms | ~0.98 ms | `01aa32e` |
| 2026-09-07 | Prediction, untrusted clients, server-side mining, persistence, sharding [#207, #211, #212, #216, #217, #218], radius 64, band ±2⁴³ | 3,138 | 912,964 | ~1,109 | 0.623 ms | ~1.19 ms | `397a653` |
| 2026-09-11 | Multiplayer played: --connect, player figures, per-machine shirts, mining retuned, radius 64, band ±2⁴⁵ | 3,138 | 912,964 | ~1,112 | 0.611 ms | ~1.08 ms | `b5dcfec` |
| 2026-09-14 | **macOS caught up to `main`** — covered borders, 3D octree LOD, visibility culling, faces turned away left out, distant caves, mountains [#250–#259], radius 64 (orbit)⁵⁵ | **2,219** | 901,932 (480,547 drawn) | **~1,568** | **0.441 ms** | ~0.94 ms | `3b52c49` |
| 2026-09-15 | Bench measures GPU/frame, draws, its own timed window (cross-session review, package 1), radius 64 (orbit)⁵⁷ | 2,219 | 901,932 (480,547 drawn) | ~1,585 | 0.468 ms | 0.777 ms | `a539938` |
| 2026-09-16 | One FrameUniform, distance fog, one sun instead of two (cross-session review, package 2), radius 64 (orbit)⁵⁸ | 2,219 | 901,932 (480,547 drawn) | ~1,762 | 0.289 ms | 1.056 ms | `c34101e` |
| 2026-09-16 | Mesh fog from a plain 1/w varying + flat face index, not a `world_pos` varying (fixes ⁵⁸'s regression), radius 64 (orbit)⁵⁹ | 2,219 | 901,932 (480,547 drawn) | **~1,126** | **0.656 ms** | -- | `28564f8` |
| 2026-09-16 | Fog depth from `clip_pos.z`, zero varyings (recovers ⁵⁹'s M3 varying cost), radius 64 (orbit)⁶⁰ | 2,219 | 901,932 (480,547 drawn) | **~1,157** | **0.638 ms** | -- | `dad0dd6` |

### Linux — Intel i7-8750H / NVIDIA GTX 1060 Max-Q Design (Vulkan)

| Date | Milestone / feature | Chunks | Tris | FPS | CPU/frame avg | CPU/frame p99 | Commit |
|---|---|---|---|---|---|---|---|
| 2026-09-11 | **Linux (Vulkan) baseline — first measured** [#36], radius 64, band ±2⁵⁴ | 3,138 | 912,964 | ~1,474 | 0.214 ms | 0.883 ms | `2ab5fb6` |
| 2026-09-15 | **Bench measures GPU/frame, draws, its own timed window** (cross-session review, package 1), radius 64⁵⁶ | 2,219 | 901,932 | ~1,970 | 0.272 ms | 0.957 ms | `401a9e5` |
| 2026-09-16 | Mesh fog from a plain 1/w varying + flat face index, not a `world_pos` varying (M3 regression fix), radius 64⁵⁹ | 2,219 | 901,932 | ~2,062 | 0.266 ms | 0.980 ms | `28564f8` |
| 2026-09-16 | Fog depth from `clip_pos.z`, zero varyings (recovers ⁵⁹'s M3 varying cost)⁶⁰ | 2,219 | 901,932 | ~1,940 | 0.274 ms | 1.021 ms | `dad0dd6` |
| 2026-09-17 | Radial fog + lighting as pipeline overrides, background rebuild on change (owner's decision from the ⁶⁰ research)⁶¹ | 2,219 | 901,932 | ~1,933 | 0.270 ms | 1.145 ms | `e4e676a` |

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

³³ **Reversed-Z depth (#129) — precision, not speed, and one node changed
sides.** Depth now runs 1 at the near plane to 0 at the far plane. Paired with
the existing `Depth32Float` buffer this is close to the best depth precision
available: float precision clusters near zero, and the conventional mapping
spends it all on the near plane where everything is already close and large.
With a near/far ratio of 0.1 : 2,000 and a horizon block 1.10 made worth
looking at, that was the wrong way round.

Three runs read **3,851–4,095 FPS / 0.098–0.103 ms** against **3,639–3,705 /
0.105–0.113 ms** for the pre-change row. That looks like a small win and is
**not claimed as one** — the ranges nearly touch, and nothing about reversing a
matrix should make the frame cheaper. Treat it as the same performance.

**Drawn nodes moved 1,341 → 1,342.** Reversing depth swaps which row
combination of the view-projection yields the near plane and which yields the
far one; the resulting six planes are the same six planes mathematically, but
not bit-identical after normalisation, so a node sitting exactly on a frustum
boundary can classify differently. The cull is conservative, so the flip is
toward drawing rather than dropping — the safe direction. Determinism is
unaffected (`the_same_scene_renders_byte_identically` still passes), and
`a_reversed_z_frustum_culls_identically` pins that the two extractions agree on
every non-borderline case.

**No golden image was regenerated**, which is the useful signal here: every
reference still passes, at 0.0012%–0.139% differing pixels against a 0.2%
threshold. Only fragments at depth ties moved, which is exactly what a
precision change should touch and nothing else.
³⁴ **A full mip chain for the block textures (#128) — a visual-quality fix
that costs nothing measurable.** The sampler already asked for linear
minification and linear mipmap filtering; with `mip_level_count: 1` both
settings were inert. At radius 64 the horizon is 1,024 blocks away, where a
16×16 tile covers well under a pixel, so distant terrain sampled one texel per
pixel and shimmered under movement — which a screenshot does not show and no
test caught.

Three back-to-back runs read **3,653–4,080 FPS / 0.097–0.102 ms CPU/frame**
against **3,851–4,095 / 0.098–0.103 ms** for the reversed-Z row above. The
ranges overlap almost exactly; this is the same performance, not a win or a
loss. Geometry is untouched (1,585 nodes, 758,754 tris, 1,342 drawn — the 1,342
is reversed-Z's borderline node, ³³, not anything mips did), and the added GPU
cost of trilinear sampling is offset by mips being far kinder to the texture
cache at distance. Recorded the median run.

Measured *after* rebasing onto reversed-Z rather than reusing the pre-rebase
numbers, since the row now sits after it and a figure measured against a
different base is not a delta against the row above it.

The real result is in the golden images, and it is large: `terrain` moved 4.67%
of its pixels and `lod_boundary` 6.12%, in both cases by replacing per-texel
speckle across the whole surface with a coherent one. See the PR for the
before/after and for why the reference set moved from Metal to Vulkan.

³⁵ **Oak trees via the structure pass (#154) — measured A/B on an idle
machine, because the first attempt was not.** The draft PR deliberately carried
no row: every reading had been taken with a game running on the same GPU, and
they ranged from 450 to 3,292 FPS on identical code. That is not a measurement,
so the PR stayed draft until it could be redone.

Re-run as a **paired A/B**: three runs of each side, alternating
trees / `main` / trees / `main` / trees / `main` in one sequence on an otherwise
idle machine, so the clock-boost ramp that the caveat at the top of this file
warns about lands on both sides equally rather than on whichever ran second.

```
trees   1446 / 1435 / 1445 FPS    CPU/frame 0.480 / 0.494 / 0.478 ms
main    1460 / 1466 / 1461 FPS    CPU/frame 0.486 / 0.483 / 0.481 ms
```

**CPU/frame is flat: 0.484 vs 0.483 ms**, a difference smaller than either
side's own run-to-run spread. Trees cost the CPU nothing measurable — which is
what `trees_near`-hoisted-per-chunk was designed to buy, and it is the metric
this file trusts at this scene size.

The −1.4% throughput (~1,442 vs ~1,462) tracks the **+1.0% geometry**
(766,476 vs 758,754 triangles, 1,600 vs 1,585 nodes) almost exactly: the trees
are extra triangles, drawn at the same cost per triangle as everything else. A
proportional GPU cost for proportionally more world is the honest reading, not
a regression in the engine.

The first of the two rows is the **baseline row**, not a feature: this machine
had no radius-64 number since `da55704` on 2026-08-11, because the skirt fix
(#125), reversed-Z (#129), the mip chain (#128) and phase 2's blocks 2.1–2.2
were all recorded on the Windows machine. It collapses that whole gap into one
macOS measurement, and it is the figure the trees row is a delta against.
Against `da55704`'s ~1,275 FPS / 0.535 ms, `main` is now **~1,462 / 0.483 ms** —
the skirt fix's −8.5% geometry (829,608 → 758,754) doing most of that work.

Gate: **MET on both sides**, at ~1.44× the 1,000-FPS bar with trees in the
world.

³⁶ **Iron ore (#156) — the cost is greedy meshing, not the ore.** Same paired
A/B as ³⁵: three runs a side, alternating ore / `main` on an idle machine.

```
ore     1379 / 1381 / 1383 FPS    CPU/frame 0.510 / 0.505 / 0.512 ms
main    1451 / 1448 / 1438 FPS    CPU/frame 0.486 / 0.489 / 0.493 ms
```

**−4.5% throughput and +4.1% CPU/frame**, and unlike the trees row this one is
outside the run-to-run spread — it is a real cost. The cause is worth stating,
because the obvious explanation is the wrong one:

**Ore adds no blocks to the world.** It is a material substitution
(`PHASE2_ARCHITECTURE.md` §6) — a solid voxel becomes a different solid voxel,
and the solidity field is bit-identical with and without it (pinned by
`ore_never_changes_whether_a_voxel_is_solid`). Yet geometry rose **+7.3%**,
766,476 → 822,420 triangles.

That is entirely **greedy-mesh fragmentation**: a stone wall that merged into
one large quad now has ore blocks punched through it, and each one splits the
quad around it. Scattering ~1% of a material through a solid volume costs far
more than 1% of its geometry, because the merged neighbours are what pay.

The levers, if this ever needs to come down, are all in `assets/ores/iron.ron`
and need no recompile: fewer/larger veins fragment less than the same volume of
ore scattered widely, and `max_y` bounds the affected depth. None was pulled
here — the gate is **MET at ~1.38×** and the tuning that costs this is the
tuning that makes ore look like ore (see the file's own comment for how it was
measured).

Node count is unchanged at 1,600: same world, same chunks, more triangles
inside them.

³⁷ **Drops and tool tiers (#158) — measured to show it changed nothing.** This
block is inventory and registry work: it touches neither worldgen nor the render
path, so the scene should be identical, and it is — **822,420 triangles and
1,600 nodes, the same figures to the digit** as the ore row above it. 1,385 FPS
against that row's ~1,381, and 0.509 ms CPU/frame against 0.509.

Recorded rather than skipped because "it obviously cannot affect performance" is
exactly the assumption worth spending one run to check; the row's value is the
identical triangle count, not the FPS.

The same applies to **mining time (#159)** on the row below it: 822,420
triangles again, 1,380 FPS, 0.502 ms. Mining adds one `raycast` and a handful of
integer operations per tick *while the break button is held*, and `--bench` holds
nothing -- so the honest reading is that this row measures the unchanged scene,
not the feature. What the feature costs when actually mining is one raycast per
tick, which is the same raycast the block highlight (#52) already does every
frame.

**The furnace (#160)** is the third such row, and the same caveat applies twice
over: 822,420 triangles, 1,381 FPS, 0.499 ms. `--bench` places no furnace, so
the per-tick furnace loop iterates an empty `BTreeMap` and costs nothing
measurable. What it costs *with* furnaces is one `BTreeMap` walk plus a handful
of integer operations per furnace per tick -- worth measuring properly when
block 2.6/2.7 make dormant chunks a thing and a world can hold thousands of
them, which is exactly the scenario those blocks exist to make cheap.

**The ECS and dropped items (#56)** is the fourth: 822,420 triangles, 1,381 FPS,
0.505 ms. `--bench` drops nothing, so `Sim::tick_entities` returns on its
`is_empty` guard and `hecs` never allocates an archetype. The row's value is the
**identical triangle count** confirming a new dependency in `cubara-sim` changed
nothing about the scene -- and confirming the guard: adding an ECS to the tick
loop is exactly the kind of change that could have cost something per frame
whether or not there were entities, and it did not.

**The chunk state machine (#47)** is the fifth and the last of this run:
822,420 triangles, 1,375 FPS, 0.498 ms. It adds a per-tick
`update_simulation_radius` walk over a `(2r+1)² x 3` box -- 243 chunk lookups a
tick at radius 4 -- and that does not show either. Worth noting the direction:
this block makes the simulation cost *less* as a world grows, since a furnace
outside the radius stops ticking entirely. `--bench` has no furnaces, so what
this row shows is that the bookkeeping itself is free.

**Bounded catch-up (#58)** is the sixth, and the one where `--bench` is least
able to say anything: 822,420 triangles, 1,367 FPS, 0.496 ms. The whole point of
the block is the cost of *waking a chunk that slept for a long time*, and
`--bench` neither sleeps nor wakes anything. The measurement that matters is a
unit test instead: `catch_up_cost_does_not_grow_with_elapsed_time` advances a
furnace by **100,000,000 ticks** and finishes instantly, which the previous
per-tick loop could not have done at all. That is the number for this block, and
it is not an FPS.

**The save format (#170)** is the seventh and last of this run: 822,420
triangles, 1,387 FPS, 0.513 ms. Saving is not on the frame path at all -- it
happens on demand, and `--bench` never calls it -- so this row is once more a
statement that the scene is untouched rather than a measurement of the feature.
Seven consecutive rows at 822,420 triangles is itself the useful signal: every
block since iron ore changed simulation, not geometry.

**Health and fall damage (#172)** makes it eight: 822,420 again, 1,379 FPS,
0.496 ms. It adds two integer operations per tick (a fall-distance accumulate
and a regeneration counter) and a row of quads to the HUD, and neither shows.
The run of identical triangle counts is now long enough to be worth stating as a
property rather than a coincidence: **since block 2.3b, every change has been to
what the world *does*, not to what it draws.**

³⁸ **The world lost its height limit, and caves stopped being carved above
level 0.** Two rows because the second changes what the benchmark *measures*.

**The first row is the old scene**, so it is comparable with every row above it:
the same fixed 3-layer slab, with only §8.6's change (caves carved at `step == 1`
only). **822,420 → 675,962 triangles, 1,379 → 1,553 FPS.** An 18% geometry
reduction in a world with barely any rock in it, because coarse LOD nodes had
been sampling the cave field every 4-8 blocks and meshing the fragments. That is
not caves; it is noise, and §8.4 had already made exactly this call for trees.

**The second row is a different scene, deliberately.** The streamed band now
follows the player (±2 chunk-layers) instead of sitting at `0..=2`, so the
benchmark measures the world the game actually builds. Leaving it on the old slab
would have let the gate pass while the real thing failed it.

The vertical radius was **chosen by measurement, not preference**. Honest figures
at radius 64, caves already restricted:

| band | nodes | triangles | FPS |
|---|---|---|---|
| ±1 | 2,635 | 807,076 | 1,260 |
| **±2** | **3,138** | **912,964** | **1,115** |
| ±3 | 3,819 | 1,015,508 | 994 |
| ±4 | 4,260 | 1,096,202 | 923 |

±2 is the largest that clears the 1,000 gate with real headroom.

**A warning about the numbers this replaces.** Before `MAX_DRAWS` was raised from
4,096 to 16,384, bands of ±4 and above reported *higher* frame rates than ±3 --
because they exceeded the draw cap and simply stopped drawing nodes. A capacity
whose failure mode is a better benchmark score is the worst kind, and every
figure taken while over that cap was measured on a world with holes in it.

Going down is what costs: air meshes to nothing, rock is full of caves, and cave
surfaces are real geometry. Going *up* remains free -- `0..=2`, `0..=7` and
`0..=15` all measured identically.

³⁹ **Positions became integers, and it cost nothing.** 912,964 triangles and
3,138 nodes -- identical to the row above, which is the point: this changed how
positions are *represented*, not what the world contains. 1,109 FPS against
~1,115 is inside the run-to-run spread.

That is worth recording because integer arithmetic replacing floating point is
exactly the kind of change people assume is slower. It is not: the work is the
same additions and comparisons, and the collision sweep lost its epsilon skin
along the way -- `move_axis` no longer nudges every bound by 1e-4 before
flooring it, because an exact `pos <-> feet` round trip does not need the nudge.

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

⁴⁰ **The replica costs nothing measurable, and that was the thing to check.**
The client now holds its own `World` (`RESEARCH_MULTIPLAYER.md` §8.2), so the
worry was two of them: twice the terrain generation, twice the memory.

CPU/frame moved 0.622 → 0.628 ms, which is inside this scene's run-to-run
scatter. The reason it is free is the design's own argument made concrete:
**terrain is a pure function of the seed, so nothing is copied.** The replica's
`WorldGen` is the same cheap seeded noise, and it is only ever evaluated for
chunks the client actually meshes — which is exactly the set it was already
meshing. What the replica adds is an edit `BTreeMap` and a block-entity map,
both of which are empty on a fresh world and small on any world.

p99 moved 1.04 → 1.22 ms, and the honest reading is that this scene's p99 is
noisy rather than that a regression is hiding in it: the avg is what carries
signal at this scale (see ¹), and the per-frame work added is one drain of an
empty `Vec`.

⁴⁵ **Six PRs and a played multiplayer session later, and nothing measured
moved.** Both machines were re-run after `--connect`, player figures, per-machine
shirt colours, a large `game.rs` refactor and a mining retune had landed. Nodes
and triangles are identical on both; the scene has not changed since ⁴³.

| | CPU/frame before | after | FPS before | after |
|---|---|---|---|---|
| macOS M3 | 0.623 ms | 0.611 ms | ~1,109 | ~1,112 |
| Windows RTX 4060 | 0.136 ms | 0.124 ms | ~2,506 | ~2,774 |

**Read neither as an improvement.** Windows is +10.7% FPS, which sounds like
something until you notice this file has already recorded 2,469 and 2,746 on
functionally identical code — an 11% spread. This sits inside it. macOS moved
0.4%. The honest summary of a gate re-run is *no regression*, which was the
question, and that is what these numbers say.

**A separate thing this session established, which no row can show.** The game
window on the M3 runs at exactly 60 while `--bench` reaches ~1,112 on the same
machine. Those are not two measurements of the same thing: `--bench` renders
offscreen and never presents (`bench.rs` asks for an adapter with
`compatible_surface: None`), so it measures drawing, not delivery.

Three machines, asked what their surface offers:

| | offered | resolved to | window FPS |
|---|---|---|---|
| Windows RTX 4060, Vulkan | Fifo, FifoRelaxed, Mailbox, Immediate | Mailbox | ~5,000 |
| Linux GTX 1060, Vulkan | Mailbox, Fifo | Mailbox | ~2,000 |
| macOS M3, Metal | Fifo, Immediate | **Immediate** | **60** |

Both machines that get Mailbox run fast; the only one that gets Immediate sits
on exactly its refresh rate. So Metal's `Immediate` appears to present at display
sync in practice. Measured on three machines, not diagnosed on one, and **not
fixed** — it does not touch the gate, which never presents.

The code now picks a concrete mode from `caps.present_modes` instead of asking
for `AutoNoVsync`. That changes no number; it means the log can say what was
chosen, where reading `config.present_mode` back only ever echoed the request.

⁴⁴ **The first Windows measurement of *this* scene, and it must not be read
against the row above it.** The Windows table stopped on 2026-08-24 at 1,585
nodes and 758,754 triangles. This one is 3,138 and 912,964 — a different world,
because the height limit was removed in between (see ³⁸). Nodes roughly doubled;
comparing FPS across that is comparing two scenes.

Recorded because the gap nearly produced a false claim. Reading the last
*macOS* row (`bbb09c2`, 0.630 ms) as though it were the last Windows one made
0.136 ms look like a **4.6× speedup from fifteen blocks of server and netcode**,
which is not a thing that happens. The two numbers are two machines: an M3's
integrated GPU and an RTX 4060.

That is the same defect phase 1's closeout recorded as #123 — *"a benchmark row
sat in the wrong machine's table, which made a hardware difference read as an
unexplained 4× speedup on identical code"*. Second time, and the warning was in
the file both sessions were reading. Last time a row was in the wrong table;
this time the right table was read wrong. The lesson is the one this file
already argues, and it is apparently worth arguing twice: **a number without its
machine attached is not a measurement.**

The comparable macOS row is `397a653` at 0.623 ms. An M3 and an RTX 4060 being
4.6× apart on a submit-bound scene is unremarkable.

⁴³ **Five more blocks, and the scene is still untouched — and this is the last
row before phase 2's gate closed.** Prediction and reconciliation (2.13),
untrusted clients (2.14), server-side mining time, per-player atomic saves and
saving off the tick loop (2.15), and sharding (2.16).

3,138 nodes and 912,964 triangles again, identical to the four rows above.
CPU/frame 0.628 → 0.623 ms and 1,108 → 1,109 FPS are scatter.

Two of those blocks put real work into the tick and it does not show here, for
reasons worth stating rather than assuming:

- **Server-side mining** raycasts once per player per tick while a break button
  is held. The benchmark has one player who is not holding anything, so it
  measures the cost of the check and not of the work — and the check is a map
  lookup.
- **Saving off the tick loop** moves file writes to another thread. The
  benchmark never saves, so this row says nothing about it either way. The claim
  that the tick no longer waits for the disk is structural (`commit` runs on a
  spawned thread), not something these numbers support.

What none of these rows measure is the thing phase 2 actually changed: the cost
of *many* clients. There is one player in the benchmark, and interest
management, per-client views and shard handoff only start costing something when
there are clients and shards to have. Those have their own tests, and they
measure bytes and hashes rather than frames, because that is what those blocks
were built to bound.

⁴² **Three multiplayer blocks, and the scene is untouched — which is the whole
claim being checked.** Blocks 2.10 (the world holds many players), 2.11 (the
per-client view and interest management) and 2.12 (the transport) landed on
Windows; this is the first macOS measurement since `bbb09c2`, so it covers all
three at once.

3,138 nodes and 912,964 triangles, identical to the four rows above it. That is
the expected answer and it is worth having on the record anyway: none of these
blocks touch worldgen, meshing or the render path, so a change in either column
would have meant something had leaked across a seam it should not have.

CPU/frame 0.630 → 0.628 ms and 1,096 → 1,108 FPS are both inside this scene's
run-to-run scatter (see ¹ on why FPS ramps here). p99 reads 0.98 ms against the
previous row's 1.38, and that is scatter too rather than an improvement to claim
— this scene's p99 has swung between 0.86 and 1.38 ms across rows that changed
nothing about rendering.

What none of this measures is the cost of *many* clients. There is one player in
the benchmark, and per-client views only start costing something when there are
clients to have them. The scaling question has its own test
(`bytes_to_one_client_do_not_grow_with_the_player_count`), and it measures bytes
rather than frames, because bandwidth is the thing block 2.11 was built to bound.

⁴¹ **Replacing the platform's `sin`/`cos` with a polynomial costs nothing
measurable.** CPU/frame 0.628 → 0.630 ms, which is scatter.

That is not surprising once you count the calls: trigonometry runs **twice per
tick** (a look direction and a horizontal axis pair), or 120 times a second,
against ~3,000 chunk nodes of meshing and culling per frame. Four multiplies and
three adds, 120 times a second, is not measurable next to that.

Worth stating because the instinct is that a hand-rolled polynomial must be
slower than a hardware-assisted `sin`. It very well might be, per call — and it
does not matter, because this is not a hot path. What it buys is that two
machines cannot disagree about where a player is looking, which is not a
performance property at all.

⁵⁴ **The first Linux measurement — a third data point on the same scene
Windows and macOS already have (3,138 nodes, 912,964 triangles, radius 64,
band ±2, `2ab5fb6`).** This is issue #36's completion criterion: a Linux +
Vulkan benchmark baseline in this file, mirroring the existing Windows/Vulkan
and macOS/Metal rows. The machine is a laptop discrete GPU one tier below the
Windows RTX 4060 (a GTX 1060 Max-Q), on the same native-Linux Vulkan backend.

```
GPU: AdapterInfo { name: "NVIDIA GeForce GTX 1060 with Max-Q Design", vendor: 4318, device: 7200, device_type: DiscreteGpu, driver: "NVIDIA", driver_info: "580.178.04", backend: Vulkan }
SUMMARY: 1474 FPS | CPU/frame avg 0.214 ms (p99 0.883) | 2730/3138 nodes | 1000-FPS gate MET
```

Sitting between the two existing rows on the same scene — macOS/M3 ~1,109 FPS
/ 0.623 ms, this GTX 1060 ~1,474 FPS / 0.214 ms, Windows/RTX 4060 ~2,469 FPS /
0.136 ms — is the expected shape: a mid-tier discrete GPU between an
integrated one and a newer discrete one, all three clearing the 1,000-FPS gate
on the same, unchanged geometry. Nothing about the engine is being measured
here; the point of this row is that a third machine now exists to catch a
Vulkan-specific regression Windows alone would not.

⁴⁶ **Seven PRs from the owner's first long play session, and the measured path
did not change.** Upright side textures (#240), grass drops soil and wheel
scrolling (#241), screens release the mouse and no autojump (#242), crosshair
and item names (#243), a cobble block and tenfold fuel (#244), crack overlay
(#245), item icons (#246). The bench passes no HUD, no cracks and no icons, and
#240 changes only which corner of a quad gets which texture coordinate -- same
vertex count, same geometry (3,138 nodes, 912,964 triangles, identical to the
row above). Three back-to-back runs, showing the usual warm-up ramp this file
warns about:

```
SUMMARY: 2466 FPS | CPU/frame avg 0.145 ms (p99 0.522) | 2730/3138 nodes | 1000-FPS gate MET
SUMMARY: 2637 FPS | CPU/frame avg 0.137 ms (p99 0.482) | 2730/3138 nodes | 1000-FPS gate MET
SUMMARY: 2674 FPS | CPU/frame avg 0.131 ms (p99 0.482) | 2730/3138 nodes | 1000-FPS gate MET
```

The row records the last. Against `9dee4a6` that is +0.007 ms CPU/frame
(+6%) and ~-100 FPS, still falling run over run -- inside the spread this
machine has shown on unchanged geometry (0.124-0.136 ms across ⁴⁴ and ⁴⁵), and
not attributable to code the bench executes. Read it as unchanged.

⁴⁷ **The first optimisation from measuring where the triangles go, rather than
from drawing less.** `crates/world/examples/geometry_census.rs` sorted every
quad of this scene: **27.9% faced a solid cell** -- walls where two solid nodes
meet, emitted because each node was meshed without looking at its neighbour --
23.3% were cave walls, 48.8% surface. Nodes now leave out a border face when
the outside cell is solid for every level of detail a neighbour could be drawn
at (same, one coarser, one finer), so no neighbour can open a hole the face
would have closed (`where_two_nodes_meet_every_visible_solid_cell_has_a_face`
walks shared planes at every level pairing; removing either the coarser or the
finer check makes it find a hole).

```
before  SUMMARY: 2674 FPS | CPU/frame avg 0.131 ms (p99 0.482) | 2730/3138 nodes   912,964 tris
after   SUMMARY: 4160 FPS | CPU/frame avg 0.102 ms (p99 0.440) | 1671/1969 nodes   674,484 tris
after, 3840x2160: 2726 FPS (was 1539)
```

Triangles -26%, and **1,169 nodes had nothing left to draw at all** (solid rock
below the surface), so draws fell 37% too. FPS +56% at 1080p and +77% at 4K,
where the removed faces had also been costing fill. Surface triangles are
unchanged (445k before, 451k after), which is the check that nothing visible
went. Meshing costs +19% single-threaded (1.38 s -> 1.64 s for the region), on
the worker pool.

⁴⁸ **Slower, because it draws terrain that was missing.** The level rings were
resolved each on its own grid, excluding the finer ring by a rounded-down node
radius: 603 of the 14,641 chunks within 60 of the player belonged to no node
(chunk 11 and chunks 36-39 out, all the way round) and 148 to two. The
`lod_boundary` golden had shown the gap for as long as it existed, as a trench
with grey walls; once covered border faces were left out (⁴⁷) it became a hole
through the ground. The rings are now an octree -- coarse nodes split into
eight children while they reach inside the finer ring -- so every chunk
belongs to exactly one node, and a test says so at four positions.

```
before  SUMMARY: 4160 FPS | CPU/frame avg 0.102 ms | 1671/1969 nodes   674,484 tris
after   SUMMARY: 3885 FPS | CPU/frame avg 0.107 ms | 1831/2174 nodes   738,078 tris
```

+9% triangles and +10% nodes are the filled gaps, less the removed overlaps.

⁴⁹ **The world is drawn in every direction now, and the default bench measures
that.** The ±2 chunk-layer band is gone: the outer radius is a cube, and detail
coarsens twice as fast vertically as horizontally (`VERTICAL_LOD_SQUASH`). The
bench's default follows the game; `--band` reproduces the rows above. New
`--eye X,Y,Z` puts a first-person camera somewhere real, turning on the spot,
because the orbit above the region is a view no player has.

```
default (orbit, 3D)   SUMMARY: 2964 FPS | CPU/frame avg 0.128 ms | 3684/4377 nodes   1,015,554 tris
--band  (orbit)       SUMMARY: 4121 FPS | CPU/frame avg 0.104 ms | 1831/2174 nodes     738,078 tris
--eye 8,40,8          SUMMARY: 6138 FPS | CPU/frame avg 0.085 ms                       877,392 tris
--eye 8,300,8         SUMMARY: 12860 FPS | CPU/frame avg 0.051 ms                      237,844 tris
--eye 8,-150,8        SUMMARY: 7473 FPS | CPU/frame avg 0.086 ms                       972,218 tris
--band --eye 8,300,8  0 nodes -- the bug: 300 blocks up, nothing drawn at all
```

The orbit sees the whole region at once and is the worst case; it pays ~28%
over the band for drawing what the band left out. How the squash was chosen,
same four views (FPS / triangles):

| view | squash 2 | squash 4 | squash 8 |
|---|---|---|---|
| orbit | 2,894 / 1.02M | 3,760 / 747k | 3,364 / 744k |
| eye at y=40 | 5,603 / 877k | 6,293 / 611k | 5,497 / 594k |
| eye at y=300 | 12,541 / 238k | 15,289 / 129k | 15,654 / 129k |
| eye at y=-150 | 5,096 / 972k | 6,985 / 487k | 6,203 / 352k |

4 is faster, but caves exist only at full detail and it keeps that only 40
blocks up and down, so the bottom of a shaft turns to solid rock; 2 keeps 80.
An earlier version squashed the *radius* too, and at squash 8 a camera 300 up
drew nothing -- what is drawn is a cube, only how finely is squashed.

Meshing the whole default region single-threaded: 1.85 s with the band, 4.96 s
now, most of it rock that meshes to nothing; nodes wholly above the tallest
generated block are skipped without generating.

⁵⁰ **What cannot be seen is not generated, meshed, uploaded or drawn.** A
search outward from the camera's node crosses from node to node only where air
inside joins the faces (`FaceLinks`), and never steps against a direction it
has already taken -- which no straight line of sight does, so nothing visible
is lost (`every_node_a_line_of_sight_hits_is_visible`: ~1,200 rays each from three
cameras through a world with caves and levels of detail). Radius 64, first-person
views, squash 2:

```
view            before (no culling)                 after
eye y=40        6138 FPS, 877,392 tris              7310 FPS, 615,408 tris   9,593 of 16,330 nodes reachable (most are sky)
eye y=300       12860 FPS, 237,844 tris             12848 FPS, 237,844 tris  nothing below to hide
eye in a cave   --                                  6732 FPS, 1,524 tris     11 of 16,330 nodes
eye in rock     7473 FPS, 972,218 tris              12985 FPS, 28 tris       7 of 16,330 -- nothing to see
```

The orbit view sits outside the region, where there is no node to search from,
and draws everything as before. The bench generates every node to measure; the
game generates only the ones the search reaches, which is the part that makes
caves at every level of detail affordable next.

⁵¹ **A cave you could see from a mountain is there from a mountain.** Coarse
cells now take the majority of their blocks (terrain judged at the middle of the
cell, caves by the majority of eight samples, the top cell wearing grass), and
caves are carved in coarse nodes down to `DISTANT_CAVE_DEPTH` = 24 blocks below
the surface: the mouth and the start of the passage. Deeper, a distant node is
rock until the player comes close.

The owner chose that depth limit after the first version, which carved caves at
every depth in every node, halved the gate's orbit (1,465 FPS, 1.99M triangles:
through the region's cut edges it drew the whole volume's caves) -- "in de verte
niet super gedetailleerd en tot heel diep". Three alternating runs against
`main` (with mountains), median FPS:

```
view            main                         this
orbit           3480, 592,540 drawn          3920, 480,547 drawn
eye y=40        5919, 147,724                6239, 167,988     (noisy, CPU-bound)
eye y=300       8265, 89,505                 5056, 102,664     (range 5002-6745)
```

The bench now fails outright on an exhausted arena or an empty scene instead of
reporting the frame rate of a world with parts missing. The arena stays at 4M
vertices: the peak is 1.8M.

⁵² **A face pointing away from the camera is no longer sent to the GPU at all.**
Back-face culling already kept those triangles off the screen, but only after
every one of their vertices had been through the vertex shader. Meshes now come
grouped by face direction (`Mesh::group_by_face`), and a node draws only the
directions the camera could be in front of (`arena::faces_facing`): one of each
opposite pair for a node the camera is outside of. The same technique Sodium
calls block face culling; Nick McDonald measured 22% with it.

Five alternating runs of `main` and this branch, median FPS:

```
view               main     this     triangles drawn (orbit)
orbit              2991     3592     1,015,554 -> 573,298
eye y=40           4893     4881
eye y=300          12923    13002
eye y=40, 4K       3244     3231
```

+20% where the GPU's vertex work is the limit (the gate's orbit, and on the
M3 more of the frame is), and no change where the CPU is. CPU per frame rises
0.122 -> 0.168 ms: the per-direction draw list. It is still not the limit.

⁵³ **Mountain ranges in the height field.** Ranges hundreds of blocks across rise
out of the hills (ridged noise under a broad region mask), peaking around y = 240
with cliff steps of up to 7 blocks; ~20% of the world is raised, ~4% above
y = 150 (`mountain_census` example). The default seed's spawn stays in the hills
and the nearest range starts ~370 blocks off, which keeps every test framed on
the origin unchanged -- no golden image or pinned hash moved.

Three alternating runs against `main`, median FPS:

```
view               main     mountains
orbit              3721     3451     (1,015,554 -> 1,110,830 triangles; 4,377 -> 4,506 nodes)
eye y=40           5009     5859     (noisy, CPU-bound)
eye y=300          12907    7949     there is now something to see from up there: 55k -> 90k triangles drawn
```

`radius_64_smoke` settles in 20.6 s locally.

⁵⁵ **The M3 catches up, and the gate's tightest machine gains the most.** The
macOS table had not moved since `b5dcfec`; everything between it and `3b52c49`
was aimed at drawing less -- covered border faces left out (#250), rings that
tile exactly (#251), a 3D octree with no vertical band (#252), visibility
culling (#253) and its sub-block search off the main thread (#254), faces turned
away from the camera left out (#257) -- and then two features that add geometry
back: caves at every level of detail (#255) and mountains (#259).

Three back-to-back runs of `--bench 64`, same machine, nothing else running:

```
                  b5dcfec          3b52c49
FPS               ~1,112           1545 / 1568 / 1587
CPU/frame avg     0.611 ms         0.442 / 0.439 / 0.441 ms
CPU/frame p99     ~1.08 ms         0.976 / 0.940 / 0.707 ms
nodes             3,138            2,219 meshed, 1,782 drawn
triangles         912,964          901,932 meshed, 480,547 drawn
```

**CPU per frame is the comparable number** (see ¹): **0.611 -> 0.441 ms, -28%**,
on a scene of almost exactly the same triangle count -- 901,932 against 912,964.
Of those, 480,547 reach the GPU, which is what #257 bought and what an M3 feels
most: the gate's orbit was the view where more of the frame is vertex work. FPS
rises across the three runs as the clocks ramp, so ~1,568 is the median, not a
peak.

The phase-1 gate is **12/12 PASS** here at `3b52c49`, its own `--bench 64` step
reporting 1,576 FPS. That moves the M3's margin over the 1,000-FPS criterion
from **~1.11x** at the phase-2 closeout to **~1.57x** -- and the M3 is the
machine that decides whether the gate holds, being the only one of the three
without a discrete GPU.

Arena occupancy at this scene, recorded because
`docs/PROPOSAL_VERTICAL_WORLD.md` §2.1 names capacity as the binding constraint
on taller worlds: **v 1,803,864/4,000,000, i 2,705,796/6,000,000, d
2,219/16,384** -- 45% of the vertex arena and 14% of the draw slots, with
mountains already in the world. Nothing is being silently dropped.

The visibility search costs 9.2 s for the bench's 65 camera positions, off the
render path and before the measured frames; it is not in the numbers above.

**What this machine does under sustained load, measured because the phase-2 gate
went red on it at this very commit.** Same binary, same commit, and every run
drew the identical scene (1,782 of 2,219 nodes):

```
condition                                   FPS               CPU/frame   p99
cold, machine idle                          1540 1568 1582 1587   0.441 ms   0.70-0.98
partway recovered                           1100                  0.477 ms   0.81
immediately after `cargo test --all`          679  861             0.546-0.674  1.40-1.50
inside check-phase-gate.sh 2 (two runs)       623  681             --         --
```

The scene does not change, so this is the machine: an M3 with 8 GB throttling and
paging (436k pageouts over the session), both its CPU and its GPU side degrading
together. Windows measured 3,982 FPS and 12/12 on the same commit. `b5dcfec`
passed this gate at a *lower* cold number (1,112), which is the clearest sign
that hot-vs-cold dominates the scene: the same 2.3x factor would have put it
under 500.

`check-phase-gate.sh` takes **one** `--bench 64` run, placed immediately after
`cargo test --all` and clippy -- which on this machine is the worst available
moment. Recorded here rather than fixed: what a gate asserts is the project
owner's to change, never an agent's (`CLAUDE.md`), so this note is the evidence
for that decision and not the decision. Phase 2's other 15 criteria pass on this
commit, including the survival replay and all five multiplayer ones.

Kept from the superseded copy of ⁵¹ that the duplication carried, because it is
measured and recorded nowhere else: **how far the visibility search still is from
what is really seen** (`visibility_grain` example, surface view) — the sub-block
search keeps 1.25M triangles, while the nodes ~39k actual rays hit hold 326k.
That gap is what screen-space occlusion would close, and it was the open question
in [#256](../../pull/256): the hi-Z pyramid was a net loss on an RTX 4060 once
#257 landed, and its own conclusion was that the M3 — GPU-bound, tile-based, no
discrete card — is the machine that decides.

**Measured here, and the answer is no.** `e5ed61a` cherry-picked onto `3b52c49`
(branch `perf/occlusion-on-the-m3`, all render tests green including
`occlusion_culling_never_changes_the_image`), alternating runs of one binary with
and without `--no-occlusion`, median FPS:

```
view                  occlusion ON    OFF       triangles: face-culled -> occluded
orbit (the gate)      1035            1570      480,547 -> 445,896   (7% saved)
eye y=40              2020            3862      167,988 -> 150,516   (10%)
eye y=300             2672            5106      102,664 -> 101,063   (1.6%)
eye y=40, 4K           796            2020      167,988 -> 152,668   (9%)
```

The pyramid culls correctly; there is nothing left for it to find. #253/#254 and
#257 already took that ground, on the CPU, for free -- on the 4060 before #257
the same code saved 31%. And the cost is *worse* on a weak GPU, not better: the
pyramid is itself GPU work that scales with pixels, and at 4K it takes 60% of the
frame. CPU/frame doubles where the readback is not hidden (0.160 -> 0.333 ms at
y=40; 0.344 -> 0.868 ms at 4K).

**The number that settles it: with occlusion on, the gate orbit is ~1,035 FPS**
against a 1,000-FPS criterion -- the margin in this row collapses from ~1.57x to
~1.04x, on the machine that decides the gate. A 7% triangle saving does not buy
that. Screen-space occlusion is not the way to close the 1.25M-vs-326k gap; a
cheaper search is.

⁵⁷ **Bench measurement tooling, package 1 -- the M3 row.** Measured on
`a539938` (final PR #266 head), idle machine, lid open, `--gpu-timing off`;
three orbit runs within 1,577-1,585 FPS (this row uses the median). `+0.02 ms`
over the `3b52c49` row above (0.441 -> 0.468 ms) is `set_camera` + the
frustum build moving inside the timed window, exactly as intended -- not a
regression, the bench counting CPU cost it was previously excluding.

`GPU/frame` is `n/a` here, deliberately: on Metal, `RenderPassDescriptor`'s
`timestamp_writes` resolves every sample invalid (`end` always exactly `0`,
`begin` plausible and advancing -- the end-of-pass sample is never written or
never resolved) and the sample-buffer-attachment machinery is not free even
though it produces nothing usable (~20% fewer FPS, +~30% CPU/frame measured
with it forced on). Filed as **#267**, linked to H11 (the wgpu 24 -> 30
upgrade) since Apple counter-sampling support has been revised in later wgpu
releases.

**`--gpu-timing auto` (the default) confirmed working on `a539938`**: seven
M3 views, all seven correctly reported "disabled after warmup" with FPS/CPU
matching this row (e.g. orbit 1080p 1568-1589 FPS / 0.466-0.474 ms). Getting
there took two more rounds after the first attempt: a single
`Maintain::Wait` doesn't reliably fire every pending `map_async` callback on
Metal within warmup's short window (fixed by polling further,
`GpuTimer::drain_after_wait`), and a broken backend can still produce a
handful of coincidentally non-zero "valid"-looking samples per 200-frame
warmup (1-7 seen across the seven runs) that are not real readings -- so
`--gpu-timing auto` only stays enabled when warmup was *entirely* clean
(zero invalid samples), not merely "at least one valid" (see
`should_disable_gpu_timing` in `bench.rs`).

Other M3 views at the same commit, `--gpu-timing off`, GPU/frame n/a
throughout:

```
view                     FPS    CPU/frame avg (p99)   draws (nodes)
orbit 480x270           1789    0.415 ms (0.962)      3945
orbit 3840x2160         1362    0.546 ms (0.993)      3945
eye 8,40,8 1080p        3577    0.215 ms (0.712)      1325 (592/1940)
eye 8,40,8 480x270      5455    0.137 ms (0.266)      1325
eye 8,40,8 3840x2160    2069    0.363 ms (0.633)      1325
```

⁵⁶ **Bench measurement tooling, package 1 of the cross-session engine review.**
Not directly comparable to the 2026-09-11 row above: that one used `--band`
(the old fixed ±2 slab, 3,138 chunks); this one is the plain default, which is
the same squash-streamed scene the M3 rows above measure (2,219 meshed nodes,
1,782 drawn, 901,932 triangles meshed / 480,547 drawn) -- so it lines up with
those, not with this table's own prior row.

Linux (i7-8750H, GTX 1060 Max-Q, Vulkan 580.178.04) had no reference GPU/frame
number to compare against before this package, since the bench did not measure
one. Three back-to-back `--bench 64` runs on the final commit (`401a9e5`),
nothing else running:

```
FPS               1961 / 1876 / 2073
CPU/frame avg     0.268 / 0.289 / 0.260 ms
CPU/frame p99     1.016 / 0.968 / 0.888 ms
GPU pass avg      0.481 / 0.518 / 0.431 ms
GPU samples       1,024 of 2,000 measured frames (~51%) each run
draws             3,945 (of up to 3 x 1,782 = 5,346 possible)
peak RSS          ~1.0-1.4 GiB (grows with window size; not yet a table column)
```

GPU/frame (0.43-0.52 ms) exceeds CPU/frame (~0.27 ms) at this resolution on this
GPU -- the mobile 1060 is the bottleneck at 1080p here, unlike the M3 rows'
"CPU/frame moves with resolution" story above (¹⁴/⁵⁵-adjacent): on this machine
it is the *GPU* pass, not backpressure on submit, that grows with pixels (0.53
ms at 1080p up to 0.98 ms at 4K in the same session, CPU/frame flat at
0.26-0.34 ms throughout) -- which package 1 exists to be able to say for the
first time.

The 1,024/2,000 sample rate (not 2,000/2,000) is `GPU_TIMER_DEPTH`'s ring
running out of free slots faster than the GPU retires work -- the CPU submits
much faster than the GPU completes each frame's pass (the whole point of
"sustained pipelined throughput"), so most frames find every ring slot still
waiting on an earlier readback. Getting closer to full coverage is left for
package 2; the average/p99 above are already stable at this sample count.

Thermal check (`nvidia-smi`, per-run): 60 degC / 139 MHz idle before, 72 degC /
1,594 of 1,670 MHz boost after three runs, `hw_thermal_slowdown` and
`sw_thermal_slowdown` both `Not Active` throughout. Lid open, machine on a desk,
not throttling -- the spread above is ordinary submit-bound noise and clock
ramp (¹), not degradation.

`check-tests-can-fail.sh` on this package's final diff: 23 mutable lines, 3
survivors after six rounds of fixes -- see the PR for the full table,
including two bugs the mutation check itself surfaced along the way (a CI
failure on software GPU adapters, and a genuine test hang traced to an
unbounded `Maintain::Wait`). The three left: `main.rs`'s `--overlay`
flag-detection (consistent with every sibling flag's same untested shape),
`GpuTimer::begin_read`'s map-error guard (would need a real GPU map failure
to exercise), and one `gpu_line` message-wording branch reachable only via
`--gpu-timing on` forced against a broken backend during actual measurement.

⁵⁸ **`FrameUniform` + distance fog + one sun (package 2).** Fog on
throughout this row (`Lighting::fog_range` from `--bench`'s own
`view_radius`) -- there is no `--fog off` toggle, so the comparison against
⁵⁷'s fog-free baseline is package-to-package, not a same-commit flag flip.
Three back-to-back `--bench 64` orbit runs on `c34101e`: 1540/1762/1869 FPS,
CPU/frame 0.427/0.289/0.280 ms, GPU/frame 0.636/0.540/0.474 ms (all
1024/2000 samples, 0 invalid) -- the first run's higher numbers are the
usual cold-boost-clock pattern (¹), not fog; the table row uses the middle,
representative run.

Also measured, all fog-on: 4K orbit 911 FPS / 0.360 ms CPU / 1.049 ms GPU
(gate not met, as at radius 64 before this package); `--eye 8,40,8` 1080p
2378 FPS / 0.223 ms / 0.346 ms; `--eye 8,40,8` 4K 814 FPS / 0.332 ms /
1.103 ms. Every GPU/frame number here sits inside the noise band ⁵⁶ already
documented for this machine without fog (1080p orbit was 0.43-0.53 ms, 4K
~0.98 ms) -- one `distance()` and one `mix()` per fragment costs nothing
measurable, as expected for a package that adds no new geometry or draws.

New golden `fog_over_the_far_ring` (`crates/render/tests/golden.rs`) is
blessed on this machine -- Linux/Vulkan -- not the Windows/Vulkan the rest
of this file's goldens are blessed on (no Windows machine available this
session). Flagged for the cross-session review to confirm the cross-backend
delta once CI runs; every *other* golden, including the figure one, stayed
byte-identical despite figure.wgsl's sun direction actually changing, so no
re-bless was needed there.

Time-of-day (the original package-2 spec's item 5) is not in this row:
`FrameUniform` carries the field, fixed at `0.0` and unused, but the actual
day-length/night-existence question is gameplay, not engineering -- flagged
to the review and the owner rather than guessed at, deliberately left out
of this package's scope.

⁵⁹ **Package 2's fog, fixed for the M3 (cross-session review).** The peer
session's interleaved A/B on the M3 found ⁵⁸'s fog costing ~40% FPS
(1567→928 orbit) and the 1000-FPS gate failing on `main` there -- traced to
the `world_pos` varying's interpolation cost on Apple's tiled GPU, not the
fog math itself. `@builtin(position).w` is `1/w_clip` in the fragment stage
(a WGSL/WebGPU guarantee, unaffected by `reverse_z`'s z-row-only flip), and
for this projection `w_clip` *is* view-space depth -- so `1.0 /
in.clip_pos.w` recovers exactly what `world_pos` was there for, with no
extra varying. Same idea applied to the face normal: greedy-meshed faces
never bend across a triangle, so the interpolated-then-renormalized
`normal: vec3<f32>` became a flat `face: u32` indexing the same
`FACE_NORMALS` table -- less varying traffic for an identical answer
(byte-identical on every golden).

Three commits, three same-commit `--fog off`/`--fog on` measurement points
(the tool this fix adds first, so the A/B has no cross-commit noise):
`--bench 64` orbit, this machine (Linux/Vulkan, where the tiled-GPU cost
this fix targets does not exist, so these are a no-regression check, not
the fix's own evidence):

| commit | fog off (FPS / CPU / GPU) | fog on (FPS / CPU / GPU) |
|---|---|---|
| `9fae678` -- `--fog` flag added, shaders untouched | 1959 / 0.273 / 0.475 ms | 2062 / 0.266 / 0.427 ms |
| `bcad2b8` -- fog from `clip_pos.w`, `world_pos` gone | 1951 / 0.271 / 0.475 ms | 1919 / 0.285 / 0.493 ms |
| `d16d3e0` -- flat `face: u32`, no more `normal` varying | 1987 / 0.265 / 0.472 ms | 2086 / 0.265 / 0.419 ms |

Every number across all three commits and both flag states sits inside the
noise band ⁵⁶ already documented for this scene on this machine
(GPU/frame 0.43-0.53 ms at 1080p orbit) -- as expected, since the cost this
fix removes is specific to tile-based deferred rendering and this is an
immediate-mode desktop GPU. Also measured at `d16d3e0`, fog on: 4K orbit 982
FPS / 0.340 ms / 0.968 ms (gate not met, as at radius 64 before package 2);
`--eye 8,40,8` 1080p 2600 FPS / 0.222 ms / 0.321 ms; `--eye 8,40,8` 4K 822
FPS / 0.317 ms / 1.089 ms -- all consistent with, and slightly better than,
⁵⁸'s package-2 figures at the same views.

Golden `fog_over_the_far_ring` is re-blessed (radial → planar fog changes
the image at the screen edges: off-axis pixels have `distance > depth`, so
the plane fades in slightly later than the radial version did there, never
earlier -- visually confirmed before blessing, not just diffed). Every
other golden, including `figure.wgsl`'s output (unchanged, keeps radial
fog and the `normal` varying -- see the PR), stayed byte-identical; the
flat-face commit alone is byte-identical on *all* goldens including this
one, confirming it changes cost, not output.

**M3 re-run (peer session, interleaved, two rounds, `--bench 64` orbit) --
the fix confirmed on the machine it targets:**

| commit | FPS | CPU/frame |
|---|---|---|
| `1fcd449` -- before package 2's fog (≈⁵⁷) | 1565 / 1577 | 0.475 / 0.468 ms |
| `9a840dd` -- package 2 on `main` now | 939 / 939 | 0.785 / 0.785 ms -- **gate NOT MET** |
| `bcad2b8` -- fog from `w_clip`, `world_pos` gone | 1103 / 1109 | 0.670 / 0.667 ms |
| `d16d3e0` -- flat `face: u32` too | 1202 / 1207 | 0.612 / 0.614 ms -- **gate MET** |

Same-commit `--fog off`/`on` at `d16d3e0`: 1207/1194 FPS off, 1208/1213 FPS
on -- fog now costs nothing measurable on the M3 either, which is what the
toggle exists to show. The `world_pos` removal (commit 2) recovered 0.118 ms
of the 0.310 ms package 2 added; the flat face index (commit 3) recovered
another 0.055 ms. ~0.19 ms/0.14 ms (wall/CPU) is still unaccounted for
against the pre-package-2 baseline -- not fog (off/on is identical) and not
varying count (this branch's head carries *fewer* varyings than pre-package-2
had: 1 flat u32 + 1 linear f32 + 1 f32 + 1 flat u32, vs. the old shader's
plain `Camera` uniform reading compile-time-folded lighting literals). The
peer session's read: the remainder is the real cost of *dynamic* lighting --
reading `frame.sun_dir`, `frame.ambient.*`, `frame.fog_color`, `frame.fog.*`
etc. from the uniform every fragment where the old shader had them as
literals the compiler folded away, plus `tex.rgb * frame.sun_color.rgb` (a
vec3 multiply that used to vanish because the sun was hardcoded white), plus
the bind-group visibility going `VERTEX` → `VERTEX_FRAGMENT`. Two follow-up
experiments were proposed for this (flat `vec3` normal instead of flat u32 +
array-index, since a dynamic array index can compile to a real load or a
select chain on Metal; and WGSL `override` pipeline constants for
`ambient_low/high`/`ao_floor`/`diffuse_weight`, which never change mid-run,
so they should not be uniform reads at all) -- deliberately **not** in this
PR. This one fixes the measured regression and gets the gate back over 1000;
the constant-folding question is real but separate, and belongs in its own
PR with its own before/after, not folded into a fix that already has three
commits and two machines' worth of numbers to keep straight.

A fourth commit landed after this row was first measured: Windows CI caught
a `wgpu` 24 DX12/naga quirk where `@builtin(position).w` in the fragment
stage is not `1/w_clip` on that backend the way the WGSL spec (and Vulkan,
and Metal) says it should be -- see the shader comment and that commit's
message for the full story. The fix reads `1/w_clip` off a plain vertex-
shader-computed varying instead of the position builtin. Byte-identical on
every Linux/Vulkan golden -- but it *does* move the M3 numbers, just not the
way a first guess here assumed: an `@interpolate(linear)` `f32` varying is
still a varying, and the peer session's re-run measured it costing 80 FPS /
0.043 ms on that machine (`d16d3e0` 1206/1207 FPS, 0.613/0.612 ms →
`28564f8` 1128/1126 FPS, 0.656/0.656 ms) -- right in line with R16's
"one float costs about a sixth of what three did" model. Gate still cleared
with room (1126 > 1000), and a correct fog value everywhere is worth more
than 80 M3 FPS, so this ships -- but the number belongs here rather than
the "not expected to change" claim this replaced, which was wrong.
`--fog off`/`on` at the head is still identical (1121/1121), confirming the
regression this PR fixes is still fixed; only the DX12 workaround's own
small cost is new. A third follow-up experiment now joins the other two for
the separate PR: read depth from `@builtin(position).z` (the depth-buffer
value, unaffected by naga's `.w` bug, backend-invariant almost by
definition) via the render.rs:163-168 projection's closed form
`view_depth = A / (clip_pos.z + B)` with `A = near*far/(far-near)`,
`B = near/(far-near)` as two uniform scalars -- no varying at all, if the
Windows golden confirms `.z` doesn't carry its own version of the `.w` bug.

⁶⁰ **Package 4: recovering ⁵⁹'s M3 varying cost, and an occupancy cliff
found while trying to recover the rest.** ⁵⁹'s DX12 varying fix left the M3
at ~1128 FPS against ~1550-1580 before package 2's fog existed at all --
~400 FPS still missing. Goal for this package: get as much of it back as
the review's spec called for (three commits, three measurement points),
without reintroducing the DX12 bug ⁵⁹ fixed.

**Shipped, this row:** fog depth from `@builtin(position).z` instead of the
`@interpolate(linear)` `inv_w` varying ⁵⁹ added. `.z` is the rasterizer's
actual depth-buffer value -- not a value naga's DX12 backend mishandles
(that bug was specifically `.w`'s raw-vs-reciprocal mixup) -- and already
per-fragment via the hardware depth interpolant every pipeline computes
regardless, so recovering view depth from it costs zero varyings.
`view_depth = A/(clip_pos.z + B)` with `(A, B)` derived from the same
near/far the projection uses (`reverse_z_depth_constants`), pinned against
the real matrix by a unit test. Confirmed correct on Windows/DX12 by CI
(the machine that caught the `.w` bug in the first place) before being
trusted. M3: `28564f8` (⁵⁹, varying) 1128/1126 -> `dad0dd6` (`.z`, this row)
1154/1155/1160 -- **+26-32 FPS**, smaller than the varying's own ~78 FPS
cost because `.z` isn't quite free either (see below), but net positive and
correct everywhere.

**Tried and dropped, with the numbers as the reason (this project's rule):**

- *Ambient/diffuse-weight as `override` pipeline constants* (every
  `Lighting` call site leaves them at `Lighting::default`, so in principle
  the compiler could fold them the way it did before `Lighting` existed).
  Measured zero gain on **both** machines: M3 `d14bfa7` 1124/1128 vs
  baseline 1130/1123 (no difference); this machine's `--fog on` GPU/frame
  1959-2068 either way. Apple's GPU apparently loads a uniform once per
  wave and shares it, making a uniform read as cheap as a constant already
  -- the "lost constant folding" theory this was built on doesn't hold.
  Traded runtime flexibility (ambient becomes pipeline-time; a future
  day/night feature would have to undo it) for nothing measurable. Dropped.

- *Flat `vec3` normal, resolved in the vertex stage instead of a per-fragment
  `FACE_NORMALS[in.face]` array index.* An isolated A/B/C throwaway (hardcode
  the fragment-stage index away entirely, wrong image, purely to measure)
  found the dynamic index itself costly on both machines: M3 +29 FPS
  (1157->1185), this machine +12-13% of GPU/frame (0.481->0.420 ms,
  reproducible over 3 rounds). Moving the lookup to the vertex stage should
  have recovered a similar win with a correct image -- instead it measured
  **~90 FPS *slower*** on the M3 (`fd394d9` 1063/1068/1065 vs `dad0dd6`
  baseline 1154/1155/1160, consistent over 3 rounds), while staying flat on
  this machine (no signal either way, GPU/frame in the same noise band as
  baseline). The isolated A/B/C win came from removing the index; this
  commit removed the index *and added a flat `vec3` varying* to carry the
  now-precomputed normal across, and on a tile-based GPU a varying is
  expensive enough to cost more than the index saved. Dropped -- the
  "obviously better" version of an optimization needs its own number, not
  just the number from the diagnostic that inspired it.

- *Fog with pre-divided clip-space thresholds* (`z' = A/d - B` computed on
  the CPU for `fog_start`/`fog_end`, `smoothstep` directly on `clip_pos.z`,
  zero fragment-side division). M3: 1162/1172/1157 against a `dad0dd6`
  baseline of 1157/1158/1136 -- **+5 FPS**, noise-level. The division
  wasn't the cost. Changes the fog ramp's shape too (linear in depth-space,
  not distance-space) for no measured benefit. Dropped.

**The occupancy cliff.** Chasing the remaining ~290 FPS gap with more
diagnostics (fs_main with the fog block compiled out via a real `if`, not
`select` -- WGSL evaluates both `select` branches, so `--fog off` never
actually removed the fog math before this: +48 FPS, M3 1205 vs 1157) led to
reverting `fs_main` to its exact pre-package-2 form (`1fcd449`): no `frame`
reference anywhere in the fragment stage, every lighting value a literal,
no fog. That alone landed at **1526-1577 FPS** -- statistically level with,
or slightly above, the 1550-1580 FPS this scene ran *before fog existed at
all*. The entire ~400 FPS gap lived in the fragment stage touching the
uniform buffer, not in any specific field it read or any specific
computation on those fields.

Two more throwaways on top of that pinned it down further:

| variant | what it does | M3 FPS (3 rounds) |
|---|---|---|
| F | literal lighting, no fog, no `frame` in `fs_main` | 1579 / 1584 / 1578 |
| G | literal lighting + fog via one interpolated `f32` (computed per-vertex) | 1390 / 1408 / 1406 |
| H | lighting *and* fog entirely as `override` constants, `fs_main` never touches `frame` | **1745 / 1742 / 1708** |

H is **12% faster than the scene ever ran, fog included** -- ⁵⁹'s and this
row's `.z`-based depth read is in there too, so the ~48-78 FPS `.z`/`.w`
question above turns out to be a symptom of the same cliff, not a cost of
`.z` itself: once the fragment stage is back under whatever threshold this
GPU has, everything gets cheap again, including `.z`. G (fog as a single
extra interpolated scalar) costs ~340 FPS versus H -- one varying, again,
is not a small thing here.

Three small changes (B/C/D above: +29, +10, +48 = ~87 FPS) did not add up
to anything near F's single +400 FPS jump. That non-additivity is the
signature of a cliff, not a sum of instruction costs: none of B, C or D
individually got the fragment shader's resource usage under whatever
threshold trips it, so each measured only its own small piece; F crossed
the threshold in one step by removing the uniform touch entirely, and the
whole remaining cost vanished at once.

**This machine (Linux/GTX 1060, immediate-mode) shows none of it.** Every
throwaway above (B, C, D, F, G, H) measured flat here -- GPU/frame stayed in
the same 0.42-0.49 ms noise band regardless of how much of the fragment
stage touched the uniform buffer. Consistent with an occupancy cliff being
a tile-based-GPU-specific effect (limited on-chip memory for live values per
threadgroup) rather than a general "fewer instructions is faster" result --
this is exactly why this project measures on two architecturally different
GPUs rather than one.

**What this means going forward, flagged rather than decided here:** any
future per-fragment feature that touches this uniform buffer (shadows,
HDR/tonemap, block light) risks the same cliff on the gate machine -- a
~25% cost that arrives in one step when some threshold is crossed, not
gradually. With the M3 gate at 1000 FPS and this row at ~1157, the margin
for a feature that reintroduces heavier fragment-stage state is thin. H's
1745 FPS shows what's available if lighting is fixed at pipeline-build
time instead of varying per frame -- a real design tradeoff (build-time-fixed
vs. runtime-mutable lighting, or a third option: `override` constants
rebuilt only when the values actually change, e.g. a quantized day/night
step) that belongs to the project owner, not to either session measuring
it. Being written up separately with H extended into a real prototype
(rebuild-on-change, cached, with both steady-state FPS and rebuild latency
measured) rather than folded into this PR.

Mutation testing: 1 mutable Rust line in this PR's final diff
(`reverse_z_depth_constants`'s `sun_is_white`-equivalent comparison --
carried over from an earlier commit in this branch's history -- caught).
`reverse_z_depth_constants` itself has no comparisons or guards for the
script's mutator to try; hand-mutated (swapped the `(A, B)` return order) and
confirmed caught by both the new pinning unit test and the fog golden.
Golden images: byte-identical on every Linux/Vulkan golden for this row's
commit (the `.z` math is equivalent to `.w`'s on every backend that was
already correct) -- no re-bless needed, unlike ⁵⁹'s original `.w`-varying
commit which did need one.

**Update, after the PR merged: the design question reached the project
owner, and the M3 reference table this research produced.** Two more
throwaways answered the questions the occupancy-cliff finding raised:

- *Variant I -- radial fog reconstructed per fragment, zero varyings, on
  top of H.* The scene's actual fog (planar, `main` as of this row) has a
  visible artifact the project owner noticed independently while playing:
  a mountain dead ahead reads foggier than the same mountain at the screen
  edge, because planar fog follows view-axis depth, not true distance.
  Radial distance is recoverable with no varying either -- `clip_pos.xy`
  (the framebuffer pixel, converted to NDC with the viewport size, *not*
  `clip_pos.w`, so this stays clear of the DX12 `.w` quirk entirely) plus
  `view_depth` (already recovered from `clip_pos.z` elsewhere) gives
  `radial = view_depth * sqrt(1 + (ndc_x*tan_half_fov*aspect)^2 +
  (ndc_y*tan_half_fov)^2)` -- the same relationship a projection matrix
  itself encodes, evaluated per fragment instead of carried across.
  Measured against H interleaved on both machines (canceling out the
  thermal drift this machine hit mid-session -- H itself re-measured lower
  once this session's GPU reached 90°C, which is why the M3 comparison
  matters more than this machine's absolute numbers here): **I costs
  nothing over H, within noise, on both machines.** Screenshots at
  identical cameras confirm the expected difference: I visibly fogs
  screen-edge terrain that H leaves clear, center-frame indistinguishable
  between the two -- correct radial behavior.

- *Rebuild cost, corrected.* The first rebuild-prototype measurement
  included re-parsing the WGSL shader module on every rebuild, which no
  real implementation would do; splitting shader-module creation (once)
  from pipeline specialization (per lighting change) dropped this
  machine's median from ~4.3 ms to ~0.4 ms. The M3 run surfaced something
  this machine's measurement couldn't: **the *first* time any given
  override permutation is used, Metal compiles a new specialization at
  ~39 ms** (one round measured 37.84-398.49 ms across 64 first-time
  variants); every subsequent use of that same permutation drops to
  ~0.17 ms once the OS shader cache has it. For a 64-step quantized
  day/night cycle this means **64 one-time ~39 ms hitches the first time
  each sun position is ever reached**, never again after. Any
  rebuild-on-change design needs pre-warming (build all 64 variants at
  startup, off the main thread, ~2.5 s total) as part of the design, not
  an afterthought discovered as a stutter at the first in-game sunrise.

Both are now in front of the project owner as two independent, no-cost
design choices with screenshots and numbers, not decided by either
session measuring them: **which fog** (planar, what's live now and what
he already likes the look of, vs. radial, physically correct and fixes
the noticed artifact -- no FPS difference either way) and **which
lighting model** (runtime-mutable at ~1155 FPS vs. pipeline-fixed with
rebuild-on-quantized-change at ~1730 FPS, pre-warmed). Nothing further
implemented pending that choice -- the real work (`Lighting` as the
override source, a change-keyed cache, `SceneRenderer::set_lighting`,
startup pre-warming, wiring through window/bench/screenshot, and
`--fog off|on` becoming a real `override` rather than a runtime branch if
radial + fixed lighting is chosen) is follow-up, not done here.

**Full M3 reference table, this package's research** (`--bench 64` orbit,
`--fog on` throughout except where marked, scene is the radius-64/
~480k-drawn-triangle one this whole package used). What it proves, in one
line before the numbers: **on a tile-based GPU the fragment stage is a
cliff, not a slope** -- B, C and D each touch a different small piece of
what the fragment stage reads and each is worth only 2-4%, but removing
*all* of it at once (F) is worth +33-37%, and "fewer instructions" is not
the same axis at all (dropping the array index sounds like a pure win, but
carrying its replacement across as a varying, commit 3, costs -8%). Read
the rest of the table as the evidence for that one line, not as a list of
independent micro-optimizations to combine piecemeal:

| label | what | FPS | ms/frame | vs. `dad0dd6` baseline |
|---|---|---|---|---|
| -- | before package 2's fog existed (`1fcd449`) | 1550-1579 | 0.469-0.478 | +34-37% |
| -- | package 2's fog, DX12-varying-fixed (`28564f8`, ⁵⁹) | 1123-1130 | -- | baseline -3% |
| **shipped** | `.z`-based depth fog, zero varying (`dad0dd6`, this row) | 1136-1160 | 0.637-0.653 | -- |
| dropped | + ambient/diffuse as override constants (`d14bfa7`) | 1124-1130 | -- | +0% (noise) |
| dropped | + flat `vec3` normal in the vertex stage (`fd394d9`) | 1063-1068 | 0.694-0.696 | **-8%** |
| diagnostic B | `FACE_NORMALS[0]` hardcoded (wrong image, isolates the index) | 1156-1186 | 0.624-0.640 | +2% |
| diagnostic C | sun/fog-color/range as overrides (isolates remaining reads) | 1136-1167 | 0.634 | +0-1% |
| diagnostic D | fog block folded out via a real `if` (isolates fog math) | 1200-1208 | 0.615-0.619 | +4% |
| dropped | fog thresholds pre-divided, no fragment division (E) | 1136-1172 | 0.632-0.637 | +0-1% |
| diagnostic F | `fs_main` reverted to pre-fog verbatim, no `frame` touch | 1526-1584 | 0.467-0.494 | **+33-37%** |
| diagnostic G | literal lighting + fog via one interpolated `f32` | 1390-1408 | 0.523-0.534 | +21% |
| **proposal** | H -- lighting+fog entirely `override`, `fs_main` frame-free | 1708-1749 | 0.424-0.437 | **+48-51%** |
| **proposal** | I -- H + radial fog, zero varying | 1724-1749 | 0.424-0.433 | +48-51% (= H) |

Diagnostic B alone was measured on `b0d2fec` (which still had the dropped
ambient/diffuse override commit underneath it), not `dad0dd6` like every
other row -- doesn't change its conclusion, since that commit measured
zero effect of its own, but it's the actual commit the number came from,
recorded so "+2%" stays checkable rather than needing to be re-derived
against the wrong baseline later.

`main`'s post-package-2 regression this package set out to fix is now
~+2-3% over ⁵⁹ instead of the ~-27% ⁵⁹ shipped with, on the shipped `.z`
commit alone -- everything from H down is not shipped, and is what's in
front of the project owner now.

⁶¹ **The project owner's decision on ⁶⁰'s two questions, implemented.** Shown
the fog screenshots, the owner reported seeing no visible difference and
asked for whichever costs less -- since H and I measured equal on both
machines, radial (I) ships, since it also fixes the artifact he separately
noticed in play. For lighting he rejected the 64-step pre-warmed-at-startup
design outright ("I think loading that up front is not a good one, if we
have to recalculate it on every block change") and asked instead for
something that loads fast, spreads the expensive work rather than
front-loading it, and can update roughly once a second using state from
previous frames -- a background-thread rebuild that swaps in when ready,
not a fixed table of pre-baked values.

`mesh.wgsl`'s fragment stage now reads nothing from the camera uniform at
all: ambient, sun, fog colour/range, and the view-depth/radial-distance
constants are all `override` pipeline values (`render.rs`'s
`mesh_pipeline_constants`), the same mechanism variant H measured. Radial
distance comes from `@builtin(position).xy` (a framebuffer pixel, converted
to NDC with the viewport size, themselves overrides) plus the existing
`.z`-based `view_depth` -- `.w` is never touched, staying clear of wgpu 24's
DX12 bug entirely. `SceneRenderer::set_lighting` rebuilds the pipeline on a
background thread only when `Lighting` actually changes (an equality check
skips the common per-frame case of an unchanged value), reusing the parsed
shader module and pipeline layout so a rebuild only re-specializes rather
than re-parsing WGSL; `encode_scene` polls for a finished rebuild every
frame (non-blocking), so the window keeps rendering with the previous
lighting for however many frames the rebuild takes rather than stalling.
`resize` rebuilds synchronously instead (the viewport terms radial fog
needs change too, and a resize already stalls for the depth buffer).
`bench`/`--screenshot`/goldens additionally call the new
`wait_for_lighting_rebuild` (blocking) right after `set_camera`, since a
one-shot capture cannot tolerate the window's "correct in a frame or two"
tolerance.

This machine, uncontended (a runaway `--help` process from earlier in the
session had been silently eating a full CPU core for hours and inflated an
earlier round of measurements on this row by ~45% CPU/frame before it was
found and killed -- flagged here rather than left as an unexplained
best-of-three): `--bench 64` orbit, fog on, three rounds, 1916/1933/1995
FPS, CPU/frame 0.298/0.270/0.272 ms, GPU/frame 0.480/0.480/0.440 ms -- flat
against ⁶⁰'s `dad0dd6` baseline (~1940 FPS, 0.274 ms), exactly as H and I
already predicted for this immediate-mode GPU. `--fog off` still measures
identically (1956/1942 FPS) -- confirmed as a real pipeline-level change,
not a runtime branch, since `Lighting::default()`'s fog-off values now
produce a genuinely different `override` set and a genuinely different
compiled pipeline, not a `select()` on the same one.

Golden `fog_over_the_far_ring` re-blessed for radial fog, visually
inspected before blessing (screen-edge terrain now fades consistently with
true distance, matching the artifact fix -- the same check this file's
package-4 rows have used at every radial/planar transition). Every other
golden stayed byte-identical: `Lighting::default`'s actual values are
unchanged, only the mechanism carrying them to the shader changed.
Mutation testing: 2 mutable lines (the `set_lighting` equality guard and
`wait_for_lighting_rebuild`'s success match), both caught -- a mutation to
either breaks the fog golden, since every golden's render depends on the
pipeline actually reflecting the `Lighting` it was given. The one
mechanism not yet under an automated test: calling `set_camera` a *second*
time with a *different* `Lighting` on an already-built `SceneRenderer` --
every golden today exercises exactly one `set_camera` call per instance, so
the rebuild-on-a-second-change path (what a live day/night system would
actually do) runs the same code but is reasoned-about rather than directly
tested. Flagged as follow-up, not silently assumed covered.

M3 numbers for this row: not yet run this session -- the peer session that
did the interleaved M3 measurements for the rest of this package's research
went offline partway through the owner's decision being made, and this
session continued solo per the owner's instruction. Whoever next has the
M3 available should add it here rather than leave the row Linux-only.
