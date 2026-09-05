# Cubara — Roadmap

What ships, and when. [`REQUIREMENTS.md`](REQUIREMENTS.md) says why the project
exists; [`ARCHITECTURE.md`](ARCHITECTURE.md) says what the code must hold to;
[`PLAN.md`](PLAN.md) says how the engine works. This document says **what we are
building right now, and how we know it is finished**.

## Why this document exists

The engine had a milestone ladder (M0–M8) that was really a wish list: eight
milestones open at once, twenty-one issues sitting side by side, and no line
anywhere that said *this is done, stop, test it*. Scope that large with no edge is
indistinguishable from no scope at all — you cannot tell what to work on, and an
agent working autonomously will pick whatever is nearest and call it progress.

So the work is cut into **three phases**, and each phase has an **exit gate that a
script answers**, not a judgement someone makes. The project's standing rule
applies to phases too: *a requirement that isn't enforced by machinery is a wish.*

| Phase | Name | The one-sentence result | Status |
|---|---|---|---|
| **1** | The engine stands | A textured, walkable, cave-riddled world at render distance 64 that runs at 1000+ FPS — and is still there after you close it. | **Complete — 2026-08-24** (gate 12/12 on both machines) |
| **2** | The first survival world | You can survive in it: gather, craft, smelt, eat, and be attacked — and it is still there tomorrow. | **Active** |
| **3** | Modern depth, with synergy | The features of a full modern voxel game, each admitted only if it connects to what already exists. | Not started |

Phases are strictly sequential. Phase *n+1* does not start until phase *n*'s gate
passes **and** the project owner has played it.

---

## How a phase works — the autonomy contract

Inside a phase the agent works on its own. This section is the boundary of that
autonomy, and it is deliberately narrow in one direction only.

**The agent decides and acts, without checking in:** which block to pick up next
from the phase's ordered list, how to design and implement it, the crate layout,
the tests, the refactors, and the whole GitHub lifecycle — issue (to
[`docs/ISSUE_STANDARD.md`](docs/ISSUE_STANDARD.md)), branch, PR, CI, merge. No
permission is asked per step; that is the point of the phase.

**The agent stops and asks when:**

1. A **gameplay or content decision** appears that the phase does not already
   settle — what a mechanic does, what a block is for, what goes in a recipe.
   These belong to the project owner ([`CLAUDE.md`](CLAUDE.md)). A plausible
   invention is worse than a question, because it looks decided.
2. The **exit gate cannot be met** and the remaining levers are exhausted. The
   answer is never to move the gate; the answer is to report the measured number,
   the levers tried, and what changing the gate would cost.
3. The **phase boundary** is reached. The phase ends with a report, not with the
   next phase.

**The agent never:** marks a phase done without the gate script passing on both
machines; edits a gate to make it pass; or lets a phase's work leak into the next
one because it was convenient.

**A phase ends with a report** containing the gate output, the benchmark numbers
with deltas against the previous rows, what was built, and anything deliberately
left undone. Then the owner plays it.

---

## Phase 1 — The engine stands

**The result:** a world of three textured block types over seeded noise terrain
with real cave systems, which you walk through under gravity rather than fly
through, rendered to a horizon 64 chunks away at over 1000 FPS, and which you can
save and come back to.

This is the last phase that is purely about the engine. Everything in it is
foundation for phase 2, and three of its pieces — determinism, block identity,
and the save format — are here specifically because they are the ones that cannot
be retrofitted later without a rewrite or a migration. Persistence is in this
phase for a precise reason: the on-disk format and the in-memory block
representation are the same decision, so designing them apart means designing the
chunk layout twice and calling the second one a migration.

The full design is in
[`docs/PHASE1_ARCHITECTURE.md`](docs/PHASE1_ARCHITECTURE.md). It is not a summary
of this list; it is the architecture the list implements, and the ordered blocks
below refer to it.

### Ordered blocks

Each block is one issue (or one tracking issue with sub-issues) and lands as its
own PR. The order is a dependency order, not a preference.

| # | Block | Why here | Existing issue |
|---|---|---|---|
| **1.0** | Measure the target before building for it: `--bench 64` runs at all, `scripts/check-phase-gate.sh` exists and **fails**, baseline recorded | You cannot optimise toward a number you have never measured. This block's deliverable is a red gate and a row in `BENCHMARKS.md`. | [#89](../../issues/89) |
| **1.1** | Deterministic arena slab offsets | Open bug; the LOD work rewrites everything around the arena, so it is fixed before, not during. | [#83](../../issues/83) |
| **1.2** | `BlockId` + per-chunk palette compression — the end of `bool` | Every later system (textures, mining, inventory, saves) needs block identity. Nothing after this works without it. | [#46](../../issues/46) |
| **1.3** | Block registry from RON — **names are identity, numbers are per-world** | The one decision that would poison saves, mods and multiplayer if made later. See design §3. | [#54](../../issues/54) |
| **1.4** | Packed vertex + texture array, per-face materials, and the three original textures | The flat green in `mesh.wgsl` goes away. Three issues, three PRs. Pinned by golden-image tests. | [#43](../../issues/43), [#44](../../issues/44), [#55](../../issues/55) |
| **1.5** | Seeded 3D noise worldgen with cave systems | Caves are the honest meshing load — a smooth heightmap flatters the renderer. The seed becomes world state (Rule 1). | [#48](../../issues/48) |
| **1.6** | Fixed-timestep tick loop + seeded world RNG | ~100 lines now; a rewrite later. Rule 1 is the keystone and the only rule that cannot be retrofitted. | [#57](../../issues/57) |
| **1.7** | Player: AABB collision, gravity, walking; free-fly becomes a debug toggle; selected-block highlight | Runs in the sim at fixed tick rate, so movement is deterministic and phase 2 inherits it. | [#53](../../issues/53), [#52](../../issues/52) |
| **1.8** | Determinism harness: world-state hash + replay test, single- vs multi-threaded | Rule 1's missing enforcement, and the hash that block 1.9's round-trip test is built on. | [#90](../../issues/90) |
| **1.9** | **Save/load:** world header with the block id table, region files, chunk payload | The on-disk format and the in-memory block representation are one decision, so they are made in one phase. See design §7. | [#60](../../issues/60) |
| **1.10** | **LOD as draw-count reduction** — the region node tree, and `cubara-render` stops depending on `cubara-world` | The block that decides whether radius 64 is reachable at all. Tracking issue with sub-issues; see design §6 and §1. | [#38](../../issues/38) |
| **1.11** | Whatever block 1.10's measurements say is still missing | **Closed empty — the measurements said nothing was missing.** Radius 64 clears the gate on both machines with margin (M3 ~1.3×, Windows ~3.6×) and draws 1,341 nodes against the 2,000 budget §6.1 set. Occlusion culling ([#42](../../issues/42)) was the standing candidate and was **not** pulled forward: this block exists to be written from a profile, and the profile did not ask for it. It stays in phase 3. | none — see closeout |

### Exit gate

```bash
./scripts/check-phase-gate.sh 1
```

Passes only when **all** of the following hold, and it exits non-zero otherwise:

- `cargo test --all`, `cargo clippy --all-targets --all-features`, `cargo fmt --check` green.
- `./scripts/check-architecture.sh && ./scripts/check-single-render-path.sh` green.
- `cargo run --release -- --bench 64` reports **≥ 1000 FPS sustained**.
- The determinism replay test passes single-threaded and multi-threaded with an
  identical world-state hash.
- Golden-image tests cover: all three block types visible and textured, a cave
  mouth, and an LOD boundary at distance.
- Unit tests cover: a player AABB never passes through a solid voxel, and the
  same seed produces a bit-identical chunk on both platforms.
- **The isolation test:** a chunk generated on its own is bit-identical to the
  same chunk generated after a shuffled selection of its neighbours. This is what
  makes "regenerate anything that was not edited" sound — see design §8.1.
- **The save round-trip test:** edit a world, save it, load it, and land on the
  same world-state hash — and a world file committed as a fixture loads to that
  same hash on Windows and macOS both, which CI already runs.

**Run on both machines** (Windows/RTX 4060 and macOS/M3), with a `BENCHMARKS.md`
row for each. The perf half of the gate deliberately does **not** run in CI —
GitHub's runners have no representative GPU, and a perf gate that measures noise
is worse than none. CI instead gets a smoke test that a radius-64 world loads
headless inside a fixed memory and time bound, which is what would actually break
silently.

### Closeout — 2026-08-24

`./scripts/check-phase-gate.sh 1` at `90f764b`, **12 passed, 0 failed**, on both
machines. The perf criterion, radius 64:

| Machine | Nodes | Tris | FPS | CPU/frame | Gate margin |
|---|---|---|---|---|---|
| Win11 / RTX 4060 (Vulkan) | 1,585 | 758,754 | ~3,591 | ~0.11 ms | ~3.6× |
| macOS / Apple M3 (Metal) | 1,585 | 829,608 | ~1,275 | 0.535 ms | ~1.3× |

The M3 row predates the skirt-overlap fix (#125), which removed ~9% of the
geometry; it is the last measured M3 figure, not a stale claim about current
code. Draw count is what this scene is bound by, and it is unchanged at 1,341.

Block 1.11 closed empty — see its row above. The gate ran green *before* the
block existed to be written, which is the outcome the block was shaped to allow
for; writing something into it anyway would have been the freelancing the phase
contract forbids.

Two defects were found by running the gate rather than by a test, and both are
worth recording because neither was a code bug:

- **CI was red on `main` with no commit responsible** (#124). The workflow pins
  `dtolnay/rust-toolchain@stable`, so Rust 1.98 arrived on its own and brought a
  lint that `-D warnings` turned into an error. An unpinned toolchain means the
  build can break with no change to the repo; left unpinned, flagged here.
- **A benchmark row sat in the wrong machine's table** (#123), which made a
  hardware difference read as an unexplained 4× speedup on identical code. The
  recording instruction has been sharpened, but the general lesson is the one
  this file already argues: a number without its machine attached is not a
  measurement.

**Not started: phase 2.** Per the autonomy contract above, a phase ends with a
report and the owner playing it.

### Explicitly not in phase 1

Inventory, items, crafting, mobs, health, day/night, trees, ores, water,
transparency, multiplayer, mods. Blocks may be placed and broken (that already
works) and the result survives a restart, but nothing is *collected*. If one of
these turns out to be load-bearing for the gate, that is a finding to report, not
a licence to build it.

---

## Phase 2 — The first survival world

**The result:** the alpha-era survival loop, in the owner's stated order —
inventory first, then the content that gives it something to hold, which is what
forces the simulation to become a real game loop.

Blocks 2.1 – 2.4 are designed in
[`docs/PHASE2_ARCHITECTURE.md`](docs/PHASE2_ARCHITECTURE.md), which plays the
same role phase 1's design doc does: it is not a summary of the list below, it
is the architecture the list implements, and its decisions are binding. Read it
before touching items, inventory, crafting, drops, trees or the furnace. Blocks
2.5 – 2.9 are designed when they are reached, against what 2.1 – 2.4 actually
produced.

### Ordered blocks

| # | Block | Note |
|---|---|---|
| **2.1** | Items, inventory, and the hotbar | Broken blocks become items; blocks are placed from the hotbar. The first thing that makes the world feel owned. |
| **2.2** | Crafting: recipes from data files, a crafting grid | Data-driven like the block registry, and for the same reasons. |
| **2.3** | Trees and ores in worldgen; wood, leaves, iron ore | The content that gives 2.1 and 2.2 something to be about. A tree is also the first thing that wants to write outside its own chunk — it lands via the fixed-radius structure pass (design §8.4), which keeps generation pure and the save format sound. |
| **2.4** | Tools, mining, and smelting: chop a tree, mine iron, run a furnace | Closes `REQUIREMENTS.md` #5's alpha definition. A furnace is the first block that owns state over time. |
| **2.5** | ECS for entities | Arrives when there are entities worth having — dropped items and mobs — not before. [#56](../../issues/56) |
| **2.6** | Chunk state machine: `Active ⇄ Dormant` | [#47](../../issues/47) |
| **2.7** | Dormant-chunk catch-up (the Factorio timers) + a worked process | The founding chunk idea, and only now is there a process (a growing tree, a running furnace) real enough to test it against. [#58](../../issues/58), [#59](../../issues/59) |
| **2.8** | Extend the save format: block entities, entity state, inventory | Phase 1 ships the format (design §7); phase 2 adds the state phase 2 invented. A format version bump, not a new format. |
| **2.9a** | Health, fall damage, and regeneration | Shipped. [#172](../../issues/172) |
| **2.9b** | Hunger, food, and the first hostile mobs | **Deferred to phase 3 by the project owner, 2026-09-05.** It was put to them as a design question — what hunger *is*, where food comes from, which mobs exist — and the answer was "not yet". Phase 2's threat model is therefore fall damage alone. |

Note that the tick loop is *not* in this list — it lands in phase 1 (block 1.6).
Trees growing and furnaces smelting are what make the tick *interesting*, but a
frame-rate-dependent world would already have broken player physics in phase 1,
and determinism added after the fact is a rewrite. Phase 2 adds content to the
tick, not the tick itself.

### Exit gate

```bash
./scripts/check-phase-gate.sh 2
```

- Everything phase 1's gate checks, still passing — a perf regression blocks the
  phase (Rule 7), it is not noted and forgotten.
- **The survival replay test:** a fixed, scripted input sequence runs headlessly
  and completes the loop — chop a tree, craft a tool, mine iron ore, smelt it,
  take damage — then asserts a world-state hash. It runs single-threaded and
  multi-threaded and must agree.

  *Amended 2026-09-05.* This used to read "… smelt it, **eat**, take damage …".
  Eating left the list along with hunger, food and mobs when the owner deferred
  block 2.9b; damage is now a scripted fall, which since 2.9a is the only thing
  in this world that hurts. Recorded here rather than quietly edited, because a
  gate that moves without a note is a gate that does not hold. Changing one is
  the owner’s call, and this is the record of them making it.
- **The round-trip test**, extended from phase 1 to cover phase 2's state: save
  the world mid-script, reload it, run the rest of the script, and land on the
  same hash as the uninterrupted run.
- **The dormant test:** a chunk left dormant for N ticks and then activated ends
  in the same state as one simulated continuously for N ticks.

That third test is the real gate. If a scripted agent can survive in the world
without a human, the world is playable; if it cannot, no screenshot proves it is.

---

## Phase 3 — Modern depth, with synergy

**Deliberately not specified here, and that is the design.**

Phase 3 is where the features of a modern voxel game arrive — and where the
project's actual thesis gets tested, since the complaint about the market leader
is precisely that its features sit loosely side by side. A feature list written
now, two phases ahead of the systems it would attach to, would be exactly the
loose-features failure it is meant to avoid. It also would not survive contact
with what phase 2 teaches us.

What *is* decided now is the **admission rule**, because that is the machinery:

> A phase-3 feature is proposed in writing before it is built, and the proposal
> must name (a) which existing systems it deepens and how, (b) what it makes
> possible that was not possible before, and (c) what it replaces or removes.
> A feature that only adds is rejected or deferred — including features the
> mainstream game has. "It exists in the other game" is not an argument.

The feature set is designed **with the project owner** at the start of phase 3, as
a system, from that rule. Candidates already on the books —
[#42](../../issues/42) occlusion culling, [#32](../../issues/32)/[#33](../../issues/33)
GPU culling, shaders, multiplayer, the mod API — are engine work that will be
scheduled against phase 3's needs rather than pursued on their own.

---

## Where the existing open issues land

| Phase 1 | Phase 2 | Engine work, scheduled against phase 3 |
|---|---|---|
| #89, #83, #46, #54, #43, #44, #55, #48, #57, #53, #52, #90, #60, #38 | #56, #47, #58, #59 | #28, #32, #33, #42, #36 |

Each phase is the GitHub milestone of the same name, and every open issue is
filed under one — an issue that fits no phase is unscheduled work, not a reason
to start. Issue titles carry their block number, and **`./scripts/next-block.sh`
prints what to work on next** so that ordering is read rather than judged.

[#52](../../issues/52) (selected-block highlight) rides along with phase 1's
player work — you cannot aim at a block you cannot see you are aiming at.

The M0–M8 milestones in `PLAN.md` §7 are superseded by these phases. `PLAN.md`
keeps what it is good at — the technical approach and the recorded findings — and
no longer answers "what ships when"; this document does, and only this document.
