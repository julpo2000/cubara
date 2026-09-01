//! The authoritative half: the world, the simulation, and everything that
//! decides what is true.
//!
//! # Why this is a separate type
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §8. Multiplayer here is an authoritative
//! server (§3), and *"privé en public"* is one architecture in two deployments
//! (§3.3): private play runs the server in-process, public runs it standalone.
//! That means the client cannot own the world and edit it directly — which is
//! exactly what `Game` did, with twenty-four fields spanning both sides of a
//! seam that did not exist.
//!
//! This is that seam, drawn where §8.1 says it goes. It is deliberately drawn
//! **before** there is any networking: at one player and no netcode the seam
//! moves by changing which struct owns a field, and after netcode exists it does
//! not (§8.5).
//!
//! # What is not here yet
//!
//! Messages (§8.3) and a client-side replica world (§8.2). This step establishes
//! *ownership*; the client still calls the server directly rather than sending
//! it an `Action`. That is the next step, and it is much cheaper once the fields
//! are already on the right side.

use cubara_sim::Sim;
use cubara_voxel::{BlockRegistry, ChunkCoord, ItemRegistry, RecipeBook, SmeltBook};
use std::sync::Arc;

use cubara_sim::REACH;
use cubara_voxel::{BlockId, DropRule, FixedVec3, Interact, ItemStack, ItemState};
use cubara_world::{ChunkState, SmeltCtx, TerrainBlocks, TimedProcess, World};

/// Everything the simulation is authoritative about.
///
/// The registries are here because the server decides what a block *means* —
/// what it drops, what tier it needs, how long it takes to break. A client
/// needs the same definitions to draw and to predict, and will be given them;
/// it does not get to disagree about them.
pub struct Server {
    /// The world being simulated. Behind an [`Arc`] so meshing jobs can carry
    /// the exact snapshot they were queued against; an edit publishes a new one.
    pub world: Arc<World>,
    pub sim: Sim,
    pub blocks_registry: Option<Arc<BlockRegistry>>,
    pub terrain: Option<TerrainBlocks>,
    pub items: Option<ItemRegistry>,
    pub recipes: Option<RecipeBook>,
    pub smelting: Option<SmeltBook>,
    /// The chunk the simulation radius was last updated around (§11). Which
    /// chunks tick is an authority question, so it lives here.
    pub sim_centre: Option<ChunkCoord>,
}

/// What a client asks the world to do (`docs/RESEARCH_MULTIPLAYER.md` §8.3).
///
/// **Deliberately not an input.** An [`InputFrame`](cubara_sim::InputFrame) is
/// *what the player did with the controls*; an `Action` is *what they are asking
/// the world to do*. The difference is the whole anti-cheat argument: the client
/// says "break", and the **server** raycasts to decide what was hit. A client
/// that could name the block would be a client that could mine across the map.
///
/// §3.4's rule -- a client "may never be believed" -- made structural rather
/// than checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Break whatever the player is looking at.
    Break,
    /// Place the held block against whatever the player is looking at.
    Place,
    /// Use whatever the player is looking at.
    Interact,
}

/// A screen the server says should open. The *screen* is client state (§8.1);
/// what is behind it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Bench,
    Furnace([i32; 3]),
}

/// What changed, for the client to react to.
///
/// This is what will cross a socket. It is a *result*, never a request: the
/// client cannot ask for an effect, only be told about one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// This chunk's geometry is stale and wants re-meshing.
    Dirty(ChunkCoord),
    /// Open this screen.
    Open(Screen),
    /// Whatever screen is open should close -- the block behind it is gone.
    CloseIfAt([i32; 3]),
}

impl Server {
    /// Apply one action and report what changed (§8.3).
    ///
    /// **The server raycasts here, not the client.** That is the point of the
    /// action being `Break` rather than `Break(block)`.
    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Break => match self.break_looked_at() {
                Some((cc, block)) => vec![Effect::Dirty(cc), Effect::CloseIfAt(block)],
                None => Vec::new(),
            },
            Action::Interact => self
                .interact()
                .map(|s| vec![Effect::Open(s)])
                .unwrap_or_default(),
            Action::Place => {
                // An interactive block under the crosshair takes precedence
                // over placing -- otherwise a bench is unusable the moment you
                // are holding anything, which is most of the time.
                if let Some(screen) = self.interact() {
                    return vec![Effect::Open(screen)];
                }
                self.place_held()
                    .map(|cc| vec![Effect::Dirty(cc)])
                    .unwrap_or_default()
            }
        }
    }

    /// Which ids the terrain is made of, or a treeless default before assets
    /// are set.
    ///
    /// Trees are solid, so physics and raycasting need this -- `is_solid_at`
    /// cannot answer from the density field alone any more. The fallback is a
    /// world with no trees rather than a panic: `Game::new()` runs before a
    /// window exists, and a headless test that never sets assets should still
    /// be able to walk around.
    pub fn terrain(&self) -> TerrainBlocks {
        self.terrain.unwrap_or(TerrainBlocks {
            oak: None,
            ores: cubara_world::OreSet::EMPTY,
            grass: cubara_voxel::BlockId::AIR,
            soil: cubara_voxel::BlockId::AIR,
            stone: cubara_voxel::BlockId::AIR,
        })
    }
    /// Break the block at `block`, applying §4's drop and durability rules.
    /// The shared tail of [`break_block`](Self::break_block) (instant, for
    /// tests and for anything that bypasses mining) and
    /// [`tick_mining`](Self::tick_mining) (timed, what the game actually does),
    /// so the two cannot drift apart on what a break *yields*.
    pub fn break_at(&mut self, block: [i32; 3]) -> ChunkCoord {
        let [x, y, z] = block;
        // Whatever state the block owned goes with it (§7) -- but its contents
        // now spill onto the floor rather than being destroyed (block 2.5,
        // §10.4). This is one of the five sites that used to lose items.
        if let Some(f) = Arc::make_mut(&mut self.world).remove_block_entity(block) {
            let contents: Vec<_> = [f.input, f.fuel, f.output].into_iter().flatten().collect();
            if let Some(items) = self.items.as_ref() {
                let spawned: Vec<_> = contents
                    .into_iter()
                    .filter_map(|(id, count)| items.new_stack(id, count).ok())
                    .collect();
                for stack in spawned {
                    self.sim
                        .entities
                        .spawn_item(stack, drop_centre(block), FixedVec3::ZERO);
                }
            }
        }

        // The drop is the optional part; the break is not. Assets are always
        // set in the real app, but making the whole action depend on them
        // would mean a missing registry shows up as clicks that silently do
        // nothing -- the least debuggable failure there is.
        //
        // Read the three as separate fields rather than through a helper: the
        // borrow checker tracks disjoint field borrows, so `items` can stay
        // borrowed while `self.sim.player.inventory` is mutated. A helper
        // returning them all borrows the whole of `self`.
        if let (Some(registry), Some(terrain), Some(items)) = (
            self.blocks_registry.as_deref(),
            self.terrain,
            self.items.as_ref(),
        ) {
            let broken = self.world.block_at(x, y, z, terrain);
            let held = self.sim.player.inventory.selected_stack();
            let held_tier = held.map(|s| items.tier(s.item())).unwrap_or(0);

            let drop = if held_tier < registry.requires_tier(broken) {
                log::debug!(
                    "{} needs tier {}, holding tier {held_tier}: breaks, yields nothing",
                    registry.name_of(broken).unwrap_or("?"),
                    registry.requires_tier(broken),
                );
                None
            } else {
                match registry.drops(broken) {
                    DropRule::Nothing => None,
                    DropRule::SameName => registry
                        .name_of(broken)
                        .and_then(|name| items.id_of(name))
                        .map(|item| (item, 1u8)),
                    DropRule::Item(d) => items.id_of(&d.item).map(|item| (item, d.count)),
                }
            };

            match drop.and_then(|(item, count)| items.new_stack(item, count).ok()) {
                Some(stack) => {
                    // Block 2.5: what does not fit falls on the floor rather
                    // than being destroyed (§10.4).
                    if let Some(rest) = self.sim.player.inventory.add(stack, items) {
                        self.sim
                            .entities
                            .spawn_item(rest, drop_centre(block), FixedVec3::ZERO);
                    }
                    // Only a break that yielded something wears the tool.
                    self.wear_held_tool();
                }
                None => log::debug!(
                    "{} yielded nothing",
                    registry.name_of(broken).unwrap_or("?")
                ),
            }
        }

        Arc::make_mut(&mut self.world).set_block(x, y, z, BlockId::AIR)
    }

    /// Spend one point of the held tool's durability, removing the stack when
    /// it reaches zero (`PHASE2_ARCHITECTURE.md` §4, decision C).
    ///
    /// A no-op for anything that is not a tool: only an item declaring
    /// `durability` carries [`ItemState::Durability`], so an empty hand or a
    /// stack of planks falls through untouched.
    ///
    /// The worn stack is rebuilt rather than mutated because `ItemStack`
    /// enforces its own invariant (a stack with state is a stack of one), and
    /// going through `ItemStack::new` is what keeps that enforcement in one
    /// place.
    fn wear_held_tool(&mut self) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        let inv = &mut self.sim.player.inventory;
        let slot = inv.selected_slot() as usize;
        let Some(stack) = inv.slot(slot) else {
            return;
        };
        let ItemState::Durability { remaining } = stack.state() else {
            return;
        };
        let left = remaining.saturating_sub(1);
        if left == 0 {
            inv.set_slot(slot, None);
            return;
        }
        let worn = ItemStack::new(
            stack.item(),
            stack.count(),
            ItemState::Durability { remaining: left },
            items.max_stack(stack.item()),
        )
        .ok();
        inv.set_slot(slot, worn);
    }

    /// If the targeted block is interactive, act on it and report `true`.
    ///
    /// Reads [`Interact`] off the block registry rather than comparing names.
    /// The name comparison this replaces carried a note saying block 2.4 was
    /// the point to generalise it, "with two real cases to design against" --
    /// the furnace is that second case.
    fn interact(&mut self) -> Option<Screen> {
        let (Some(registry), Some(terrain)) = (self.blocks_registry.as_deref(), self.terrain)
        else {
            return None;
        };
        let origin = self.sim.player.pos.to_f32();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        let [x, y, z] = hit.block;
        match registry.interact(self.world.block_at(x, y, z, terrain)) {
            Interact::None => None,
            Interact::Bench => {
                // Width lives on `Crafting` (world state), not on the screen: a
                // 3x3 grid holding items in its outer cells is a different world
                // from a 2x2 one, and the hash already covers it.
                self.sim.player.crafting.set_width(3);
                Some(Screen::Bench)
            }
            Interact::Furnace => {
                // A furnace placed before this block existed (or loaded from an
                // older save) has no entity yet; give it one on first use rather
                // than refusing to open.
                Arc::make_mut(&mut self.world).add_furnace([x, y, z]);
                Some(Screen::Furnace([x, y, z]))
            }
        }
    }

    /// One tick of every furnace in the world (`PHASE2_ARCHITECTURE.md` §7).
    ///
    /// Iterates positions in `BTreeMap` order, so which furnace ticks first is
    /// the positions' own order rather than a hash seed's -- Rule 1, and the
    /// same reason the hash iterates them that way.
    ///
    /// In this scope every furnace ticks every tick, because every loaded chunk
    /// is active. Block 2.6's dormant chunks and 2.7's catch-up are what change
    /// that, and [`Furnace::advance`] already takes an elapsed count so they can.
    pub fn tick_furnaces(&mut self) {
        let (Some(items), Some(smelting)) = (self.items.as_ref(), self.smelting.as_ref()) else {
            return;
        };

        // Bring the simulation radius up to date (§11): chunks the player has
        // left go dormant, chunks they have reached wake up.
        //
        // Only when the player has actually changed chunk. Standing still can
        // change no chunk's state, and this walks a (2r+1)²x3 box -- 243
        // lookups at radius 4 -- which is pure waste every tick the player is
        // not moving, which is most of them.
        let centre = ChunkCoord::from_world_pos(self.sim.player.pos.to_f32());
        let now = self.sim.tick;
        let woken = if self.sim_centre == Some(centre) {
            Vec::new()
        } else {
            self.sim_centre = Some(centre);
            Arc::make_mut(&mut self.world).update_simulation_radius(centre, SIM_RADIUS_CHUNKS, now)
        };
        let caught_up: std::collections::BTreeMap<ChunkCoord, u64> =
            woken.into_iter().map(|w| (w.coord, w.elapsed)).collect();

        let world = Arc::make_mut(&mut self.world);
        let positions = world.block_entity_positions();
        if positions.is_empty() {
            return;
        }

        // **One pass over the block entities**, not one pass per chunk. The
        // obvious shape -- for each active chunk, find the entities in it --
        // is O(chunks x entities) and allocates a vector of every block entity
        // in the world for each of the 243 chunks in range. This is O(entities).
        for pos in positions {
            let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
            if world.chunk_states().get(coord) != ChunkState::Active {
                continue;
            }
            // A chunk that woke this tick owes the ticks it slept through *plus*
            // this one -- the same total the two-pass version produced, which is
            // what keeps the dormancy gate test passing.
            let ticks = caught_up.get(&coord).copied().unwrap_or(0) + 1;
            advance_furnace(world, pos, ticks, items, smelting);
        }
    }

    fn break_looked_at(&mut self) -> Option<(ChunkCoord, [i32; 3])> {
        let origin = self.sim.player.pos.to_f32();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        Some((self.break_at(hit.block), hit.block))
    }
    /// Place the held hotbar item's block against the targeted face, consuming
    /// one of it.
    ///
    /// The same name mapping as [`break_block`](Self::break_block), backwards.
    /// An item with no matching block -- a stick, an ingot -- places nothing
    /// **and consumes nothing**: a click that does nothing must not quietly
    /// spend an item.
    fn place_held(&mut self) -> Option<ChunkCoord> {
        // An interactive block under the crosshair takes precedence over
        // placing. Otherwise a bench would be unusable the moment you are
        // holding anything -- which is most of the time.
        let registry = self.blocks_registry.as_deref()?;
        let items = self.items.as_ref()?;
        let held = self.sim.player.inventory.selected_stack()?;
        let block = registry.id_of(items.name_of(held.item())?)?;

        let origin = self.sim.player.pos.to_f32();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        let target = [
            hit.block[0] + hit.normal[0],
            hit.block[1] + hit.normal[1],
            hit.block[2] + hit.normal[2],
        ];

        // Only now that the placement is certain to happen.
        let slot = self.sim.player.inventory.selected_slot() as usize;
        self.sim.player.inventory.take_one(slot, items)?;

        // A block that owns state gets it the moment it is placed, rather than
        // on first use -- so a furnace someone never opens still ticks, and the
        // world hash covers it either way.
        let interactive = self
            .blocks_registry
            .as_deref()
            .map(|r| r.interact(block) == Interact::Furnace)
            .unwrap_or(false);
        let world = Arc::make_mut(&mut self.world);
        let cc = world.set_block(target[0], target[1], target[2], block);
        if interactive {
            world.add_furnace(target);
        }
        Some(cc)
    }
}

/// Advance one furnace by `ticks`, whether that is one ordinary tick or a
/// dormant chunk's whole backlog.
fn advance_furnace(
    world: &mut World,
    pos: [i32; 3],
    ticks: u64,
    items: &ItemRegistry,
    smelting: &SmeltBook,
) {
    let Some(f) = world.furnace_at_mut(pos) else {
        return;
    };
    // Resolved to plain numbers once, here: a furnace only ever asks about the
    // one item in its fuel slot and the one its recipe outputs, so nothing in
    // the catch-up needs a registry (§12.3).
    let recipe = f.input.and_then(|(id, _)| smelting.for_input(id));
    let ctx = SmeltCtx {
        recipe,
        fuel_burn: f.fuel.and_then(|(id, _)| items.burn_ticks(id)),
        output_max: recipe.map(|r| items.max_stack(r.output)).unwrap_or(64),
    };
    // Bounded catch-up (§12.1): one ordinary tick and a million-tick backlog go
    // through the same call, and cost the same.
    f.advance(ticks, &ctx);
}
/// How far from the player, in chunks, the simulation keeps running
/// (`PHASE2_ARCHITECTURE.md` §11.4).
///
/// **Deliberately unrelated to render distance.** Coupling them would let the
/// settings menu quietly change what the world simulates. Small, because
/// simulation is the expensive part and dormancy is what makes a big world
/// affordable; expected to grow once block 2.7 makes a dormant chunk nearly
/// free.
const SIM_RADIUS_CHUNKS: i32 = 4;
/// The middle of block `b`, where an item dropped by breaking it appears.
fn drop_centre(b: [i32; 3]) -> FixedVec3 {
    let half = cubara_voxel::Fixed::from_raw(cubara_voxel::fixed::ONE / 2);
    FixedVec3::from_blocks(b[0], b[1], b[2]) + FixedVec3::new(half, half, half)
}
