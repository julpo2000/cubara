# Phase 2 — the alpha ladder, in full

The design [`ROADMAP.md`](../ROADMAP.md)'s phase-2 blocks implement. Same role
[`PHASE1_ARCHITECTURE.md`](PHASE1_ARCHITECTURE.md) plays for phase 1: **not a
summary of the block list — the decisions the block list executes**, with the
reasons, so no issue has to re-open one.

## Scope of this document

Blocks **2.1 – 2.4**: items and inventory, crafting, trees and ores, and tools
through to a smelted iron ingot. That is the owner's stated first slice, and it
closes [`REQUIREMENTS.md`](../REQUIREMENTS.md) #5's alpha definition — *"a
playable survival world where you can chop trees and at least mine iron"*.

Blocks 2.5 – 2.9 (ECS, the chunk state machine, dormant catch-up, the save
extension, health and mobs) are **deliberately not designed here**. They are
designed when they are reached, against what 2.1 – 2.4 actually produced, rather
than guessed at now. Where a decision below has to anticipate one of them — §7
and §8 both do — it says so, and commits only to the shape, not the content.

## The four decisions this document exists to record

These are **gameplay decisions, and they were the owner's**
([`CLAUDE.md`](../CLAUDE.md): what the game contains is not the agent's to
invent). They are written here as settled, so the issues that follow can be
executable rather than exploratory.

| # | Decision | Chosen | Rejected, and why |
|---|---|---|---|
| A | Crafting model | **Shaped**: 2×2 always available, 3×3 at a bench | Shapeless (a bench stops being a milestone); always-3×3 (removes a progression beat for nothing) |
| B | Tool tiers | **Tier gates the drop.** Too low a tier yields *nothing* | Speed-only (the wood→stone→iron order becomes advisory, and you can walk straight to iron) |
| C | Durability | **Tools wear out and break** | No durability — see §1: retrofitting per-item state later touches inventory, crafting *and* the save format at once |
| D | The ladder | log → planks → sticks + bench → wood pick → stone → stone pick → iron ore → furnace → **iron ingot** | Skipping the wood tier; furnace-only with no tiers |

Two constraints come from `REQUIREMENTS.md` and were **not** open to choice:

- **#3, "modding must be easy"** — every number below (stack sizes, tree shape,
  smelt duration, recipe contents, durability) lives in a RON data file, not in
  code. Adding a recipe or a tree species must need no recompile, exactly as
  adding a block already needs none (#54).
- **#6, "mechanics may be inspired; assets and branding are original"** — names,
  textures and content are ours. Recipes are designed, not transcribed.

---

## §1 What an item is — the keystone decision

Phase 1's keystone was `BlockId` (block 1.2): everything after it needed block
identity, so it landed before anything else. Phase 2's equivalent is the item
stack, for the same reason plus one more — **decision C makes it impossible to
model an item as a pair of numbers.**

```rust
/// Item identity. Mirrors `BlockId`'s shape deliberately: names are identity,
/// numbers are per-world (§1.2). 0 is NONE.
pub struct ItemId(pub u16);

/// Per-item state. `None` for everything stackable; a tool carries its own wear.
pub enum ItemState {
    None,
    Durability { remaining: u16 },
}

pub struct ItemStack {
    pub item: ItemId,
    pub count: u8,
    pub state: ItemState,
}
```

**The invariant, enforced by the constructor and pinned by a test:** a stack
whose `state` is not `None` always has `count == 1`. Two tools with different
remaining wear are not interchangeable, so they cannot share a stack. Everything
stackable carries `ItemState::None` and merges normally.

**Why this shape now, rather than `(ItemId, u8)` with durability bolted on
later.** The roadmap already makes this argument for block identity and for the
save format: some decisions cannot be retrofitted without a migration, and
per-item state is one of them. Adding it later would change the inventory's
element type, every crafting input and output, the drop path, *and* the on-disk
chunk payload — four systems at once, after each has grown code that assumes a
stack is two numbers. Adding it now costs one enum.

The invariant is what keeps that cheap: because state implies `count == 1`, the
common path (stackables) is exactly as simple as the pair-of-numbers design would
have been, and only tools pay for the generality.

### §1.2 Names are identity, numbers are per-world

Unchanged from block 1.3's decision for blocks, and for the same reason: a save
that stores numeric ids breaks the moment a data file is added or reordered, and
mods make that certain rather than likely.

Items are `cubara:oak_log`, `cubara:iron_ingot` in RON. The numeric `ItemId` is
assigned per world and written into the save header's **item id table**, next to
the block id table that already exists (§8).

### §1.3 Which crate

`cubara-voxel`, alongside `BlockId` and `BlockRegistry`.

Items and blocks are entangled by nature — a drop maps a block to an item, a
placeable item maps back to a block — and both load from RON through the same
registry machinery. Splitting them across two crates would mean one depends on
the other, or a third holds the mapping; neither is better than keeping the pure
data layer in one place. `cubara-world` and `cubara-sim` already depend on it,
and `cubara-render` will need item identity for the hotbar.

**Recorded honestly:** `cubara-voxel` already holds more than voxels (the mesher,
`Vertex`, `Aabb`), and items make the name drift further. It should become
`cubara-core`. That is a mechanical rename touching every crate, so it is a
follow-up of its own rather than something smuggled into a feature PR.

---

## §2 Inventory

```rust
pub struct Inventory {
    slots: [Option<ItemStack>; SLOT_COUNT],  // 36: 9 hotbar + 27 main
    selected: u8,                            // hotbar index, 0..9
}
```

The hotbar is **slots 0..9** — the same array, not a second container. A slot is
a slot; which ones the UI draws along the bottom is a rendering concern, and
splitting the storage would mean two code paths for "put this item somewhere".

**Insertion order is deterministic and specified**, because inventory contents
are part of the world-state hash (Rule 1, and block 2.9's survival replay test
asserts on it):

1. Merge into the **lowest-indexed** existing stack of the same item that has
   room and carries `ItemState::None`.
2. Otherwise place in the **lowest-indexed** empty slot.
3. Otherwise the pickup fails and the item is not consumed.

"Lowest-indexed" is the whole specification. Any rule that depended on iteration
order, recency or hashing would make two identical playthroughs diverge.

`SLOT_COUNT`, hotbar width and each item's `max_stack` live in data, not as
literals scattered through the code.

---

## §3 Crafting

Shaped, per decision A. A recipe is a RON file:

```ron
(
    name: "cubara:wooden_pick",
    pattern: [
        "PPP",
        " S ",
        " S ",
    ],
    key: {
        "P": "cubara:plank",
        "S": "cubara:stick",
    },
    output: (item: "cubara:wooden_pick", count: 1),
)
```

**Matching is position-independent within the grid.** Before comparing, both the
recipe pattern and the player's grid are *trimmed* of empty leading and trailing
rows and columns. A 2×2 recipe therefore matches in any corner of a 3×3 bench,
and the player is never asked to guess the alignment. Mirroring is **not**
applied: a recipe that should also work mirrored declares both patterns, so an
asymmetric recipe can stay asymmetric on purpose.

**The 2×2 grid is part of the inventory; the 3×3 needs a bench.** Recipes declare
no size — a recipe simply fails to match if its trimmed pattern does not fit the
grid it is offered to. The bench therefore gates 3×3 recipes without any recipe
needing to say so, and nothing has to be duplicated per grid size.

### §3.1 How the player actually crafts

Two decisions the owner made when block 2.2's UI came into view, recorded here
so the issues that implement them are executable.

**The grid is the basis, and every recipe has a grid form.** Ingredients go into
a grid; the result appears in a slot beside it; clicking that slot takes the
result and consumes the ingredients. A recipe book — click what you want, it
gathers the ingredients — is allowed as a *later convenience*, but it may never
be the only way to make something.

That invariant is already enforced by the types rather than by discipline:
`RecipeBook` holds nothing but shaped patterns, so a recipe with no grid form
cannot be expressed. If a book is added later and someone wants a book-only
recipe, they will have to change the data model to get it, which is exactly the
friction that should exist.

**Moving items is click-to-pick-up, click-to-place.** Click a slot to lift its
stack onto the cursor, click another to put it down; right-click places one.
Not drag-and-drop, and not both.

The reason is testability, not taste. Click-to-place is a *state machine* — a
held stack plus a sequence of slot indices — so every rule about merging,
splitting and swapping is a unit test with no mouse involved. Dragging is a
gesture over mouse-motion events, which can only really be verified by hand.
Supporting both would be two input paths into the same state change, which is
the duplication Rule 5 exists to prevent.

**Rejected: shapeless recipes as a second kind.** One matcher is Rule 5. A
recipe that genuinely does not care about layout is expressible as a shaped one
today; if that becomes painful, adding a `shapeless: true` variant is a data
change, and the argument for it should be made with real recipes in hand.

---

## §4 Breaking blocks: drops and tiers

Per decision B, a block declares what it yields and what it takes:

```ron
(
    name: "cubara:iron_ore",
    solid: true,
    faces: All("iron_ore"),
    shapes: [Full],
    drops: Some((item: "cubara:raw_iron", count: 1)),
    requires_tier: 2,
)
```

Tiers are a plain ordinal: **0 hand, 1 wood, 2 stone, 3 iron**. A held tool below
`requires_tier` means the block still breaks but **yields nothing** — it is
consumed and no item appears.

"Breaks but yields nothing" is deliberate over "cannot be broken at all": a block
that refuses to break with no explanation reads as a bug to a new player, where a
block that breaks and drops nothing teaches the rule in one go.

A block with `drops: None` yields nothing regardless of tool — leaves, for now
(§5).

**Durability (decision C) decrements on a successful break**, by one, on the held
tool only. At zero the stack is removed from its slot. Breaking bare-handed costs
nothing. Whether a *failed*-tier break also costs durability: **no** — you are
not punished twice for the same mistake.

---

## §4.1 The edit overlay had to learn what a block is

**A gap in this document, found while implementing 2.1c (#141) and recorded
here rather than worked around.** §2 designs the inventory and §4 designs drops,
and neither noticed that the thing they both stand on was a boolean:

```rust
edits: BTreeMap<[i32; 3], bool>,          // before
pub fn set_block(&mut self, x: i32, y: i32, z: i32, solid: bool) -> ChunkCoord
```

`true` meant "something solid", resolved to grass/soil/stone by depth. That was
right for phase 1, where breaking and placing were a debug affordance and no
system asked *which* block. It can express neither "you broke an oak log, take an
oak log" nor "place the stone you are holding" — the whole of block 2.1.

**The overlay carries a `BlockId`.** `BlockId::AIR` is a break; anything else is
a placement of that specific block. There is no `bool` overload beside it — one
way to edit a block, per Rule 5.

Two consequences worth stating, because both were invisible before:

- **Solidity is derived, not stored.** `is_solid_at` asks whether the recorded
  block is `AIR`, rather than keeping its own flag. One source of truth, so an
  edit cannot read solid in one method and air in another.
- **`load_chunk_edits` became lossless.** It used to flatten a loaded voxel to
  `loaded != AIR` on the way in, which was fine only because an edit could not be
  anything but stone-or-air. It now writes the block straight through.

**The trap this walked into, recorded so the next person does not.** Translating
the old `set_block(.., true)` calls looked mechanical, and it was not: ids are
assigned by sorted name, so `BlockId::STONE` (the constant, id 1) is *grass* in a
three-material registry. A blind translation silently changed which material two
fixtures placed. Call sites now resolve from their own registry
(`blocks.stone`), never from the constant. `World::chunk_at`'s doc comment
already told this story for block 1.4b; it is the same trap one layer up.

### §4.2 Every block needs an item of the same name

A consequence of the placeholder drop policy that is easy to miss and was:
because a drop is resolved by *name*, a block with no matching
`assets/items/<name>.ron` **silently drops nothing**. Not a crash, not a
warning — just an empty inventory and a confused player.

It bit immediately. Block 2.1a shipped nine item files for the ladder to iron
and none for `cubara:grass`, `cubara:soil` or `cubara:stone` — which is every
block the world is currently made of. Breaking anything yielded nothing.

`every_shipped_block_has_an_item_of_the_same_name` now keeps the two asset
directories in step, and names the offenders when they drift. Block 2.4 replaces
the policy with real `drops:` tables; when it does, that test's failure message
should point at the table instead.

### §4.3 Mining takes time — hardness per block, speed per tool

**Decided by the project owner, 2026-08-31**, resolving the §9 question "is
breaking a block instant, or is there a mining *time* per block?". It is
recorded here rather than in an issue because it changes what a tool *is*: not
just a key that unlocks drops, but the thing that makes mining faster.

The rule, and it is one rule:

```
ticks_to_break = ceil(hardness / speed)
```

- **`hardness`** is a property of the block, in `assets/blocks/*.ron`. Air and
  anything unbreakable is absent, not zero.
- **`speed`** is a property of the held tool, in `assets/items/*.ron`. The empty
  hand has speed 1 and is not a special case — it is the floor of the same
  scale, which is what keeps "mine dirt by hand" and "mine stone with an iron
  pick" the same code path.
- Both are integers, and the division is integer division. Mining progress is
  **tick-counted, never wall-clock**: Rule 1, and the same reason the tick loop
  exists at all. A player mining the same block from the same tick with the same
  tool breaks it on the same tick on every machine.

**Progress is per-position and is abandoned, not banked.** Look away, switch
tools, or break the block's neighbour and the counter resets. Storing partial
progress per position would be a block-entity-sized problem (§7) for something
the player cannot see, and "come back and it is half-mined" is a mechanic nobody
asked for.

**Tier still gates drops, exactly as §4 says, and the two are independent.** A
tier-too-low tool still breaks the block — it just yields nothing. Hardness
decides *how long*; tier decides *whether you get anything*. Keeping them
separate is what lets a wooden pick chew slowly through stone and get cobble,
while never getting iron out of iron ore no matter how long it takes.

## §5 Trees

Placement is already decided and is not re-opened here:
[`PHASE1_ARCHITECTURE.md` §8.4](PHASE1_ARCHITECTURE.md) — **a structure pass with
a declared maximum radius**, pure in `(seed, coord)`, no deferred writes into
neighbours, level 0 only. Oak declares a radius of 1 chunk, which its canopy fits
inside.

What is new here is the shape, and it is data:

```ron
(
    name: "cubara:oak",
    trunk: (block: "cubara:oak_log", height: (4, 6)),
    canopy: (block: "cubara:oak_leaves", radius: 2, shape: Blob),
    places_on: ["cubara:grass"],
    density: 0.02,
)
```

`height: (4, 6)` is an inclusive range resolved from `hash(seed, x, z)` — the
same hash that decided the placement, so a tree's size is as reproducible as its
position.

**Leaves do not decay, and there are no saplings, in this scope.** Both are real
gameplay decisions and neither is needed for the ladder to iron. They are listed
in §9 as the owner's to make, rather than quietly assumed either way.

---

## §6 Ores

Iron ore is **not** a structure — it needs no cross-chunk reach, so it should not
pay the structure pass's cost. It is a threshold in the existing density pass
(block 1.5's seeded 3D noise): where stone would be generated, a second noise
channel above its threshold and below a declared `max_y` becomes ore instead.

That keeps `generate(seed, coord)` pure and unchanged in shape, and ore
distribution inherits the cross-platform bit-identical guarantee block 1.5
already tests for.

---

## §7 The furnace — the first block that owns state over time

A furnace has an input slot, a fuel slot, an output slot and a progress counter.
That is state attached to *one block position*, which nothing in phase 1 has.

**What burns: wood. Decided by the project owner, 2026-08-31.**

Logs and planks are fuel; there is **no coal**, and no coal ore is generated.
The reasoning is that it closes the ladder with content that already exists —
you chop trees in block 2.3a, so the furnace works the moment you can build one,
and no new ore, item or texture is needed to smelt the iron `REQUIREMENTS.md` #5
asks for.

Fuel is declared in data, per `REQUIREMENTS.md` #3, as a burn duration in ticks
on the item:

```ron
ItemDef(
    name:       "cubara:oak_log",
    max_stack:  64,
    durability: None,
    burn_ticks: Some(80),
)
```

An item with no `burn_ticks` is not fuel. That is a property of the *item*
rather than a separate fuel table, so "can this go in the fuel slot" is one
lookup and cannot disagree with itself.

**If coal is ever added**, it is a data file and not a rewrite: block 2.3b's
`OreSet` holds four ores precisely so a second one costs a RON file, and
`burn_ticks` on a coal item is the whole of the rest. This decision is
deliberately *not* closing that door, only declining to open it now.

**Block entities: a per-chunk side table.**

```rust
pub struct BlockEntities {
    entries: BTreeMap<LocalPos, BlockEntity>,   // sparse, ordered
}
```

Sparse, because almost no block has one. `BTreeMap`, because iteration order
feeds the world-state hash — the same reasoning that made the arena's draw list a
`BTreeMap` in issue #81.

It lives on the chunk, so it loads, saves and unloads with the chunk it belongs
to and needs no separate lifetime rules.

**Ticking, and the seam to block 2.7.** In this scope a furnace ticks only while
its chunk is active: one tick of progress per sim tick, deterministic, no
wall-clock. Block 2.7 (dormant catch-up — the Factorio timers of
`REQUIREMENTS.md` #4) is what makes it resumable across dormancy.

This design does **not** attempt that now. It does commit to the one property
that makes it possible later: **furnace progress is a pure function of elapsed
ticks and its slot contents.** Nothing about it may depend on having been ticked
one tick at a time. That is exactly what a catch-up framework needs, it is cheap
to hold now, and it is expensive to recover once a system has assumed otherwise.

---

## §8 What this does to the save format

Block 2.8 extends the format properly. This scope needs two things from it, and
the shape is settled here so that 2.1 – 2.4 do not each invent their own:

1. **An item id table in the world header**, in the same form as the block id
   table block 1.9 already writes — name → per-world numeric id.
2. **A block-entity section in the chunk payload**, sparse: a count followed by
   `(local position, entity)` pairs.

Both are a **format version bump**, not a new format. The existing round-trip
test (`the_committed_fixture_loads_to_a_known_hash`) keeps its fixture; a second
fixture covers a world with an inventory and a running furnace.

---

## §9 Not in this scope — and which are the owner's call

Engineering-deferred, mine to sequence: ECS (2.5), the chunk state machine (2.6),
dormant catch-up (2.7), health/hunger/mobs (2.9).

**Gameplay decisions, still open, deliberately not assumed:**

- Do leaves decay when their trunk is removed?
- Do saplings drop, and can trees be replanted?
- Does a broken tool leave anything behind?
- Is crafting instant, or is there a result preview to confirm?

**Answered since, and moved into the sections that implement them** (kept here
so the trail from question to decision is readable):

- *Is breaking a block instant, or is there a mining time?* → **timed**, §4.3.
- *What does the furnace burn?* → **wood; no coal**, §7.

None of these block the ladder to iron. Each is listed so that it gets asked
rather than invented — a plausible-sounding invention is worse than a question,
because it looks decided.
