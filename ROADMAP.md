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

| Phase | Name | The one-sentence result |
|---|---|---|
| **1** | The engine stands | A textured, walkable, cave-riddled world at render distance 64 that runs at 1000+ FPS. |
| **2** | The first survival world | You can survive in it: gather, craft, smelt, eat, and be attacked — and it is still there tomorrow. |
| **3** | Modern depth, with synergy | The features of a full modern voxel game, each admitted only if it connects to what already exists. |

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
through, rendered to a horizon 64 chunks away at over 1000 FPS.

This is the last phase that is purely about the engine. Everything in it is
foundation for phase 2, and two of its pieces — determinism and stable block
identity — are here specifically because they are the ones that cannot be
retrofitted later without a rewrite.

The full design is in
[`docs/PHASE1_ARCHITECTURE.md`](docs/PHASE1_ARCHITECTURE.md). It is not a summary
of this list; it is the architecture the list implements, and the ordered blocks
below refer to it.

### Ordered blocks

Each block is one issue (or one tracking issue with sub-issues) and lands as its
own PR. The order is a dependency order, not a preference.

| # | Block | Why here | Existing issue |
|---|---|---|---|
| **1.0** | Measure the target before building for it: `--bench 64` runs at all, `scripts/check-phase-gate.sh` exists and **fails**, baseline recorded | You cannot optimise toward a number you have never measured. This block's deliverable is a red gate and a row in `BENCHMARKS.md`. | new |
| **1.1** | Deterministic arena slab offsets | Open bug; the LOD work rewrites everything around the arena, so it is fixed before, not during. | [#83](../../issues/83) |
| **1.2** | `BlockId` + per-chunk palette compression — the end of `bool` | Every later system (textures, mining, inventory, saves) needs block identity. Nothing after this works without it. | [#46](../../issues/46) |
| **1.3** | Block registry from RON, with **stable string ids** | The one decision that would poison saves, mods and multiplayer if made later. See design §3. | [#54](../../issues/54) |
| **1.4** | Texture array + per-face texture indices + the material shader | Three original 16×16 textures; the flat green in `mesh.wgsl` goes away. Pinned by a golden-image test. | [#43](../../issues/43), [#44](../../issues/44) |
| **1.5** | Seeded 3D noise worldgen with cave systems | Caves are the honest meshing load — a smooth heightmap flatters the renderer. The seed becomes world state (Rule 1). | [#48](../../issues/48) |
| **1.6** | Fixed-timestep tick loop + seeded world RNG | ~100 lines now; a rewrite later. Rule 1 is the keystone and the only rule that cannot be retrofitted. | [#57](../../issues/57) |
| **1.7** | Player: AABB collision, gravity, walking; free-fly becomes a debug toggle | Runs in the sim at fixed tick rate, so movement is deterministic and phase 2 inherits it. | [#53](../../issues/53) |
| **1.8** | **LOD as draw-count reduction** — the region node tree, and `cubara-render` stops depending on `cubara-world` | The block that decides whether radius 64 is reachable at all. Tracking issue with sub-issues; see design §6 and §1. | [#38](../../issues/38) |
| **1.9** | Determinism harness: world-state hash + replay test, single- vs multi-threaded | Rule 1's missing enforcement. Turns "deterministic" from prose into a CI failure. | new |
| **1.10** | Whatever block 1.8's measurements say is still missing — occlusion culling is the standing candidate | Deliberately unspecified: it is chosen from a profile, not from a guess. | [#42](../../issues/42) |

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

**Run on both machines** (Windows/RTX 4060 and macOS/M3), with a `BENCHMARKS.md`
row for each. The perf half of the gate deliberately does **not** run in CI —
GitHub's runners have no representative GPU, and a perf gate that measures noise
is worse than none. CI instead gets a smoke test that a radius-64 world loads
headless inside a fixed memory and time bound, which is what would actually break
silently.

### Explicitly not in phase 1

Inventory, items, crafting, mobs, health, day/night, save/load, trees, ores,
water, transparency, multiplayer, mods. Blocks may be placed and broken (that
already works) but nothing is *collected*. If one of these turns out to be
load-bearing for the gate, that is a finding to report, not a licence to build it.

---

## Phase 2 — The first survival world

**The result:** the alpha-era survival loop, in the owner's stated order —
inventory first, then the content that gives it something to hold, which is what
forces the simulation to become a real game loop.

### Ordered blocks

| # | Block | Note |
|---|---|---|
| **2.1** | Items, inventory, and the hotbar | Broken blocks become items; blocks are placed from the hotbar. The first thing that makes the world feel owned. |
| **2.2** | Crafting: recipes from data files, a crafting grid | Data-driven like the block registry, and for the same reasons. |
| **2.3** | Trees and ores in worldgen; wood, leaves, iron ore | The content that gives 2.1 and 2.2 something to be about, and the first blocks with *behaviour* rather than just appearance. |
| **2.4** | Tools, mining, and smelting: chop a tree, mine iron, run a furnace | Closes `REQUIREMENTS.md` #5's alpha definition. A furnace is the first block that owns state over time. |
| **2.5** | ECS for entities | Arrives when there are entities worth having — dropped items and mobs — not before. [#56](../../issues/56) |
| **2.6** | Chunk state machine: `Active ⇄ Dormant` | [#47](../../issues/47) |
| **2.7** | Dormant-chunk catch-up (the Factorio timers) + a worked process | The founding chunk idea, and only now is there a process (a growing tree, a running furnace) real enough to test it against. [#58](../../issues/58), [#59](../../issues/59) |
| **2.8** | Save/load: region file format | [#60](../../issues/60) |
| **2.9** | Health, hunger, damage, and the first hostile mobs | The largest block, and last: it is the one that needs every system before it. |

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
  eat, take damage — then asserts a world-state hash. It runs single-threaded and
  multi-threaded and must agree.
- **The round-trip test:** save the world mid-script, reload it, run the rest of
  the script, and land on the same hash as the uninterrupted run.
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
| #83, #46, #54, #43, #44, #48, #57, #53, #38, #52 | #56, #47, #58, #59, #60 | #28, #32, #33, #42, #36 |

[#52](../../issues/52) (selected-block highlight) rides along with phase 1's
player work — you cannot aim at a block you cannot see you are aiming at.

The M0–M8 milestones in `PLAN.md` §7 are superseded by these phases. `PLAN.md`
keeps what it is good at — the technical approach and the recorded findings — and
no longer answers "what ships when"; this document does, and only this document.
