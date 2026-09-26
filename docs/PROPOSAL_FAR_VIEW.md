# Proposal: seeing hundreds of kilometres

**Status: proposed 2026-09-26. The direction is set, and nothing is built yet.**
The experiments behind the numbers are on the branch `exp/far-view-octree`: a
few constants changed and one probe added, not for merging.

## What the owner asked for, and has already decided

On 2026-09-14, after climbing a mountain (#264): *"verder zie je nog wel de
boundaries als je op de berg staat. zou chiller zijn als je echt oneindig ver
kon kijken."* On 2026-09-26 the owner answered four questions:

| Question | The owner's answer |
|---|---|
| How far? | *"daadwerkelijk oneindig … ik wil dat je daadwerkelijk honderden kilometers kan zien, als je hoog genoeg bent en het landschap mee werkt. hiervoor is het dus nodig dat lod extreem goed werkt"*. **262 km first**; go further only once that is measured. |
| What does the distance look like? | *"het moeten geen grote rechthoeken lijnen. ze moeten wel bergvormig zijn, maar je moet natuurlijk niet elk blok renderen. ik zou ff kijken wat hier slim is. het moet er wel uitzien alsof elk blok gerenderd is"* |
| Does the 1000-FPS gate measure what you see? | **Yes.** |
| Measured how? | **Three fixed first-person eyes**, at ground (y = 40), on a hill (y = 300) and in flight (y = 3,000), each at the full view distance and each at 1,000 FPS or more on both machines. |

The gate change is the owner's call, and this table is the record of him making
it. `ROADMAP.md` notes it as well.

The owner left one thing open on purpose: how to make distant terrain look as
if every block were rendered without rendering every block. That is §3. It is an
engineering choice, and the owner asked for it to be made well.

`ROADMAP.md` requires a phase-3 feature to be proposed in writing before it is
built. This is that proposal.

---

## §1 What exists now

| | Where | Value |
|---|---|---|
| Ring schedule | `crates/world/src/node.rs` `DEFAULT_RING_SCHEDULE` | levels 0–3, outer ring 64 chunks = **1,024 blocks** |
| Far plane | `crates/render/src/render.rs` `FAR_PLANE` | 2,000 blocks |
| Fog | `Lighting::fog_range(render_radius_blocks())` | ends inside 1,024 blocks, and exists to hide the edge |
| Vertical reach | `VERTICAL_LOD_SQUASH` = 2 | 512 blocks up or down, so **from 3 km up nothing is drawn** |

## §2 What the measurements say

All measured on the M3 (Metal) at 1920 × 1080 with a 60° vertical field of
view. FPS at these scene sizes is noisy between runs (`BENCHMARKS.md`), so CPU
per frame is the column to compare. The bench's fixed-eye camera turns a full
circle, pitched down about 14°.

### A. The obvious approach: keep doubling the rings

Rings extended to level 11, the far plane moved out of the way, nothing else
changed:

| Eye | View distance | Nodes drawn | Triangles | Vertex arena | FPS | CPU/frame |
|---|---|---|---|---|---|---|
| ground, y = 40 | 1 km (today) | 1,940 | 820k | 41% | 3,482 | 0.218 ms |
| ground, y = 40 | 262 km | 4,955 | 1.30M | 65% | 2,309 | 0.329 ms |
| hill, y = 300 | 1 km (today) | 869 | 437k | 22% | 3,203 | 0.275 ms |
| hill, y = 300 | 262 km | 3,963 | 928k | 46% | 2,401 | 0.280 ms |
| flight, y = 3,000 | 1 km (today) | **0: nothing is drawn** | | | | |
| flight, y = 3,000 | 262 km | 2,545 | 283k | 14% | 5,558 | 0.157 ms |

It is affordable, because each ring is twice as coarse as the one inside it. It
is also **not what the owner asked for**, for two reasons.

**It flattens the world.** A node is a cube with the same cell size on every
axis (`PHASE1_ARCHITECTURE.md` §6.2), so a surface can only sit at a cell
boundary. The probe (`far_view_probe` on the experiment branch) asks how high
two columns are drawn at each level. One is plain ground, truly at y = 28. The
other is the tallest peak in a 16 km square, truly at y = 242.

| Level | Cell | Drawn at | Plain (true 28) | Peak (true 242) |
|---|---|---|---|---|
| 0–3 | 1–8 blocks | 0–1 km | 29–32 | 232–243 |
| 4–6 | 16–64 | 1–8 km | **48–64** | 192–224 |
| 7–8 | 128–256 | 8–32 km | **0** | **256**, one box |
| 9–11 | 512–2,048 | 32–262 km | **0** | **0** |

From 8 km out, the land falls to a flat plane at y = 0. From 3 km up that plane
is what you see:

![From 3 km up with the rings extended: past the fine centre, a grey plane of
stone with islands of grass](img/far-view-high-r16384.jpg)

*The straight edge is where level 6 meets level 7, at 8 km. Beyond it are the
stone tops of the y = 0 plane. The green islands are hills tall enough to
survive a 128-block cell.*

**It is made of big rectangles, by construction.** Level *L* is used from
64 × 2^*L* blocks away, so every coarse cell appears at 1/64 of a radian, which
is **about 16 pixels wide**. That holds at every level and every distance, and
it is exactly the "grote rechthoeken" the owner does not want. The rectangles
are already there today, between 160 m and 1 km:

| Level | Starts at | One cell on screen | Today? |
|---|---|---|---|
| 1 | 160 blocks | 13 px | yes |
| 2 | 288 | 14 px | yes |
| 3 | 512 | 16 px | yes |
| 4 and further | 64 × 2^*L* | 16 px | only in the experiment |

### B. When is a block smaller than a pixel?

At 1080p a pixel is 0.97 milliradians. A block is therefore one pixel wide at
**1,030 blocks**, and smaller than a pixel beyond that. At 1440p that distance
is 1,375 blocks, and at 4K it is 2,060.

This splits the problem in two:

- **Beyond ~1 km no single block can be seen.** What can be seen is the shape of
  the land (its silhouette and height), and the colour a pixel's worth of blocks
  adds up to: grass tops, soil and stone on the steps between them, lit by the
  sun from one side. Reproduce both and it is indistinguishable from rendering
  every block. Neither needs a block to be drawn.
- **Within ~1 km blocks are 1 to many pixels**, and there is no shortcut: a
  block that can be seen has to be drawn as a block.

### C. What drawing every visible block within 1 km costs

The near field was given a schedule in which no cell is wider than 2 px: level
0 out to 512 blocks and level 1 out to 1 km. The vertex arena had to be enlarged
just to hold it.

| Eye | Today: FPS / meshed triangles | ≤ 2 px cells: FPS / meshed triangles |
|---|---|---|
| ground, y = 40 | 3,482 / 0.82M | **955** / 4.86M |
| hill, y = 300 | 3,203 / 0.44M | **695** / 3.98M |

That is six times the geometry, and **below the gate** on the M3. So the band
between 160 m and 1 km cannot be made pixel-exact by adding geometry. §3.5 says
what this proposal does about it: nothing yet, deliberately.

---

## §3 The design

### 3.1 The criterion, made checkable

*"Het moet eruitzien alsof elk blok gerenderd is"* becomes: **no simplification
changes what a pixel shows by more than about a pixel of geometry, compared
with drawing every block.** The two regimes in §2B meet it in different ways,
and each way is a pure function that a unit test can check:

- For geometry, the projected size of every cell drawn is at most *k* px. This
  is a function of the schedule and the eye, and needs no GPU.
- For colour, the aggregate block shading (3.3) of a staircase must match the
  area-weighted colour of the actual blocks it stands for, computed by brute
  force on the CPU for a set of slopes and view directions.

The owner judges the result with his eyes, from golden images taken at the
three gate eyes. The tests exist so that a later commit cannot quietly undo
what he approved.

### 3.2 Two regimes, two representations, no overlap

| Distance | What is drawn | How |
|---|---|---|
| **0 – ~1 km** | voxels, as today | the node tree, unchanged: rings capped at 1,024 blocks |
| **~1 km – 262 km** | **far terrain**, new | a height-field mesh with block-aggregate shading (3.3) |

The far terrain is the new system. It **does not extend the node tree**: table A
shows that rings of cubes flatten the land and are made of rectangles. Instead:

- **A quadtree of height-field patches**, each a grid of 32 × 32 quads
  (`crates/world/src/far.rs`). A patch is split for two reasons:
  - **by distance**, while its quads would look wider than 16 px. That caps
    the triangle count by the screen, not by the view distance;
  - **by height error**, while its own measured gap to the next finer patch
    would look taller than 1 px. This puts fine patches where the land is
    rough (mountains) and leaves flat land coarse. It stops at a floor of
    2 px quads, because below that it only chases one-block steps, which a
    smooth surface cannot follow anyway.
- **Why not distance alone, as CDLOD does (*revised 2026-09-26, measured in
  F3a*).** The first plan was CDLOD (Strugar 2010): distance-only splitting,
  with geomorphing between levels. Measured from the hill eye out to 25 km,
  against the ground each pixel covers:

  | Quads by distance | Height bound | Mean error | p90 | Triangles, 262 km (all patches) |
  |---|---|---|---|---|
  | 16 px | none | 0.61 px | 1.58 px | 1.2M |
  | 8 px | none | 0.39 px | 1.04 px | 3.3M |
  | 4 px | none | 0.24 px | 0.61 px | ~13M |
  | **16 px** | **1 px** | **0.30 px** | **0.75 px** | **3.7M** |
  | 16 px | 2 px | 0.55 px | 1.42 px | 1.5M |

  Under distance alone, getting nine columns in ten under a pixel takes 4 px
  quads everywhere, which is about 13M triangles. The height bound gets the
  same accuracy for about a quarter of that. The triangles in view at once
  are about a quarter of "all patches", but the bench measures that with the
  real frustum (F3c).
- **Skirts, not geomorphing.** A split for roughness can leave a fine patch
  beside one two levels coarser, and geomorphing cannot blend across that.
  Each patch drops a skirt along its edges, as the voxel nodes already do
  (`PHASE1_ARCHITECTURE.md` §6.4). The popping that geomorphing would have
  hidden is kept under a pixel by the same height bound.
- **Heights come from the generator, prefiltered.** A vertex's height is the
  mean of `WorldGen::surface_height` over the footprint of its quad, sampled
  on a grid of at most 4 × 4. The terrain
  is a height field with caves beneath it (§8.1), so from kilometres away the
  height field is the complete visible truth. The patches are generated on the
  existing worker threads, like today's mesh jobs, and hold 33 × 33 heights plus
  a material summary each. That is small enough that the whole 262 km view fits
  in a few megabytes.
- **Vertical precision is not tied to cell size.** Heights are real numbers, not
  lattice steps, which is exactly what table A's cubes lack.

### 3.3 Block-aggregate shading: making it look like every block

A slope made of blocks is a staircase. From far away, each pixel covers many
steps, and the colour it shows is a mix of two kinds of face:

- **top faces** (grass, snow and so on), normal straight up, lit by the sun
  from above;
- **side faces** (soil near the top of a step, stone on tall cliffs), normals
  along ±x and ±z, lit or in shadow depending on which way they face the sun.

For a slope with gradient (*g*ₓ, *g*_z) in blocks per block, every unit of
ground carries one unit of top area and |*g*ₓ| and |*g*_z| units of side area.
How much of each is visible depends on the view direction. The fragment shader
knows the gradient (from the patch's heights), the view direction, the sun, and
the materials, so it computes the **area-weighted, visibility-weighted mix of
the faces the blocks really have**. A gentle hill comes out mostly grass. A
cliff comes out stone-grey with its sunlit side brighter than its shaded side,
and it reads as a cliff made of blocks rather than a smooth grey slope. This is
what makes the distance look like blocks without a block being drawn.

The CPU brute-force check in 3.1 is what keeps this honest: generate the actual
staircase, render its faces' areas and lighting by counting, and compare.

### 3.4 Joining the two

- The far terrain starts where the voxel rings end. It is drawn after the voxels
  with a depth test, and discards anything inside the voxel region, so the two
  never draw the same ground.
- At the join, a block is about 1 px wide at 1080p (§2B). At 4K it is about
  2 px, so the join is slightly visible there. The join distance can be made to
  follow the resolution.
- It needs one new pipeline, built in `scene.rs` like every other one, inside
  the one `encode_scene`. The window, the bench and the screenshot keep sharing
  that single path, so Rule 5 and `check-single-render-path.sh` hold as they
  are.
- **The far plane becomes infinite** (free with reverse-Z), and **fog becomes
  haze**: an atmosphere tens of kilometres thick rather than a wall hiding an
  edge. The owner sets its strength by looking at it.

### 3.5 What this does not solve, on purpose

- **The band between 160 m and 1 km.** Its cells are 13–16 px wide today (§2A),
  and making them pixel-exact costs six times the geometry and the gate (§2C).
  Options for later, each to be judged from images: (a) leave it as it is; (b)
  keep coarse geometry but give the tops of coarse cells the colours of their
  actual blocks, so only the silhouette is coarse; (c) bring the far terrain
  closer, which smooths blocks that are still 2–4 px wide. That choice is the
  owner's, once F3 gives images to judge from.
- **Caves, overhangs and trees beyond 1 km.** Caves and overhangs are not in a
  height field. A 10-block cave mouth at 1 km is about 10 px, so a dark spot in
  the material summary may be worth adding. Forests could be a darker, bumpier
  green in the material summary. Both are for F6, if the images call for them.
- **Your builds in the distance.** They show only in the voxel region, as they
  do today.
- **Travelling hundreds of kilometres.** Seeing 262 km needs nothing more.
  Standing 262 km from the origin needs camera-relative rendering, because
  `f32` positions jitter at that distance. That is separate work.
- **The world is flat.** Nothing curves away. Past a point, how far you see
  depends on what the land hides, not on the curvature of the world.

## §4 The admission rule

- **(a) What it deepens.** The worldgen: mountain ranges (#259) and the unlimited
  height of the world (#175) become things you see from far away and navigate
  by. Climbing and flying in creative mode now show you something. The generator
  gets a second consumer, which uses the same pure function.
- **(b) What it makes possible.** Finding your way by landmarks. Seeing the range
  you will walk to next. Building high in order to look out.
- **(c) What it replaces.** The edge of the world: the fog wall, `FAR_PLANE`, and
  the 1,024-block limit on what exists visually. It also replaces `--bench 64`'s
  orbit as the gate (the owner's decision, above). The ring extension of §2A,
  the obvious approach, is rejected rather than added alongside.

## §5 Decisions left for the owner

1. **The haze.** How thick, judged from golden images.
2. **The band between 160 m and 1 km** (3.5), judged from F3's images.

Both come with images, not questions in the abstract.

## §6 Plan, in order

| # | Block | Why here |
|---|---|---|
| **F1** | **The gate first.** `--bench` gains the three eyes, and `check-phase-gate.sh` uses them. They are recorded on both machines before anything is built. | The same order as phase 1's block 1.0: measure the target first. The eyes' view distance grows with F3, and the gate grows with it. |
| **F2** | **The criterion as tests.** The projected-cell-size function, with a test that today's rings fail it at 13–16 px, which proves the test can fail. Plus the brute-force staircase reference for 3.3. | So F3 and F4 are built against a check rather than a screenshot. |
| **F3** | **Far terrain, plain-shaded.** The patches, split by distance and by height error, with skirts and prefiltered heights; the infinite far plane; the join at 1 km. F3a (the GPU-free half: `crates/world/src/far.rs`) is first. | The core. Golden images at the three eyes, before and after. |
| **F4** | **Block-aggregate shading** (3.3) | What makes it look like blocks. |
| **F5** | **Haze** replaces the fog that hides the edge | Tuned by the owner from images. |
| **F6** | Whatever F1–F5's measurements and images say is still missing | For example the 160 m – 1 km band, cave mouths, or forests. Written from evidence, as block 1.11 was. |

**The Windows laptop is needed at F1 and F3**: the gate runs on both machines.

## §7 How it is checked

- **Geometric error:** a unit test that every drawn far-terrain quad projects to
  at most *k* px from each gate eye. It is a pure function, with no GPU.
- **Colour:** a unit test that aggregate shading matches brute-force block
  counting, within a tolerance, over a grid of slopes, view directions and sun
  directions.
- **Silhouette:** the probe's peak and plain, drawn by the far terrain within
  one pixel of their true height at every distance. Today's rings fail this
  from 1 km (§2A), so it is a test that can fail.
- **Golden images** at the three gate eyes, looked at by the owner before
  they are committed.
- **A `BENCHMARKS.md` row per gate eye.**

## §8 Sources

- F. Strugar, *Continuous Distance-Dependent Level of Detail for Rendering
  Heightmaps*, JGT 2010 ([pdf](https://aggrobird.com/files/cdlod_latest.pdf)):
  the quadtree, the distance-based split, and geomorphing without skirts. It
  was the first plan, and §3.2 records why the measurements replaced its
  distance-only split with a height-error bound and skirts (the chunked-LOD
  idea: split where the error would show, not everywhere at a distance).
- A. Tevs, I. Ihrke, H.-P. Seidel, *Maximum mipmaps for fast, accurate, and
  scalable dynamic height field rendering*, I3D 2008
  ([ACM](https://dl.acm.org/doi/10.1145/1342250.1342279)). Ray-casting a height
  field is the alternative. It gives exact blocks at every distance, at a cost
  per pixel. It was not chosen because that cost lands on a 1 ms frame budget,
  and point-sampling sub-pixel blocks needs filtering anyway, which gives back
  what 3.3 already does.
- Voxel-game LOD mods (Distant Horizons, Voxy) render simplified voxel geometry
  far away. That is the approach of §2A, and its cells are the rectangles the
  owner rejected.
