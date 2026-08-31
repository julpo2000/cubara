# Proposal: a world with no height limit

**Status:** proposed, not scheduled. **Owner's request, 2026-08-31:** *"duizenden
blokken diep kunnen graven en omhoog kunnen bouwen. Gigantische bergen en
ravijnen."*

`ROADMAP.md` requires a phase-3 feature to be **proposed in writing before it is
built**, naming (a) which existing systems it deepens and how, (b) what it makes
possible that was not possible before, and (c) what it replaces or removes. This
is that proposal. It is deliberately not an implementation plan for today — see
§6.

---

## §1 What exists now, and why

The world is **48 blocks tall**. Chunks are 16³ and exactly three chunk-layers
(`y` 0–2) are streamed and simulated:

| | Where | Value |
|---|---|---|
| Streamed band | `crates/app/src/streaming.rs` | `STREAM_Y_MIN = 0`, `STREAM_Y_MAX = 2` |
| Simulated band | `crates/world/src/world.rs` | `chunk_y_range() -> 0..=2` |
| Terrain shape | `crates/world/src/worldgen.rs` | base `24`, amplitude `±14` |

The surface sits around `y` 10–38, so a player has a few dozen blocks of sky and
**ten to thirty-five blocks of digging** before the world ends.

This is not an accident. `PHASE1_ARCHITECTURE.md` calls it a *"thin 3-chunk
vertical band"* — a deliberate phase-1 simplification so LOD could be measured
horizontally first. `REQUIREMENTS.md` #2 promises practically infinite **render
distance**, which is horizontal; vertical extent was never specified.

**Three foundations already point the right way**, and they are why this is not a
rewrite:

- **Chunks are cubic on purpose.** `PHASE1_ARCHITECTURE.md` §7 says a
  non-cubic region "would reintroduce the vertical special case that cubic
  chunks exist to avoid".
- **`ChunkCoord` is `i32`** — addressing is already effectively unbounded.
- **`WorldGen::density(x, y, z)` has no `y` bounds.** It is a pure function and
  already answers correctly at `y = -5000`.

So the limit is two constants, not the data model.

---

## §2 The measurement that changes the shape of this proposal

The intuition — *"a taller world means more chunks, so it will be slower"* — is
**wrong**, and measuring it first saved proposing the wrong work.

**Extending the band *upward* is free.** Radius 64, terrain unchanged, band
`0..=N` — that is, adding chunk-layers above the surface:

| Band | Nodes | Triangles | FPS |
|---|---|---|---|
| `0..=2` (today) | 1,600 | 822,420 | 1,387 |
| `0..=7` | 1,600 | 822,420 | 1,382 |
| `0..=15` | 1,600 | 822,420 | 1,388 |

**Identical** — air meshes to nothing.

### §2.0 Correction: downward is *not* free, and the first version of this document said it was

The table above was originally read as "vertical extent is nearly free". **That
conclusion was wrong, and it was wrong because every band measured started at
`0` and grew upward — into air.** Measuring downward, into rock, gives the
opposite answer:

| Band | Nodes | Triangles | FPS | Gate |
|---|---|---|---|---|
| `0..=2` (today, mostly air) | 1,600 | 822,420 | **1,390** | MET |
| `-1..=1` | 2,635 | 1,406,392 | **866** | **FAILED** |
| `-2..=2` | 3,138 | 1,512,280 | **790** | **FAILED** |
| `-3..=3` | 3,819 | 1,679,574 | **706** | **FAILED** |
| `-4..=4` | 4,260 | 1,760,268 | **670** | **FAILED** |

`-1..=1` has the *same number of layers* as today and still nearly doubles the
triangle count.

**The cause is caves, and it is not waste.** The cave field (§8.3) is 3D noise
with no lower bound, so it keeps carving at every depth — every underground chunk
is full of real cave walls. That geometry is genuinely there; it is simply not
visible from outside the rock.

Which means **no choice of band fixes this.** Any underground rock within render
range costs, so the answer is not a smaller vertical radius — it is not drawing
what cannot be seen.

**What costs is surface area — which is exactly what mountains and ravines are.**
Same radius, terrain amplitude raised:

| Terrain | Band | Nodes | Triangles | FPS | Gate |
|---|---|---|---|---|---|
| ±14, base 24 | 3 | 1,600 | 822,420 | **1,390** | MET |
| ±60, base 128 | 16 | 5,513 | 1,999,992 | **930** | **FAILED** |
| ±120, base 128 | 16 | 5,096 | 1,999,994 | **735** | **FAILED** |
| ±250, base 256 | 32 | 6,662 | 1,999,996 | **821** | **FAILED** |

### §2.1 The number that matters more than the FPS

Every mountain run **hit the arena ceiling**:

```
arena v 3999984/4000000, i 5999976/6000000, d 5513/4096
                                            ^^^^^^^^^^^
4096/5513 nodes drawn
```

The vertex buffer, the index buffer **and** the draw-slot capacity are all full.
Geometry is being **silently dropped** — 1,417 of 5,513 nodes never drawn.

So those FPS figures are measured on a **truncated world**, and are a *lower
bound* on the real cost. The honest reading is not "it runs at 930 FPS"; it is
"it does not fit, and what does fit runs at 930".

Note also that ±120 produced *fewer* nodes than ±60: bigger mountains mean more
chunks that are entirely solid or entirely air. The relationship between terrain
scale and cost is not monotonic, which is another reason to design this against
measurements rather than intuition.

---

## §3 (a) Which existing systems this deepens

- **The LOD node tree (block 1.10).** `desired_nodes` already divides its
  vertical band per level (`y_range.start().div_euclid(extent)`), so the machinery
  for coarse vertical detail exists. What it lacks is a *player-relative* vertical
  extent per ring — today the band is one global constant for every level.
- **Worldgen (block 1.5).** `density` is already unbounded in `y`. Mountains and
  ravines are a change to the *noise*, not to the contract.
- **The chunk arena.** Fixed capacities (4M vertices, 6M indices, 4096 draws)
  become the binding constraint rather than a comfortable margin.
- **The chunk lifecycle (block 2.6).** `chunk_y_range()` is hardcoded to `0..=2`
  and would follow the player instead.

## §4 (b) What it makes possible that is not possible now

- Digging thousands of blocks down, and building thousands up.
- Mountains and ravines as *terrain features* rather than as ±14-block texture.
- Caves with real vertical extent — the cave noise (§8.3) is already 3D and is
  currently squeezed into a 48-block band.
- Ore distribution that means something: `iron_ore` declares `max_y: 40` with no
  minimum, which is nearly the whole world today.

## §5 (c) What it replaces or removes

- **`STREAM_Y_MIN`/`STREAM_Y_MAX` and `chunk_y_range()`** — replaced by a
  player-relative vertical extent, per ring.
- **The fixed arena capacities** — replaced by capacities sized from the
  schedule, or grown on demand. This is the part that cannot be skipped: without
  it the world is silently truncated, which is worse than being slow.
- **`TERRAIN_BASE_HEIGHT`/`TERRAIN_AMPLITUDE` as the whole of terrain shape** —
  a single sine-ish amplitude does not make mountains. Ravines in particular are
  a *carving* pass, and the current cave noise is one octave with a blob
  threshold.

---

## §6 What this needs before it can be built

**It is blocked on perf work that is already on the books**, and this is the
first thing that actually justifies it:

- **[#42](../../issues/42) occlusion culling.** This is the whole blocker, and
  §2.0 is why: underground cave surfaces are real geometry that is invisible from
  outside the rock. Block 1.11 declined to pull occlusion culling forward because
  *"the profile did not ask for it"*. **This profile asks**, and it asks for
  nothing else first — even the simplest form of vertical world fails the gate
  without it.
- **[#32](../../issues/32)/[#33](../../issues/33) GPU culling.**
- **Arena capacity** — sized or dynamic, per §2.1.

`ROADMAP.md` already anticipates exactly this: those issues are *"engine work
that will be scheduled against phase 3's needs rather than pursued on their
own"*. This is that need.

## §7 Sequencing — the owner's call, not the agent's

`ROADMAP.md`: **"Phases are strictly sequential. Phase n+1 does not start until
phase n's gate passes and the project owner has played it."**

Phase 2 is at **10/11**. Its one red criterion is the survival replay test, which
needs eating — and hunger, food and mobs are undecided gameplay awaiting the
owner. So phase 3 cannot start under the roadmap's own rule.

There is one argument for taking it earlier, and it should be weighed rather than
assumed: **this changes `WORLDGEN_VERSION`, which invalidates every existing
save.** Doing it before anyone has a world worth keeping is cheaper than after.
That is a real cost of waiting, and it is the owner's to price.

**Nothing in this proposal has been implemented.** The measurements in §2 were
taken by temporarily editing constants, benchmarking, and reverting; `main` is
untouched.
