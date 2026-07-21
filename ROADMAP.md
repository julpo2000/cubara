# Cubara — Roadmap to 1.0

This document defines **what 1.0 is** and the milestone ladder that gets there.
[`REQUIREMENTS.md`](REQUIREMENTS.md) says *why* the project exists;
[`PLAN.md`](PLAN.md) says *how* the engine is built; this file says *what ships,
in what order, and when a phase is allowed to be called done*.

Every milestone here is a GitHub milestone. Every issue belongs to exactly one.

---

## 1. What 1.0 is

**Cubara 1.0 is a complete, self-contained survival voxel game that matches the
feature bar the incumbent voxel game had reached by its 2012 era — and beats it on
the four things that game never fixed.**

The 2012 bar is the *content* target, not a design brief. It means a player can:
generate a world with varied biomes, caves, ores and structures; mine and place
blocks with tools that wear out; craft and smelt via a real recipe system; carry
and store items; farm crops and breed animals; fight and be fought by mobs; eat,
take damage, die and respawn; enchant gear and brew potions; wire redstone logic;
travel to a second dimension; and trade with villagers. Singleplayer and
multiplayer are the same game.

**The four pillars are what make it worth building.** These are not stretch goals;
1.0 does not ship without them:

| Pillar | 1.0 commitment |
|---|---|
| **Performance** | 1000+ FPS in the benchmark scene, sustained; no frame-time spikes during streaming, meshing, or lighting updates; render distance far past the incumbent's practical limit via LOD. |
| **Shaders** | A real shader pipeline with loadable shader packs — not a hardcoded look. Shadows, water, and post-processing are pack-driven. |
| **Modding** | Content is data, not code. Adding blocks, items, recipes, and behaviour is a data/script change with no engine recompile, and mods survive engine updates. |
| **Multiplayer** | Client–server from the ground up. Singleplayer runs the same server in-process, so there is no second code path to keep in sync and no "multiplayer is different" class of bug. |

### Cohesion over feature count

REQUIREMENTS.md #5 asks for features that *synergise* rather than sit loosely side
by side. Concretely, for 1.0 that means each system below is expected to compose
with the others rather than be a silo:

- The **dormant-chunk timer** model (PLAN.md §5) is what drives crop growth,
  furnace smelting, and mob spawning while you are away — one mechanism, three
  features, not three ad-hoc timers.
- The **block registry** is what lighting, meshing, physics, tools, and drops all
  read. A new block gets correct behaviour everywhere from one data file.
- The **ECS** is what players, mobs, dropped items, and projectiles all live in.
- **Shader packs** and the **mod API** address the same renderer, not a bolted-on
  second path.

An issue that introduces a parallel mechanism where one already exists should be
rejected in review, regardless of whether it works.

---

## 2. Release gates

A phase closes only when its gate is demonstrably met. Gates are checked, not
asserted.

| Gate | Meaning |
|---|---|
| **Engine gate** (M1, already met) | 1000+ FPS in the benchmark scene. Re-validated at every subsequent milestone; a regression blocks the milestone. |
| **Alpha** | A player can survive their first day: spawn, chop trees, craft tools, mine iron, build shelter, survive a night. |
| **Beta** | Feature-complete singleplayer. Everything in §1 exists and works; remaining work is bugs, balance, and polish. |
| **1.0** | Beta plus all four pillars, on Windows and macOS, packaged and installable, with the perf gate re-validated on both. |

---

## 3. Milestone ladder

### Engine foundation — M0–M8

Building the thing that makes the game possible. Largely complete.

| Milestone | Delivers | Status |
|---|---|---|
| **M0 — Setup** | Repo, CI, window, clear screen. | Done |
| **M1 — First chunk** | A chunk of cubes on screen; **1000-FPS gate met**. | Done |
| **M2 — Meshing & culling** | Greedy meshing, hidden-face + frustum culling, simple worldgen. | Done |
| **M3 — Streaming** | Load/unload chunks around the player; endless world. | Done |
| **M3.5 — GPU-driven rendering** | Shared chunk arena, `multi_draw_indirect`, capability-driven cull strategy. | In progress |
| **M4 — LOD** | Distant chunks at lower resolution; large render distance. | In progress |
| **M5 — Player & interaction** | Camera, raycasting, place/break. | In progress |
| **M6 — Data-driven content** | Block registry from data files, texture atlas, base block set. | Open |
| **M7 — Simulation** | Tick system + dormant-chunk catch-up (the timer model). | Open |
| **M8 — Persistence** | Save/load the world. | Open |

### Survival core — M9–M18 → **Alpha**

Everything needed for a player to survive a first day.

| Milestone | Delivers |
|---|---|
| **M9 — Lighting** | Sky light + block light propagation, smooth lighting, torches, day/night cycle. Incremental relight that never spikes the frame. |
| **M10 — Player physics** | AABB collision against voxels, gravity, jumping, step-up, swimming, fall damage. |
| **M11 — Fluids** | Water and lava: source blocks, flow, translucent rendering, buoyancy, drowning. |
| **M12 — Worldgen II** | Layered-noise biomes, cave systems, ore distribution by depth, surface decoration. |
| **M13 — Items & inventory** | Item stacks, hotbar, inventory UI, dropped-item entities, pickup, containers/chests. |
| **M14 — Crafting & smelting** | Data-defined recipes, 2×2 and 3×3 crafting, furnace smelting driven by the timer model. |
| **M15 — Tools & mining** | Tool tiers, per-block break speed, correct-tool drops, durability. |
| **M16 — Entities & ECS** | ECS integration; entity transform, rendering, animation, and spatial queries. |
| **M17 — Mobs & AI** | Passive and hostile mobs, pathfinding, spawning and despawning rules, light-driven spawning. |
| **M18 — Combat & survival stats** | Health, hunger, armour, melee and ranged combat, damage types, death and respawn. |
| **Alpha** | **Gate: survive the first day.** |

### Depth systems — M19–M23 → **Beta**

The systems that make the world worth staying in.

| Milestone | Delivers |
|---|---|
| **M19 — Farming & breeding** | Tilled soil, crops growing on the dormant-timer model, bonemeal, animal breeding. |
| **M20 — Structures** | Dungeons, mineshafts, strongholds, desert/jungle temples — generated, populated, loot-tabled. |
| **M21 — Redstone & mechanisms** | Signal propagation, wire, torches, repeaters, pistons, doors, pressure plates. |
| **M22 — Dimensions** | A dimension abstraction plus the second dimension, portals, and cross-dimension travel. |
| **M23 — Villages, trading, enchanting & brewing** | Villages, villagers, trading economy, XP, enchanting table, potion brewing. |
| **Beta** | **Gate: feature-complete singleplayer.** |

### The pillars — M24–M28 → **1.0**

What separates Cubara from a clone.

| Milestone | Delivers |
|---|---|
| **M24 — Multiplayer** | Dedicated server, network protocol, entity/chunk sync, client prediction, singleplayer running the server in-process. |
| **M25 — Shaders & visual quality** | Shader-pack format and loader, shadow mapping, water reflection/refraction, post-processing chain. |
| **M26 — Modding API** | Mod loading, scripting, data packs, a stable API surface, versioning policy. |
| **M27 — Audio** | Sound engine, spatial audio, block/step/mob sounds, music. |
| **M28 — UI, options & accessibility** | Menus, world creation, keybinds, video/audio options, accessibility settings. |
| **1.0** | **Gate: pillars met, both platforms, packaged, perf re-validated.** |

---

## 4. Ordering rules

The ladder is not arbitrary. Some ordering is load-bearing:

- **Lighting (M9) before mobs (M17)** — hostile spawning is a function of light
  level. Building spawning first means rewriting it.
- **ECS (M16) before mobs (M17) and multiplayer (M24)** — both need entities to
  already have a home.
- **Block registry (M6) before nearly everything** — tools, drops, lighting, and
  physics all key off block data. Hardcoding block behaviour now means unpicking
  it from five systems later.
- **The timer model (M7) before farming (M19) and smelting (M14)** — those are
  meant to be *consumers* of one mechanism, per §1.
- **Multiplayer (M24) shapes M13–M18** — even before the network exists, gameplay
  state changes go through a server-authoritative path so M24 is an integration,
  not a rewrite. Issues in those milestones call this out explicitly.
- **Persistence (M8) early** — a save format that has to absorb ten systems
  retroactively is a migration problem; absorbing them as they land is not.

---

## 5. Keeping this document honest

- A milestone's scope is fixed when it opens. New ideas become issues in a *later*
  milestone, not additions to the one in flight.
- If a gate is missed, the gate does not move — the scope does. Cut features,
  keep the bar.
- If reality contradicts this roadmap, this file gets updated in the same PR that
  proves it wrong. A stale roadmap is worse than none, and this project has
  already learned that a rule nobody enforces is a wish (see
  [`CONTRIBUTING.md`](CONTRIBUTING.md) on mechanical enforcement).
