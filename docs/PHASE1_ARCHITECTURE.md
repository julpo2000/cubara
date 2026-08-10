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
   | **Names are identity, numbers are per-world** (§3) | Every save and every mod breaks the day a block is inserted in the middle of a file. |
   | Block **shape and state live in the flat id space** (§3.5) | The voxel array grows a second field; palette compression, meshing and saves all change shape. |
   | The **save format**, designed with the block representation (§7) | The chunk layout gets designed twice, and the second one is a migration. This is why persistence is in phase 1 and not phase 2. |
   | Vertices are **node-local**, not world-space (§5) | Precision failures far from origin, and a vertex format change means re-meshing the world. |
   | **Fixed tick + seeded RNG in world state** (§9) | Determinism cannot be retrofitted — Rule 1. It is a rewrite, and it takes multiplayer and replays with it. |

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
| Worldgen samples for a full load | ~8.2M | 2,000 nodes × 16³ samples each. Generating far nodes at full resolution and downsampling would be ~545M samples — a factor of 65. This is why generation is LOD-native (§8). |

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

### 3.2 The registry, authored as material × shape

`cubara-voxel` owns `BlockRegistry`, loaded from `assets/blocks/*.ron`. A
definition describes a **material**, and the shapes that material comes in:

```ron
Material(
    name:     "cubara:stone",
    solid:    true,
    faces:    All("stone"),          // or Sided(top: .., side: .., bottom: ..)
    shapes:   [Full],                // phase 1 has only Full; later: Stair, Slab, …
)
```

At startup the registry **expands** each material × shape pair into its own
`BlockId`. `cubara:oak` with `[Full, Stair, Slab]` becomes three ids. You author
a material once and get its whole family; the rest of the engine only ever sees a
flat list of ids and never learns what a "shape" is.

Phase 1 defines exactly three materials, all `Full`: `cubara:stone`,
`cubara:soil`, `cubara:grass` (sided — grass top, soil bottom, a blended side).
The `shapes` field carries one value and the expansion is a one-element loop; it
is written this way now because it costs nothing and because the alternative —
one hand-written definition per material-and-shape combination — is the
combinatorial explosion this decomposition exists to avoid.

The registry resolves texture *names*. It does not know what an array layer is —
`cubara-render` maps names to layers when it builds the texture array. That is the
seam that keeps the block definitions GPU-free (Rule 4).

### 3.3 Where numbers live, and where names live

There is no string anywhere near a block in memory or in the bulk of a save file.
Concretely:

| Where | What is stored | Size |
|---|---|---|
| The voxel array | index into the chunk's palette | **4 bits** typically |
| The chunk's palette (memory and disk) | `BlockId` — a `u16` | 2 bytes × ~3–20 entries |
| The world header, once per world | `id → name` table | a few KB per world, total |
| Never | a name | — |

So a chunk on disk is a handful of `u16`s plus bit-packed indices, which is
**four times smaller** than storing a 2-byte id per block outright, and the names
exist exactly once per world file rather than once per chunk.

### 3.4 Names are identity; numbers are per-world

Runtime `BlockId`s are assigned by sorting material×shape names
lexicographically: the same definitions in, the same ids out, on every machine and
every run — which is what Rule 1 needs. The world header records the mapping that
was in force when the world was created, and loading remaps saved ids → current
runtime ids (§7.2).

The reason this matters is not tidiness, it is that **a number has to be assigned
by someone, and that someone has to stay consistent forever.** If the number is
the identity then inserting a block into a file renumbers everything after it and
silently reinterprets every stone block in every existing save as soil. And a
fixed partition of the id space — a byte for the category, a byte for the member —
adds a second failure: two mods that both claim category `0x2A` produce worlds
that used them both and can never be recovered, and a category that fills up
cannot borrow from an empty one. 65,536 ids is plenty; a *fixed division* of them
is the part that binds.

With names as identity, a mod's blocks take whatever numbers are free in the world
they are installed into, and a conflict is structurally impossible. The cost is
one sort and one table.

### 3.5 Shape and state in the flat id space — and why that is free

**Block state** (a log's axis, wheat's growth stage) and **shape** (stair, slab)
are the same problem, and both are handled the way §3.2 handles shape: the
registry expands them into **distinct ids in one flat space**. The voxel array
stays a single palette index forever, palette compression keeps working unchanged,
the mesher never learns what a property is, and the save format does not move.

The objection to flattening is that ids explode — thousands of materials times
several shapes times several states. Palette compression is what makes that a
non-issue: a chunk containing oak stairs spends **one palette entry** on them, and
its voxel array is still 4-bit indices. The explosion is confined to the registry,
which is a few thousand entries in RAM and is not in any hot path.

This is why shape is *not* a third byte per voxel. A byte per block would cost
4 KB per chunk — for information that, in the overwhelming majority of chunks, has
one or two distinct values.

Nothing here is built in phase 1 beyond the one-element expansion in §3.2. What is
built is the id space that makes it a registry change later rather than a chunk
format change.

### 3.6 Block entities

A furnace's contents are not block state — they are unbounded and per-instance.
They live in a per-chunk side table, `BTreeMap<LocalPos, BlockEntity>`, ordered
because Rule 1 forbids letting iteration order affect results, and serialised as
its own section of the chunk payload (§7.3).

Phase 1 has none and writes no such section. The format carries a version number
(§7.1), so adding the section in phase 2 is a version bump — which is the seam,
and the reason no empty field is reserved for it now.

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

## §7 The save format

Persistence is in phase 1 because the on-disk format and the in-memory block
representation are one decision (§0). Designing them apart means designing the
chunk layout twice, and calling the second one a migration.

### 7.1 Layout

```
saves/<world>/
  level.ron                      # header: seed, tick, RNG, player, id table
  region/r.<rx>.<ry>.<rz>.cbr    # 32×32×32 chunks = 512³ blocks per region
```

Regions are cubic because chunks are (`REQUIREMENTS.md` #4) — a column-shaped
region would reintroduce the vertical special case that cubic chunks exist to
remove.

A region file is a sorted directory followed by payloads:

```
"CBRG" | u16 format_version | u32 entry_count
[ u16 local_index | u32 offset | u32 length ] × entry_count   — sorted by index
payloads, written in that same order
```

Sorted, and written in directory order, so **the same world state produces a
byte-identical file** — which is what makes the round-trip test a hash comparison
rather than a semantic diff. All integers are explicitly little-endian, so a world
saved on Windows loads on macOS; CI runs both, so a fixture world committed to the
repo tests exactly that.

No compression in phase 1. Chunks are already palette-compressed and only edited
ones are written, so the win would be small and it needs a new dependency — a
decision to take deliberately, not in passing.

### 7.2 The header and the id table

`level.ron` is RON — the registry already parses it, worlds are tiny, and a
header you can read in a text editor is worth a lot while the format is young.

```ron
World(
    format_version:   1,
    worldgen_version: 1,
    seed:   6017244015443278,
    tick:   148203,
    rng:    (state: .., inc: ..),
    player: (pos: (..), vel: (..), yaw: .., pitch: ..),
    blocks: [ (1, "cubara:grass"), (2, "cubara:soil"), (3, "cubara:stone") ],
)
```

`blocks` is the id table from §3.4: the numbers that were in force when this world
was created. Loading builds a `saved_id → runtime_id` remap, so ids may be
reassigned freely between runs and a mod's blocks take whatever numbers are free.

Two guards, both hard errors in phase 1 rather than silent damage:

- **A name in the table that the registry no longer knows** (a removed mod) fails
  the load and names what is missing. Preserving unknown blocks so the world
  survives a temporarily-uninstalled mod is the right long-term answer and is
  phase-2 work; the id table is the seam it attaches to.
- **A `worldgen_version` mismatch** fails the load, because of §7.4.

### 7.3 Chunk payload

The in-memory representation, written out — §4's two cases and nothing else:

```
u8 storage:  0 = uniform            → u16 block_id
             1 = palette            → u8 len, [u16; len], u8 bits, [u64; n] packed
```

Phase 2's block entities (§3.6) become a further section, guarded by
`format_version`. Nothing is reserved for them now.

### 7.4 Only edits are written

Worldgen is a pure function of the seed (§8), so an unmodified chunk can be
regenerated instead of stored. A chunk is written only once it has been edited —
which is already exactly where `World::set_block` marks it.

Two consequences, one good and one that has to be guarded:

- Saves are tiny and proportional to what you actually built, not to how far you
  walked. At radius 64 that is the difference between a few KB and tens of
  thousands of chunks on disk.
- It makes reloading an **aggressive test of Rule 1**: if worldgen is not
  deterministic across runs, threads or platforms, the world visibly changes shape
  around your edits. That is the failure mode `worldgen_version` guards — when the
  generator changes, old worlds must not be silently regenerated into something
  else. Phase 2 decides whether the answer is migration or persisting generated
  terrain; phase 1 refuses to load and says so.

LOD nodes are never written. They hold no voxel data (§4) and are pure derived
geometry.

### 7.5 What pins it

- **Round trip:** generate, apply a scripted edit sequence, hash, save, load,
  hash — equal.
- **Fixture:** a world file committed to the repo loads to a known hash on
  Windows and macOS both.
- **Regeneration:** an unedited chunk that has been evicted and reloaded equals
  the chunk originally generated, bit for bit.
- **Byte stability:** saving the same world state twice produces identical bytes.

## §8 Worldgen

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

## §9 The tick loop and the sim/render seam

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

## §10 Player physics

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

## §11 The render path

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

## §12 Decisions, in one table

| # | Decision | Rejected alternative | Because |
|---|---|---|---|
| 1 | `BlockId(u16)` + per-chunk palette, `Uniform` fast path | one byte per voxel | memory at radius 64; and 256 types is not "thousands" |
| 2 | Registry from RON; **names** are the stable identity, numbers are per-world | a fixed id partition (byte of category + byte of member) | ids are plentiful; a *fixed division* of them is what binds — two mods claiming one category make a world unrecoverable, and a full category cannot borrow from an empty one |
| 3 | Materials authored with their **shapes**, expanded to a flat id space | one hand-written definition per material-and-shape pair | you write "oak" once and get its family; the combinatorial explosion stays in the registry |
| 3b | Shape and block state as distinct ids, not a byte per voxel | a third byte in the voxel array | palette compression makes flattening free; a shape byte costs 4 KB/chunk for data that has one or two distinct values per chunk |
| 3c | Only **edited** chunks are written; the rest regenerate from the seed | persisting all generated terrain | saves scale with what you built, not how far you walked — and reloading becomes a hard test of Rule 1 (guarded by `worldgen_version`) |
| 4 | 8-byte packed, **node-local** vertex | 28-byte world-space f32 | vertex-memory budget; precision far from origin; re-meshing cost later |
| 5 | `INDIRECT_FIRST_INSTANCE` → `instance_index` for per-node data | per-vertex node index | zero per-vertex cost, one path on both backends (fallback recorded) |
| 6 | LOD node = 2^L chunks, one mesh, one draw, 16³ samples | per-chunk voxel downsampling | draws are the binding constraint (§2); triangle reduction alone cannot reach radius 64 |
| 7 | Skirts at LOD seams | stitched transition geometry | local and parallel-safe; stitching serialises the mesher |
| 8 | Density function sampled at node resolution | generate full-res, downsample | 8.2M samples vs 545M for a full radius-64 load |
| 9 | Fixed 60 Hz tick, render-side interpolation | per-frame simulation | Rule 1; and it is a rewrite if deferred |
| 10 | `cubara-render` loses its `cubara-world` dependency | leave it | Rule 3; it is what makes the renderer rebuildable |

## §13 What phase 1 does not build

Inventory, items, crafting, mobs, health, day/night, trees, ores, water,
transparency, lighting propagation, multiplayer, mods. Also, deliberately: block
shapes and block state beyond the one-element expansion (§3.2), block entities
(§3.6), compression in the save format (§7.1), and preserving blocks whose mod has
been uninstalled (§7.2).

The claim this document makes is not that those will be easy. It is that each one
lands at a **named seam** — a registry expansion, a second render pass, a new
system in `cubara-sim`, a `format_version` bump over a chunk payload that already
holds palette-compressed ids and a world header that already holds the id table —
rather than as a change to the voxel array, the mesher, the arena and the save
format at the same time. If a phase-2 feature cannot find its seam, that is a
defect in this document and it gets fixed here.

[wgpu#6823]: https://github.com/gfx-rs/wgpu/issues/6823
