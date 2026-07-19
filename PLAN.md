# Cubara — Project Plan

This document is the single source of truth for the vision, architecture, and
roadmap. It grows over time; decisions are recorded here so we don't have to
re-litigate them.

See [`REQUIREMENTS.md`](REQUIREMENTS.md) for the founding wishes that drive this
plan.

---

## 1. Vision

A voxel game with the gameplay depth of the mainstream voxel game, but built on an
engine designed from the ground up for:

1. **Performance** — practically infinite render distance with LOD, stable frame
   times, no GC stutter. Benchmark: **1000+ FPS in a simple world** before we
   seriously start on gameplay.
2. **A solid, well-considered foundation** — the engine is the basis of everything.
3. **Extensibility & modding** — data-driven content, clear module boundaries,
   later a scripting/mod API.
4. **Multi-platform** — Windows + macOS now, more later (incl. possibly browser).
5. **Professional process** — git, issues, PRs, CI.

Core gameplay philosophy: **progression and synergy**. Features hang together
instead of sitting loosely side by side (the weakness of the current market leader).

### Copyright
The name is **Cubara**. We never brand the project as a sequel to or clone of an
existing game. Textures, names, and content are our own. Mechanics may be inspired;
assets and branding are original.

---

## 2. Decisions (log)

| Topic | Choice | Status |
|---|---|---|
| Project name | **Cubara** | ✅ locked |
| Render approach | Rasterization + greedy meshing → GPU-driven | ✅ locked |
| Chunk format | 16×16×16 (cubic chunks) | ✅ locked |
| Dormant-chunk simulation | Deterministic "catch-up" (Factorio-style) | ✅ concept |
| **Tech stack (language + libs)** | Recommendation: **Rust + wgpu** | ⏳ **to confirm** |

See §8 for the open language decision.

---

## 3. Render approach (why rasterization is the solid base)

You want the approach that is best long-term for thousands of blocks, processes,
and shaders. That is **rasterization with greedy meshing**, not experimental
ray-marching through a sparse voxel octree. Reasons:

- **Proven and predictable** — every major voxel game uses this; easy to debug and
  profile.
- **Scales to GPU-driven rendering** — the path to "infinite" render distance runs
  via *indirect draw* + chunk data in GPU buffers, so thousands of chunks go out
  in a handful of draw calls. We build that on top of the rasterizer.
- **Shaders fit naturally** — a material/shader system (WGSL) plugs directly into a
  rasterizer pipeline. A ray-march SVO makes custom shaders much harder.
- **Moddability** — data-driven meshes + a texture atlas make thousands of block
  types trivial to add.

Ray-marching is kept as an *optional* future technique for extreme distance (e.g.
an SVO representation purely for far LOD), but not as the foundation.

### Render techniques (roadmap)
- **Greedy meshing** — merge coplanar faces → far fewer triangles.
- **Hidden-face culling** — no faces between two solid blocks.
- **Frustum culling** — only chunks in view.
- **LOD** — distant chunks at lower resolution (merge voxels 2³/4³…), managed via a
  chunk-region octree.
- **Occlusion culling** — later (e.g. GPU hi-Z).
- **GPU-driven rendering** — indirect draw, per-chunk data on the GPU. This is the
  key to huge render distance.
- **Texture array/atlas** — for thousands of block textures.

---

## 4. Architecture

Designed as a **workspace of separate modules** (crates/libraries), so boundaries
stay sharp and modding/multiplayer plug in cleanly later.

```
cubara/
├─ core        # math, base types, ids, time, logging
├─ platform    # window, input, timing (event loop)
├─ render      # GPU renderer, pipelines, shaders, meshing, LOD
├─ voxel       # block definitions, chunk data structure (16³), palette compression
├─ world       # chunk storage, streaming, worldgen, save/load
├─ sim         # simulation tick, dormant-chunk catch-up (Factorio timers)
├─ game        # actual gameplay (player, items, crafting) — later
├─ modding     # scripting/mod API — later
├─ net         # multiplayer — later
└─ app         # ties everything together; the executable
```

### Data-driven design
Blocks, items, and recipes are defined in **data files** (e.g. RON/TOML/JSON), not
hardcoded. This makes "thousands of blocks" and modding feasible without touching
the engine.

### Entities via ECS
For entities (mobs, items, players) we use an **ECS library** (e.g. `hecs` /
`bevy_ecs` as a standalone library, not a whole engine). ECS keeps features
decoupled yet cooperating — exactly the "synergy without loose features" goal.

### Threading
- Worldgen + meshing on **worker threads** (thread pool).
- The main thread does almost nothing but render → stable frame times.
- Communication via message queues/channels; no locks on the hot path.

---

## 5. Chunk system (16×16×16 cubic chunks)

Your proposal of 16³ is the right call: **cubic chunks** instead of the traditional
16×16×256 columns. Benefits: true 3D infinity (up/down too), cleaner LOD, and
fine-grained loading/unloading.

- **Palette compression per chunk** — store a palette of used block types + compact
  indices per chunk. Memory-efficient, scales to thousands of types.
- **Chunk states**: `Ungenerated → Generated → Meshed → Active ⇄ Dormant → Unloaded`.
- **Persistent loaded regions** — specific areas can stay loaded (think Factorio-
  style "active" regions around machines/players).

### Dormant-chunk simulation (the "Factorio timers")
Don't simulate every chunk every tick. Instead:

- Every chunk (or process within it) remembers the **last simulated tick**.
- When a dormant chunk is activated, we compute the **elapsed time** and do a
  **deterministic catch-up**: crops keep growing, furnaces finish smelting, etc.,
  as if you had been there.
- Requires block updates to be **deterministic** and **time-parameterizable**
  (given start state + Δt → end state), without iterating tick-by-tick where
  avoidable (closed-form where possible, otherwise bounded batch simulation).

This is a foundational design principle, not an add-on: we design the simulation
system around it from the start.

---

## 6. Performance discipline

- **Profiling from day one** (e.g. `tracy` / `puffin`) — we never guess, we measure.
- **Benchmark scene** — a standardized simple world in which we measure FPS; the M1
  gate is 1000+ FPS in it.
- **Frame-budget thinking** — every ms counts; avoid allocations on the hot path.
- **No GC** — a key reason the recommended stack is not a managed language.

---

## 7. Roadmap & milestones

Each milestone = a GitHub milestone with issues. The **M1 gate** is your hard
condition: only after 1000+ FPS do we build gameplay.

| # | Milestone | Result |
|---|---|---|
| M0 | **Setup** | Repo, project skeleton, window + clear screen, CI green |
| M1 | **First chunk** | Render one chunk of cubes; **1000+ FPS** in benchmark ← *gate* |
| M2 | **Meshing & culling** | Greedy meshing, hidden-face + frustum culling, multiple chunks, simple worldgen |
| M3 | **Streaming** | Load/unload chunks around the player; "infinite" flat world |
| M3.5 | **GPU-driven rendering** | Shared buffers + `multi_draw_indirect`, then GPU compute culling — see §10 |
| M4 | **LOD** | Distant chunks at lower resolution; large render distance |
| M5 | **Player & interaction** | Camera/controller, raycasting, place/break blocks |
| M6 | **Data-driven content** | Block registry from data files, texture atlas, base block set |
| M7 | **Simulation** | Tick system + dormant-chunk catch-up; trees/crops grow |
| M8 | **Persistence** | Save/load the world |
| **Alpha** | **Playable survival** | Chop trees, mine iron — a real small world |

**After alpha** (planned separately, with synergy first): inventory/crafting,
items, mobs, shaders, multiplayer (`net`), expanded mod API (`modding`), more
biomes and content toward a full modern voxel game.

---

## 8. Open decision: tech stack (language)

This is *the* foundational, hard-to-reverse choice. See the separate discussion in
chat. In short:

- **Recommendation: Rust + wgpu + winit.** No GC (stable frame times), memory-safe
  (great for a large codebase + netcode), strong concurrency for parallel worldgen/
  meshing, and `wgpu` targets Metal (Mac), DX12/Vulkan (Windows), and WebGPU
  (browser) from one codebase. Downside: learning curve + compile times.
- **Alternative: C++ + Vulkan.** Maximum control, partly familiar to you, but manual
  memory management and messier cross-platform.

Once this is locked, we fill in: exact crates/libraries, build system, CI config,
and the `app` skeleton (M0).

---

## 9. Development process (professional)

- **Git from day one**, `main` as the main branch, feature branches (`feature/…`).
- **Conventional Commits** (e.g. `feat:`, `fix:`, `perf:`, `docs:`).
- **GitHub**: features via **issues**, changes via **PRs**, milestones per phase.
- **CI** (GitHub Actions): build + tests + lint/format on every push/PR.
- **Multi-platform CI**: build on Windows and macOS.
- **Semantic versioning** + changelog.
- Later: `CONTRIBUTING.md`, issue/PR templates, branch protection on `main`.

---

## 10. GPU-driven rendering (milestone M3.5)

Recorded plan for the render-performance arc between M3 (streaming) and M4 (LOD).
Tracked in GitHub milestone **M3.5** — issues #26 (spike), #27 (step 1), #28
(step 2), #29 (tracking).

**Status:** #26 spike ✅ done, #27 step 1 ✅ done (the portable win). On the heavy
bench scene (1,349 chunks) the arena + one `multi_draw_indirect` cut M3 CPU/frame
from **0.317 ms to 0.199 ms** (~37%) and 1,349 draws to **one** — see
[`BENCHMARKS.md`](BENCHMARKS.md). **Step 2 (GPU compute cull, #28/#32/#33) is
parked** — a prototype showed it doesn't pay off portably today; see *Finding: GPU
culling is Vulkan-only under wgpu right now* below.

### The problem

After M3, the renderer draws **one chunk per draw call**. The benchmark scene is
~1,350 chunks, so ~1,350 draw calls per frame, and we are **CPU-submit-bound** at
~0.5 ms/frame (see [`BENCHMARKS.md`](BENCHMARKS.md)). The draw-call count is the
bottleneck, and cutting it is the road to practically infinite render distance
(§3).

### Batching and GPU-driven are one arc, not two options

Reducing draw calls ("batching", *A*) and moving the whole cull+draw to the GPU
("GPU-driven", *B*) are **not alternatives**. *B* is the destination, and the core
of *A* — putting all chunk geometry in **shared GPU buffers** with **per-chunk
metadata** — is the foundation *B* is built on. You cannot draw many chunks from
one indirect submit unless their geometry lives in shared buffers, so that work is
done once and reused. Sequenced so nothing is thrown away:

1. **Step 1 — shared buffer arena + `multi_draw_indirect` (CPU culling) [#27]. ✅**
   All resident chunk geometry moves into a pooled vertex/index buffer with
   per-chunk sub-allocations (a first-fit, coalescing free-list allocator, since
   streaming frees and reuses slots constantly). A per-chunk metadata array holds
   buffer offsets, index count, and AABB. Each frame the CPU frustum-culls, writes
   an indirect-args buffer, and issues **one** `multi_draw_indexed_indirect` instead
   of ~1,350 draws. Backends without `MULTI_DRAW_INDIRECT` fall back to a
   `draw_indexed` loop over the *same* shared buffers, so there's no second geometry
   path. (Implemented in `crates/render/src/arena.rs`.)

2. **Step 2 — GPU compute frustum culling [#28]. ⏸ parked.** The intent: per-chunk
   metadata to a storage buffer; a compute shader reads the frustum + AABBs and
   writes the visible draws' indirect args, so CPU per-frame cost goes **flat
   regardless of chunk count** — the endgame from §3. Still the right destination,
   but **parked** for now — see the finding below.

### Finding: GPU culling is Vulkan-only under wgpu right now

The #26 spike checked feature support on both machines (`cargo run --release --
--caps`):

| feature | Windows / Vulkan (RTX 4060) | macOS / Metal (Apple M3) |
|---|---|---|
| `MULTI_DRAW_INDIRECT` | ✅ | ✅ |
| `MULTI_DRAW_INDIRECT_COUNT` | ✅ | ❌ |
| `INDIRECT_FIRST_INSTANCE` | ✅ | ✅ |

A GPU-compute-cull prototype (2026-07-19) then exposed the real blocker, which
isn't the missing `_COUNT` feature — it's that **wgpu has no native multi-draw on
Metal and emulates `multi_draw_indirect` as a CPU loop of `count` draws.** So on
Metal the per-frame CPU cost is proportional to the number of draws *submitted*,
and a GPU cull can't lower that: the CPU still records every draw. With the only
count the CPU knows being the *conservative* one (all resident chunks, culled ones
drawn as zero-index no-ops), GPU culling is actually a **small regression** vs the
Step 1 CPU cull, which submits only the visible set. Measured on M3, radius-20 scene:

| path | draws submitted | CPU/frame |
|---|---|---|
| Step 1 CPU cull (#27) | 3,540 (visible) | **0.439 ms** |
| GPU cull, conservative count | 3,634 (all resident) | 0.520 ms |

On **Vulkan** it's the opposite: `multi_draw_indirect` (even without `_COUNT`) is a
single native command, so CPU/frame is already flat there — GPU culling would help.
But that makes the flat-CPU win **Vulkan-only today**, and chasing it now would
either regress Metal or fork the draw path per-backend. We deliberately keep **one
uniform renderer** (Step 1) instead.

**Decision:** park Step 2. Step 1's shared arena + per-chunk metadata is exactly the
foundation a GPU cull would reuse, so nothing here is wasted.

**Do not depend on wgpu fixing this.** Native Metal multi-draw is *not* a scheduled
wgpu release — it's a long-open issue ([gfx-rs/wgpu#2148], open since Nov 2021) with
no timeline, and it's genuinely hard: Metal has no draw-count-buffer concept, so a
native path needs Indirect Command Buffers (and a `gl_DrawID`-style WGSL builtin,
[wgpu#6823], to index per-chunk data) — a different encoding model, not a flag flip.
So treat Step 2 as unblocked by one of *our* choices, not by waiting:

- we make Vulkan the primary perf/benchmark backend and let Metal keep the CPU-cull
  path as an accepted second-class path, or
- we deliberately accept one backend branch and build the Vulkan-gated cull ourselves.

**Re-check periodically** (say each time we touch render perf, or bump wgpu) that the
uniform CPU-cull arena is still the most sensible method — if wgpu gains ICB-based
GPU-driven encoding, or our own priorities shift toward Vulkan-first, revisit this
decision then.

[gfx-rs/wgpu#2148]: https://github.com/gfx-rs/wgpu/issues/2148
[wgpu#6823]: https://github.com/gfx-rs/wgpu/issues/6823

### Out of scope (separate track)

**Async chunk meshing on worker threads** (§4) fixes streaming *hitches*, not the
draw-call count. Orthogonal; tracked separately.
