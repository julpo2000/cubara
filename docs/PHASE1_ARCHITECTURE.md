# Phase 1 — Architecture

The design that [`ROADMAP.md`](../ROADMAP.md) phase 1 implements: a textured,
walkable, cave-riddled world at render distance 64 running at 1000+ FPS.

[`ARCHITECTURE.md`](../ARCHITECTURE.md) states the rules the code must hold to and
does not change. This document is the *design* that satisfies them for one phase:
which types exist, in which crate, and which decisions are already made so nobody
re-opens them mid-PR.

---

## §0 What "accounts for expansion" means here

`ARCHITECTURE.md` is blunt about the failure mode: pre-built slots for features we
might want produce dead fields and false abstractions, and the guess gets thrown
away anyway. So this document does not add empty enum variants, unused config, or
`Option<T>` fields waiting for phase 3.

Expansion-readiness here is exactly two things:

1. **Seams, not slots.** Adding water later must be a new render pass and a new
   registry field — a localised change with a compile error at every site that
   needs updating — not a rewrite of the mesher, the arena, and the chunk store.
   The test is: *can this subsystem be deleted and rebuilt without the rest
   falling over?*
2. **The handful of decisions that are genuinely expensive to reverse get made
   now, and only those.** There are four of them, and each is here because
   deferring it means a migration, a corrupted save format, or a rewrite:

   | Decision | If deferred |
   |---|---|
   | Stable **string** block identity (§3) | Every save and every mod breaks the day a block is inserted in the middle of a file. |
   | Block state lives in the **flat id space** (§3.4) | The voxel array grows a second field; palette compression, meshing and saves all change shape. |
   | Vertices are **node-local**, not world-space (§5) | Precision failures far from origin, and a vertex format change means re-meshing the world. |
   | **Fixed tick + seeded RNG in world state** (§8) | Determinism cannot be retrofitted — Rule 1. It is a rewrite, and it takes multiplayer and replays with it. |

Everything else is deliberately built for today's requirement only.

---

## §1 The crate graph after phase 1

```
        ┌──→ cubara-render ──→ cubara-voxel
cubara-app ─┤
        └──→ cubara-sim ──→ cubara-world ──→ cubara-voxel
```

| Crate | Owns | Must never know about |
|---|---|---|
| `cubara-voxel` | `BlockId`, `Chunk`, palette, the block registry, mesh building, the `Vertex` format | the GPU, the world, the player |
| `cubara-world` | chunk & node storage, worldgen, streaming policy, raycast, node meshing | the GPU, the window, the player |
| `cubara-sim` | **new.** the tick loop, world RNG, player state and physics, `InputFrame` | the GPU, the window |
| `cubara-render` | adapter, pipelines, arena, texture array, camera, the one `render_scene` | gameplay, input, the world |
| `cubara-app` | the event loop, the three thin entry points (window / `--bench` / `--screenshot`) | — |

**One change to today's graph: `cubara-render` stops depending on `cubara-world`.**
It depends on it now, which is a Rule 3 smell — the renderer's inputs should be
meshes, origins and a camera, nothing that knows what a chunk is. Node meshing
moves to `cubara-world` (it is pure CPU work on chunk data and belongs next to
it), and `cubara-app` hands the results to the renderer. This is what makes the
renderer genuinely rebuildable, which is the bar `ARCHITECTURE.md` sets.

**Enforced by:** a new check in `scripts/check-architecture.sh` rejecting
`cubara-world` in `crates/render/Cargo.toml`. A rule with no mechanism is a wish.

---

## §2 The budget — what radius 64 actually costs

Every design decision below is driven by these numbers, so they come first.

### The binding constraint is draw count, not triangles

Measured on the M3 at radius 12 (`BENCHMARKS.md`): 1,349 draws cost 0.199 ms of
CPU per frame. That is **0.148 µs per submitted draw**, and it is not going away:
wgpu has no native multi-draw on Metal and emulates `multi_draw_indirect` as a CPU
loop over `count` draws ([`PLAN.md`](../PLAN.md) §10). The CPU records every draw,
so cost scales with *draws submitted*, and no culling strategy changes that.

1000 FPS is a 1.000 ms frame. Allotting **0.35 ms** to draw submission (the rest
goes to streaming, meshing hand-off, sim, upload and present):

```
0.35 ms / 0.148 µs per draw  ≈  2,360 draws
```

> **Design budget: at radius 64, the number of drawn nodes must stay under ~2,000.**

For scale: radius 64 is a 129×129 chunk footprint. At full resolution with even a
thin 3-chunk vertical band that is **49,923 chunks** — 7.4 ms/frame of submission
alone, or 135 FPS, with a perfect cull and zero triangles. **LOD must cut draw
count by roughly 25×, and it can only do that by merging chunks into shared
meshes.** That is §6, and it is the block phase 1 turns on.

### The other three budgets

| Budget | Value | Where it comes from |
|---|---|---|
| Triangles per frame | ≲ 1M | ~1 ms of GPU for a simple opaque pass on an M3. With ≤2,000 nodes that is ~500 tris/node; radius 12 measures 161 tris/chunk today, and caves will push it up — which is why caves are in phase 1, so the budget is tested honestly. |
| Vertex memory | ≤ 32 MB | 4M-vertex arena. At today's 28-byte vertex that is 112 MB, and adding a texture layer naively makes it 160 MB. §5 packs it to 8 bytes. |
| Worldgen samples for a full load | ~8.2M | 2,000 nodes × 16³ samples each. Generating far nodes at full resolution and downsampling would be ~545M samples — a factor of 65. This is why generation is LOD-native (§7). |

The triangle ceiling is an estimate, not a measurement. **Block 1.0 replaces it
with a measured number before anything is built on it.**

---

## §3 Block identity

### 3.1 `BlockId`

```rust
#[repr(transparent)]
pub struct BlockId(pub u16);   // 65,536 types; 0 is always air
```

A **runtime** index into the registry. It is not stable across runs, and it never
reaches disk or the network in raw form.

### 3.2 The registry, from RON

`cubara-voxel` owns `BlockRegistry`, loaded from `assets/blocks/*.ron`:

```ron
Block(
    name: "cubara:stone",
    solid: true,
    faces: All("stone"),                         // or Sided(top: .., side: .., bottom: ..)
)
```

Phase 1 defines exactly three: `cubara:stone`, `cubara:soil`, `cubara:grass`
(sided — grass top, soil bottom, a blended side). Names, textures and art are
original to the project.

The registry resolves texture *names*. It does not know what an array layer is —
`cubara-render` maps names to layers when it builds the texture array. That is the
seam that keeps the block definitions GPU-free (Rule 4).

### 3.3 Stable identity is the string, not the number

**Runtime ids are assigned by sorting block names lexicographically.** Same set of
definitions in, same ids out, on every machine and every run — which is what
Rule 1 needs.

Anything persisted or transmitted stores **names**: a chunk's palette on disk is a
list of strings (or indices into a per-world name table that is itself stored as
strings). Loading a world maps names → current runtime ids.

This costs one sort and one lookup table. It is here rather than in phase 2
because the alternative is discovering, after there are saved worlds, that
inserting a block into a RON file silently reinterprets every stone block in every
save as dirt — and that a mod adding blocks makes existing worlds unloadable. It
is the cheapest expansion insurance in the project.

### 3.4 Block state and block entities — decided, not built

Phase 1 has no block state and no block entities. The decisions are recorded here
so they are not made badly under pressure in phase 2:

- **Block state** (a log's axis, wheat's growth stage) becomes **distinct ids in
  the same flat id space** — the registry expands `cubara:wheat` with a growth
  property into eight ids. The voxel array stays a single `u16` forever, palette
  compression keeps working unchanged, and the mesher never learns what a property
  is. No field is added now.
- **Block entity data** (a furnace's contents) lives in a per-chunk side table,
  `BTreeMap<LocalPos, BlockEntity>` — ordered, because Rule 1 forbids letting
  iteration order affect results. It is *not* in the voxel array. Nothing is added
  now; the point is that when phase 2 adds a furnace, the chunk format does not
  change.

---

## §4 Chunk storage

```rust
pub enum ChunkStorage {
    Uniform(BlockId),                 // no allocation at all
    Palette { palette: Vec<BlockId>, bits: u8, data: Box<[u64]> },
}
```

`bits` is `ceil(log2(palette.len()))` clamped to 1/2/4/8/16; a chunk promotes on
the first write that overflows its palette and re-packs.

The `Uniform` case is not a micro-optimisation — at radius 64 the large majority
of chunks are entirely air or entirely stone, and it is what keeps the resident
full-resolution core inside a sane memory footprint. The full-res core (roughly
2,300 chunks) costs single-digit MB palette-compressed.

**LOD nodes hold no voxel data at all** — only a mesh and its metadata (§6). The
far field is geometry, not a world you can edit; editing requires the full-res
core, which is exactly where the player is.

---

## §5 Meshing and the vertex format

### 5.1 One mesher

There is one mesher, and **a full-resolution chunk is just a level-0 node**. There
is no chunk path and a separate LOD path. This is Rule 5 applied before the fact:
the renderer already grew three divergent copies once, and a second meshing path
is the obvious place for it to happen again.

### 5.2 The vertex is packed and node-local

Today: `position: [f32;3], normal: [f32;3], ao: f32` — 28 bytes, in **world
space**. Two problems, one immediate and one structural:

- Adding a texture layer and greedy-quad UV extents pushes it to ~40 bytes, and
  4M vertices × 40 B = 160 MB, over budget (§2).
- World-space `f32` positions lose precision as the player travels away from the
  origin. Radius 64 is fine; a world you can walk across is not. And a vertex
  format change means re-meshing everything, so it is done once, now.

Phase 1 format — two `u32`, **8 bytes**, positions local to the node:

```
word 0:  x:10  y:10  z:10  ao:2          // node-local lattice coords
word 1:  tex_layer:12  face:3  u_len:8  v_len:8  (1 spare)
```

`face` is 3 bits because greedy-meshed voxel faces are always one of six axis
directions — the normal is a lookup, not data. `u_len`/`v_len` carry the greedy
quad's extent so the texture tiles across a merged face.

### 5.3 How the shader knows which node it is drawing

Node origins live in a storage buffer indexed per draw. wgpu has no `gl_DrawID`
equivalent ([wgpu#6823], noted in `PLAN.md` §10), so the index comes from
**`INDIRECT_FIRST_INSTANCE`** — confirmed supported on both Metal and Vulkan by the
`--caps` spike. Each node's indirect args set `first_instance = node_index`,
`instance_count = 1`, and the vertex shader reads `@builtin(instance_index)`.

Zero per-vertex cost, one code path on both backends. **If it does not behave
under Metal's emulated multi-draw, the fallback is a `node_index: u16` in the
vertex** (10 bytes instead of 8, still inside budget). Block 1.0 verifies which
one applies before §6 is built on it.

---

## §6 LOD — the region node tree

**This is the block that decides whether radius 64 is reachable.**

### 6.1 The change in kind

`build_mesh_lod` today downsamples voxels *within* one chunk: fewer triangles,
**same number of draws**. Per §2, draws are the binding constraint, so that
approach cannot reach radius 64 no matter how far it is pushed.

> **An LOD node is one mesh, one arena allocation, and one draw, covering
> 2^L × 2^L × 2^L chunks.**

Level 0 is a single chunk. Level 3 covers 8×8×8 = 512 chunks in one draw. That is
where the 25× draw-count reduction comes from.

### 6.2 Uniform node cost

Every node, at every level, is sampled on a **fixed 16³ lattice**. A level-0 node
samples every block; a level-3 node samples every 8th block across a 128³ volume.

The consequence is that generating and meshing any node costs the same, so total
cost is O(node count) and node count is bounded by the ring schedule — rather than
far nodes being quietly the most expensive things in the frame.

### 6.3 The ring schedule

A table of `(level, outer radius in chunks)`, e.g. level 0 out to 8, level 1 to
16, level 2 to 32, level 3 to 64. It is a tuned constant, not a formula, with one
constraint it must satisfy:

> total drawn nodes at radius 64 < 2,000 (§2)

Block 1.8 tunes it against a measurement. If a schedule that meets the budget
makes the near field visibly coarse, that is a finding to report — moving the
budget is not the agent's call.

### 6.4 Seams between levels: skirts, not stitching

Where a level-2 node meets a level-3 node the surfaces do not line up and a crack
of background shows through. Two standard fixes; the decision is **skirts**: each
node extends its border quads downward by one LOD cell, hiding the gap.

Rejected: stitching (generating transition geometry that matches the neighbour's
resolution). It requires knowing the neighbour's level at mesh time, which
serialises a meshing pipeline that is deliberately parallel and re-runs when a
neighbour's level changes. Skirts are purely local, cost a handful of quads, and
are invisible on opaque terrain. If transparency later makes skirts visible, that
is the point at which stitching earns its cost.

### 6.5 Lifecycle

Nodes reuse what already exists: the same worker pool that meshes chunks today,
the same arena and slab allocator, the same frustum cull, the same
`multi_draw_indirect` submit. A node is generated → meshed → uploaded → drawn →
evicted, and results are merged into the world in a **fixed order** regardless of
which worker finishes first (Rule 1, and the fix for [#83](../../issues/83)).

---

## §7 Worldgen

```rust
pub struct WorldGen { seed: u64 }
impl WorldGen { fn density(&self, x: i32, y: i32, z: i32) -> f32; }
```

A pure function of position and seed, with no state, no wall clock and no ambient
RNG. Terrain is layered noise; caves are a second 3D noise field subtracted from
the terrain density.

**LOD-native by construction:** a node evaluates `density` on its own lattice at
its own step size. Nothing generates at full resolution and downsamples, which is
the factor-of-65 in §2.

**The cross-platform float risk, stated plainly.** Rule 1 says no floating point
where integers will do, and noise is floating point. The mitigation is that the
*output* is thresholded to a `BlockId`, and the determinism test asserts that the
same seed produces a **bit-identical block array on Windows and macOS** — both of
which CI already runs. If that test ever fails, the fix is fixed-point noise, and
we will know in phase 1 rather than discovering it in multiplayer.

---

## §8 The tick loop and the sim/render seam

`cubara-sim` is new, small, and GPU-free.

```rust
pub struct Sim { pub tick: u64, rng: WorldRng, pub player: Player }
pub struct InputFrame { /* movement axes, look delta, button edges */ }

impl Sim { pub fn tick(&mut self, world: &mut World, input: &InputFrame); }
```

- **Fixed timestep**, 60 Hz, accumulator-driven. The sim advances by tick number
  and never reads elapsed seconds (Rule 1). `Instant::now()` stays in `app` and
  the profiler.
- **The renderer interpolates** between the previous and current sim state using
  the leftover accumulator fraction, so 1000 FPS rendering of a 60 Hz sim is
  smooth. Interpolation is a render-side concern and never writes back.
- **`WorldRng` is explicit state** — a small fixed-algorithm PRNG stored with the
  world and saved with it, not `thread_rng()`.
- **Input is a value type.** `InputFrame` exists in phase 1 for one immediate
  reason: it is what makes the replay test (block 1.9) possible. It is also, not
  coincidentally, the shape netcode needs later.

`cubara-app` owns the loop: collect input → `sim.tick(&mut world, &input)` →
build the scene → `render_scene`. The renderer receives data and returns pixels;
if it can move the player, the boundary is wrong (Rule 3).

---

## §9 Player physics

An AABB swept against solid voxels, resolved **axis by axis in a fixed order**
(Y, then X, then Z) so the result never depends on iteration or scheduling.
Gravity, ground detection, step-up over one block, and a jump impulse. It runs
inside `Sim::tick`, at fixed dt — which is what makes it deterministic and
testable without a GPU.

Free-fly survives as a debug mode, but as a *mode inside the sim*, not a second
movement implementation on the side (Rule 5).

Position is `f32` in phase 1. The determinism harness hashes the player's position
bits, so a cross-platform divergence surfaces as a CI failure; fixed-point is the
recorded fallback if it does.

Pinned by unit tests: a player AABB never ends a tick intersecting a solid voxel,
for a spread of velocities including ones large enough to tunnel a naive
implementation.

---

## §10 The render path

Unchanged in shape — one `render_scene`, one arena, one indirect submit — with
three additions:

1. A **texture array** (16×16 tiles, one layer per texture, nearest filtering,
   mipmapped) plus its bind group.
2. A **per-node origin storage buffer**, indexed via `instance_index` (§5.3).
3. The shader reads the texture layer and quad extents from the packed vertex and
   samples the array; the flat green constant in `mesh.wgsl` is deleted.

**Transparency is not in phase 1.** When water, glass and leaves arrive they are a
*second* arena and a *second* pass, sorted back-to-front, sharing the same mesher
and the same node tree. That is additive because the arena is parameterised by
nothing gameplay-related — which is the seam this section exists to protect.

---

## §11 Decisions, in one table

| # | Decision | Rejected alternative | Because |
|---|---|---|---|
| 1 | `BlockId(u16)` + per-chunk palette, `Uniform` fast path | one byte per voxel | memory at radius 64; and 256 types is not "thousands" |
| 2 | Registry from RON; **names** are the stable identity | raw numeric ids on disk | inserting a block would corrupt every save and every mod |
| 3 | Block state as distinct ids in a flat space | a second field per voxel | keeps the voxel array, palette and mesher unchanged forever |
| 4 | 8-byte packed, **node-local** vertex | 28-byte world-space f32 | vertex-memory budget; precision far from origin; re-meshing cost later |
| 5 | `INDIRECT_FIRST_INSTANCE` → `instance_index` for per-node data | per-vertex node index | zero per-vertex cost, one path on both backends (fallback recorded) |
| 6 | LOD node = 2^L chunks, one mesh, one draw, 16³ samples | per-chunk voxel downsampling | draws are the binding constraint (§2); triangle reduction alone cannot reach radius 64 |
| 7 | Skirts at LOD seams | stitched transition geometry | local and parallel-safe; stitching serialises the mesher |
| 8 | Density function sampled at node resolution | generate full-res, downsample | 8.2M samples vs 545M for a full radius-64 load |
| 9 | Fixed 60 Hz tick, render-side interpolation | per-frame simulation | Rule 1; and it is a rewrite if deferred |
| 10 | `cubara-render` loses its `cubara-world` dependency | leave it | Rule 3; it is what makes the renderer rebuildable |

## §12 What phase 1 does not build

Inventory, items, crafting, mobs, health, day/night, save/load, trees, ores,
water, transparency, lighting propagation, multiplayer, mods.

The claim this document makes is not that those will be easy. It is that each one
lands at a **named seam** — a new registry field, a second render pass, a new
system in `cubara-sim`, a serialiser over a chunk format that already stores
stable names — rather than as a change to the voxel array, the mesher, the arena
and the streaming policy at the same time. If a phase-2 feature cannot find its
seam, that is a defect in this document and it gets fixed here.

[wgpu#6823]: https://github.com/gfx-rs/wgpu/issues/6823
