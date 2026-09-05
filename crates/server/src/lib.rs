//! The authoritative half: the world, the simulation, and everything that
//! decides what is true.
//!
//! # Why this is its own crate
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §3.3: multiplayer here is one architecture in
//! two deployments — private play runs the server **in-process**, public runs it
//! **standalone**. A crate is what makes the second one real. While `Server`
//! lived in `crates/app` it was correct by *content* (it imports no `wgpu`) and
//! wrong by *construction*: anything that linked it also linked a windowing
//! library and a graphics API, so a headless host had to install a GPU stack for
//! a process that will never draw a pixel.
//!
//! This is `ARCHITECTURE.md` Rule 4 collecting its debt. "The simulation runs
//! with no GPU" was written so a dedicated server would be possible without a
//! rewrite; the rewrite it saved is this file staying exactly as it was and only
//! its `Cargo.toml` changing.
//!
//! # What is not here yet
//!
//! **Networking.** No sockets, no protocol, no players connecting. `Action` and
//! `Effect` (§8.3) are the messages that will cross a socket, and
//! [`headless::run`] is the loop that will service one — but today the only
//! client is in the same process. `cubara-server` runs a world; it does not yet
//! run a *shared* one. Saying otherwise would be the kind of plausible-sounding
//! invention that looks decided.
//!
//! Also missing: the client-side replica world (§8.2).

pub mod assets;
pub mod clock;
pub mod headless;
pub mod net;
pub mod predict;
pub mod view;
pub mod wire;

use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs, PlayerState, Sim};
use cubara_voxel::{Angle, BlockRegistry, ChunkCoord, ItemRegistry, RecipeBook, SmeltBook};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::view::ClientView;

use cubara_sim::REACH;
use cubara_voxel::{BlockId, DropRule, FixedVec3, Interact, ItemStack, ItemState};
use cubara_world::{ChunkState, Furnace, SmeltCtx, TerrainBlocks, TimedProcess, World};

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
    /// Which player this server's *local* client drives (block 2.10).
    ///
    /// Named rather than assumed. A dedicated server has many players and no
    /// local one, and this field is the seam where that becomes true: today it
    /// is always [`PlayerId::LOCAL`] because there is one client and it is in
    /// this process, and every site that means "the player at this keyboard"
    /// says so instead of writing `0`.
    pub local: PlayerId,
    pub blocks_registry: Option<Arc<BlockRegistry>>,
    pub terrain: Option<TerrainBlocks>,
    pub items: Option<ItemRegistry>,
    pub recipes: Option<RecipeBook>,
    pub smelting: Option<SmeltBook>,
    /// The chunk the simulation radius was last updated around (§11). Which
    /// chunks tick is an authority question, so it lives here.
    pub sim_centre: Option<ChunkCoord>,
    /// What each client can perceive, and what it is owed (block 2.11).
    ///
    /// This replaced a single `journal: Vec<Effect>`. One queue was right while
    /// there was one client; with many it is the O(players²) shape
    /// `docs/RESEARCH_MULTIPLAYER.md` §3.2 names as the thing that decides
    /// whether the player-count target is reachable at all.
    ///
    /// Private, and drained rather than read: a client that could *look*
    /// without taking could apply the same change twice, or miss one. Over a
    /// socket each of these is a send queue.
    views: BTreeMap<PlayerId, ClientView>,
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
    /// Click a slot on the open furnace's screen.
    ///
    /// The one action that names its target, and for a reason the raycast rule
    /// does not cover: the player is not *looking* at the slot, they are looking
    /// at a screen. The position is still checked against the world — a furnace
    /// that is not there cannot be clicked — so this is a lookup the server
    /// validates, not a claim it believes.
    ClickFurnace { pos: [i32; 3], slot: FurnaceSlot },
}

/// Which slot of a furnace a click landed on.
///
/// The server's own vocabulary, deliberately not `cubara_render`'s
/// `PanelSlotKind`: where a slot is *drawn* is presentation, and a server that
/// spoke in panel layouts would be a server that knew what a screen looks like
/// (Rule 3). The client translates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FurnaceSlot {
    /// What is waiting to be smelted.
    Input,
    /// What is waiting to be burned.
    Fuel,
    /// What has been smelted. Take-only.
    Output,
}

/// A screen the server says should open. The *screen* is client state (§8.1);
/// what is behind it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Bench,
    Furnace([i32; 3]),
}

/// What changed, for the client to apply to its replica (§8.3).
///
/// This is what will cross a socket. It is a *result*, never a request: the
/// client cannot ask for an effect, only be told about one.
///
/// **Note what is not here: a chunk id.** The old `Dirty(chunk)` told the client
/// which chunk to re-mesh, which only works when both sides share one `World` —
/// the client had nothing of its own to make dirty. Now the server reports the
/// *edit*, the client applies it to its own world, and the dirty chunk is what
/// its own `set_block` hands back. A remote client would derive it the same way,
/// because there is no other way for it to derive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// A block changed. The client applies it to its replica.
    Edit { pos: [i32; 3], block: BlockId },
    /// A block entity appeared, changed, or (`None`) was removed.
    ///
    /// The whole value, not a delta. A furnace is three item slots and two
    /// counters; sending what it *is* costs less than describing what happened
    /// to it, and cannot desynchronise the way a missed delta can.
    BlockEntity {
        pos: [i32; 3],
        furnace: Option<Furnace>,
    },
    /// Open this screen.
    Open(Screen),
    /// Whatever screen is open should close -- the block behind it is gone.
    CloseIfAt([i32; 3]),
    /// Where another player is now (block 2.11).
    ///
    /// **Never the receiving client's own player.** Section 3.4 splits those:
    /// a client predicts itself and is corrected, and interpolates everyone
    /// else, because it does not know their inputs. Sending someone their own
    /// position back every tick would be both useless and, once there is
    /// latency, actively wrong -- it would fight the prediction.
    ///
    /// Fixed-point and binary angles, like everything else that crosses this
    /// seam: nothing here is a float (section 3.5).
    PlayerMoved {
        who: PlayerId,
        pos: FixedVec3,
        yaw: Angle,
        pitch: Angle,
    },
    /// A player left the world, or walked out of sight. The client stops
    /// drawing them.
    PlayerGone(PlayerId),
    /// **The server's correction to the client this is addressed to** (block
    /// 2.12b).
    ///
    /// Deliberately not `PlayerMoved` with the owner's own id. Those are
    /// different messages because they exist for different reasons: another
    /// player's pose is *interpolated*, because their input is unknown; your own
    /// is *reconciled*, because your input is known and has already been acted
    /// on locally. Collapsing them would delete the distinction reconciliation
    /// is built on.
    ///
    /// `seq` is **which of this client's inputs the server had applied** when it
    /// produced `state` -- not a tick number. The client's clock and the
    /// server's are different clocks, and mixing them is how a reconnect rewinds
    /// the world.
    ///
    /// Block 2.12b sends `seq` and never reads it: a client that does not
    /// predict has nothing to reconcile. It is here anyway because changing a
    /// *wire format* is the expensive part, not carrying a field -- landing
    /// without it would make block 2.13's first commit a breaking change to a
    /// message a day old. Do not remove it as dead code.
    SelfState { seq: u64, state: PlayerState },
}

impl Server {
    /// An empty world with no assets — what `Game::new()` builds before a
    /// window exists, and what a test that does not care about content wants.
    ///
    /// The player starts at y = 48 because terrain does not exist yet at this
    /// point; [`set_assets`](Self::set_assets) is what stands them on the
    /// ground once it does.
    pub fn new() -> Self {
        let mut server = Self {
            world: Arc::new(World::new()),
            sim: Sim::new(
                0,
                Player::new(
                    FixedVec3::from_blocks(0, 48, 0),
                    Angle::from_radians(0.6),
                    Angle::from_radians(-0.3),
                ),
            ),
            blocks_registry: None,
            terrain: None,
            items: None,
            recipes: None,
            smelting: None,
            sim_centre: None,
            views: BTreeMap::new(),
            local: PlayerId::LOCAL,
        };
        // The local client is watching from the start. A `Server` with no view
        // would queue every effect into nothing, which is not a smaller server
        // -- it is one whose edits never reach the screen.
        server.open_view(PlayerId::LOCAL);
        server
    }

    /// A server with every definition loaded and the save at `dir` restored, if
    /// there is one.
    ///
    /// This is the whole of "start a world" — what the dedicated server does at
    /// boot and what the window does once its registry exists. Returns whether
    /// a save was loaded; `false` is the normal first run, not an error.
    pub fn open(&mut self, dir: &std::path::Path) -> bool {
        let items = assets::load_item_registry();
        let recipes = assets::load_recipe_book(&items);
        self.set_assets(Arc::new(assets::load_block_registry()), items, recipes);
        let loaded = self.load_from(dir);
        // The local client is watching from the moment the world opens. Loading
        // replaces the `Sim` -- and with it every player -- so this comes after,
        // or the view would be centred on a player who no longer exists.
        for who in self.sim.player_ids() {
            self.open_view(who);
        }
        loaded
    }

    /// Give the server the assets it needs to turn blocks into items and back,
    /// and stand the player on the ground.
    pub fn set_assets(
        &mut self,
        registry: Arc<BlockRegistry>,
        items: ItemRegistry,
        recipes: RecipeBook,
    ) {
        self.terrain = Some(
            TerrainBlocks::from_registry(&registry)
                .with_oak(&assets::load_structure_registry(), &registry)
                .with_ores(&assets::load_ore_registry(), &registry),
        );
        self.blocks_registry = Some(registry);
        self.smelting = Some(assets::load_smelt_book(&items));
        // Terrain is known for the first time here, so this is where the player
        // can be put somewhere that exists.
        self.place_player_on_ground();
        self.items = Some(items);
        self.recipes = Some(recipes);
    }

    /// Stand the player on the terrain under their column, and make that their
    /// spawn point.
    ///
    /// **Without this the game is unplayable.** [`Server::new`] places the
    /// player at y = 48 because terrain does not exist yet at that point -- but
    /// the surface under that column is at y = 15, a 32-block drop. Once block
    /// 2.9a made falling hurt, that is 29 damage against 20 health: the player
    /// dies on the first landing, respawns at the same point in mid-air, and
    /// dies again, forever.
    ///
    /// Every test missed it because they all reposition the player just above
    /// the ground before doing anything. `the_game_does_not_start_by_killing_the_player`
    /// is the one that starts the way the app does.
    ///
    /// Two blocks above the surface, not exactly on it: the eye is 1.62 above
    /// the feet, so this leaves a fraction of a block to settle -- well inside
    /// the 3-block safe fall, and it avoids having to reach for the private
    /// eye-height constant from another crate.
    pub fn place_player_on_ground(&mut self) {
        let Some(terrain) = self.terrain else {
            return;
        };
        let p = self.sim.player_mut(self.local).pos;
        let [px, _, pz] = p.to_f32();
        let Some(hit) = self
            .world
            .raycast([px, 200.0, pz], [0.0, -1.0, 0.0], 400.0, terrain)
        else {
            return;
        };
        let standing = FixedVec3::new(p.x, cubara_voxel::Fixed::from_blocks(hit.block[1] + 2), p.z);
        self.sim.player_mut(self.local).pos = standing;
        self.sim.player_mut(self.local).velocity = FixedVec3::ZERO;
        self.sim.player_mut(self.local).fall_distance = cubara_voxel::Fixed::ZERO;
        // Death returns here, not to wherever `Server::new` happened to start.
        self.sim.player_mut(self.local).spawn = standing;
    }

    /// Advance the player's simulation by one tick.
    ///
    /// Split from [`tick_world`](Self::tick_world) so the client can keep doing
    /// its per-tick mining between the two, which is where it has always
    /// happened. Merging them would reorder the tick, and tick order is Rule 1
    /// — the pinned world hashes would move, and a moved hash with no reason is
    /// indistinguishable from a determinism bug.
    pub fn tick_sim(&mut self, input: &InputFrame) {
        self.tick_sim_all(&PlayerInputs::one(self.local, *input));
    }

    /// The same tick, with an input per player -- what a server with more than
    /// one client will call. [`tick_sim`](Self::tick_sim) is the one-client case
    /// expressed in terms of it, rather than a second path (Rule 5).
    pub fn tick_sim_all(&mut self, inputs: &PlayerInputs) {
        let terrain = self.terrain();
        self.sim
            .tick(Arc::make_mut(&mut self.world), inputs, terrain);
    }

    /// Advance everything that ticks whether or not a player is doing anything:
    /// furnaces, and dropped items falling and ageing out.
    ///
    /// This is the half a dedicated server runs with no one connected. That it
    /// *is* a half — that the world does not stop when the player does — is the
    /// thing a headless server tests for free.
    pub fn tick_world(&mut self) {
        // Before the furnaces, so a client that walked into a chunk this tick is
        // owed that chunk's contents *and then* whatever changed in it -- rather
        // than a change to a block it has not been told exists.
        self.refresh_views();
        self.publish_player_states();
        self.tick_furnaces();
        // Dropped items fall, age out, and get picked up -- on the same fixed
        // clock as everything else (§10.4, Rule 1).
        if let Some(items) = self.items.as_ref() {
            let terrain = self.terrain();
            self.sim.tick_entities(&self.world, terrain, items);
        }
    }

    /// The world's state reduced to one number (`cubara_sim::WorldHash`).
    ///
    /// **Over the simulation radius, not the whole world.** An infinite world
    /// has no "whole" to hash, and the simulation radius is exactly the part
    /// two machines running the same world have to agree about
    /// (`RESEARCH_MULTIPLAYER.md` §3.2) — a chunk nobody is simulating cannot
    /// have diverged, because nothing has happened in it.
    ///
    /// The region is derived from the player's position rather than read off
    /// the chunk lifecycle: `chunk_states().active()` is empty on a world that
    /// has been loaded but not yet ticked, so hashing that would make a
    /// freshly-restored world hash differently from the one that was saved —
    /// which is the one comparison this most needs to get right.
    pub fn hash(&self) -> cubara_sim::WorldHash {
        self.hash_with_workers(1)
    }

    /// [`hash`](Self::hash), computed across `workers` threads.
    ///
    /// The result must not depend on `workers` — that is Rule 1, and it is what
    /// the phase 2 gate's survival replay checks by running the same finished
    /// world through this at one thread and at several. `hash` fixes the count
    /// at 1 because a server hashing itself wants the answer, not the
    /// parallelism; a test that wants to prove the two agree needs to be able
    /// to ask for both.
    pub fn hash_with_workers(&self, workers: usize) -> cubara_sim::WorldHash {
        cubara_sim::WorldHash::compute(
            &self.sim,
            &self.world,
            &self.hash_region(),
            self.terrain(),
            workers,
        )
    }

    /// The chunks [`hash`](Self::hash) covers, in ascending order.
    ///
    /// Separate so a test can check it against what the world actually
    /// simulates -- see `the_hash_covers_every_chunk_that_is_simulating`.
    pub fn hash_region(&self) -> Vec<ChunkCoord> {
        let centre = ChunkCoord::from_world_pos(self.sim.player(self.local).pos.to_f32());
        let mut region = Vec::new();
        for x in (centre.x - SIM_RADIUS_CHUNKS)..=(centre.x + SIM_RADIUS_CHUNKS) {
            for y in (centre.y - SIM_HASH_VERTICAL_CHUNKS)..=(centre.y + SIM_HASH_VERTICAL_CHUNKS) {
                for z in (centre.z - SIM_RADIUS_CHUNKS)..=(centre.z + SIM_RADIUS_CHUNKS) {
                    region.push(ChunkCoord::new(x, y, z));
                }
            }
        }
        // Ascending coordinate order, which is what `compute` folds in.
        region.sort();
        region
    }

    /// Write the world to disk (#179).
    ///
    /// Best-effort and non-fatal: a failed save is logged, not a crash. Losing
    /// a session is bad; losing it *and* taking the process down with it is
    /// worse, and it may be shutting down precisely because something is
    /// already wrong.
    pub fn save_to(&self, dir: &std::path::Path) {
        let (Some(registry), Some(items), Some(blocks)) = (
            self.blocks_registry.as_deref(),
            self.items.as_ref(),
            self.terrain,
        ) else {
            return;
        };
        match cubara_sim::save_world(dir, &self.sim, &self.world, registry, items, blocks) {
            Ok(()) => log::info!("world saved to {}", dir.display()),
            Err(e) => log::error!("could not save the world: {e}"),
        }
    }

    /// Replace this server's world with the one on disk, if there is one (#179).
    ///
    /// Returns whether anything was loaded. A missing save is the normal first
    /// run, not an error. A save that exists but *fails* to load is logged and
    /// ignored rather than fatal -- most often it is a version mismatch after
    /// the generator changed, and refusing to start over it would be worse than
    /// starting a fresh world.
    ///
    /// **Called after `set_assets`**, which stands the player on the ground:
    /// this overwrites that position with the saved one, so a player who quit
    /// in a mineshaft comes back to the mineshaft.
    pub fn load_from(&mut self, dir: &std::path::Path) -> bool {
        let (Some(registry), Some(items), Some(blocks)) = (
            self.blocks_registry.as_deref(),
            self.items.as_ref(),
            self.terrain,
        ) else {
            return false;
        };
        if !dir.join("level.ron").exists() {
            return false;
        }
        match cubara_sim::load_world(dir, registry, items, blocks) {
            Ok((sim, world)) => {
                self.sim = sim;
                self.world = Arc::new(world);
                // The simulation radius is recomputed from scratch: the saved
                // world has no chunk lifecycle (§11), by design.
                self.sim_centre = None;
                log::info!("world loaded from {}", dir.display());
                true
            }
            Err(e) => {
                log::error!(
                    "could not load {}: {e} -- starting a fresh world",
                    dir.display()
                );
                false
            }
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    /// Take everything the local client has not been told yet.
    ///
    /// Drained, not borrowed: an effect applied twice is a desynchronised
    /// replica, and taking the queue is what makes that impossible rather than
    /// merely discouraged.
    pub fn drain_effects(&mut self) -> Vec<Effect> {
        self.drain_effects_for(self.local)
    }

    /// The same, for a named client (block 2.11).
    pub fn drain_effects_for(&mut self, who: PlayerId) -> Vec<Effect> {
        self.views
            .get_mut(&who)
            .map(ClientView::drain)
            .unwrap_or_default()
    }

    /// A client's view, if it has one.
    pub fn view(&self, who: PlayerId) -> Option<&ClientView> {
        self.views.get(&who)
    }

    /// Give `who` a view, and owe them everything already in sight.
    ///
    /// Separate from `Sim::join` because a *player* existing and a *client*
    /// watching are different things: a dedicated server may hold a player
    /// whose connection dropped, and it should keep simulating them without
    /// queueing effects nobody will ever collect.
    pub fn open_view(&mut self, who: PlayerId) {
        self.views.entry(who).or_default();
        self.refresh_view(who);
    }

    /// Drop a client's view. Their player may well still exist.
    pub fn close_view(&mut self, who: PlayerId) {
        self.views.remove(&who);
    }

    /// Send `who` the server's view of their own player (block 2.12b).
    ///
    /// `seq` is the last input from that client the server had applied when this
    /// state was produced. The caller owns the counting, because it owns the
    /// connection the inputs arrived on -- a `Server` has no idea how many
    /// messages a link has carried, and should not.
    pub fn publish_self_state(&mut self, who: PlayerId, seq: u64) {
        let Some(state) = self.sim.get(who).map(|p| p.state()) else {
            return;
        };
        self.publish_to(who, Effect::SelfState { seq, state });
    }

    /// Tell everyone still watching that `who` has gone.
    ///
    /// Unfiltered by interest, deliberately, and it is the one place that is
    /// right: a client that was drawing someone needs to stop, and by the time
    /// this is called the departed player's position is no longer available to
    /// decide who could see them. Sending a departure to someone who was not
    /// watching costs nine bytes and no confusion; *not* sending it to someone
    /// who was leaves a person standing in their world forever.
    pub fn announce_departure(&mut self, who: PlayerId) {
        for view in self.views.values_mut() {
            view.push(Effect::PlayerGone(who));
        }
    }

    /// Bring one client's interest set up to date with where its player is, and
    /// backfill any chunk that has just come into view.
    ///
    /// A no-op when the player has not changed chunk, which is most ticks.
    fn refresh_view(&mut self, who: PlayerId) {
        let Some(player) = self.sim.get(who) else {
            return;
        };
        let centre = ChunkCoord::from_world_pos(player.pos.to_f32());
        let Some(view) = self.views.get_mut(&who) else {
            return;
        };
        let Some(previous) = view.recentre(centre) else {
            return;
        };
        let view = view.clone();

        // **Walk the edits, not the chunks.** At the replication radius the view
        // holds tens of thousands of chunks and the world holds a handful of
        // edits; iterating the sparse side is the difference between a scan that
        // costs nothing and one that costs more than the tick.
        //
        // Edits before block entities, so a client applies the block before
        // whatever state hangs off it -- the same order `snapshot` uses.
        let mut owed: Vec<Effect> = Vec::new();
        for (pos, block) in self.world.edits() {
            if view.newly_visible(previous, pos) {
                owed.push(Effect::Edit { pos, block });
            }
        }
        for (pos, f) in self.world.block_entities() {
            if view.newly_visible(previous, *pos) {
                owed.push(Effect::BlockEntity {
                    pos: *pos,
                    furnace: Some(*f),
                });
            }
        }

        if let Some(view) = self.views.get_mut(&who) {
            for e in owed {
                view.push(e);
            }
        }
    }

    /// Bring every client's interest set up to date. Once per tick.
    pub fn refresh_views(&mut self) {
        let watchers: Vec<PlayerId> = self.views.keys().copied().collect();
        for who in watchers {
            self.refresh_view(who);
        }
    }

    /// Queue an effect for every client that can perceive `pos`.
    ///
    /// This is the whole of interest management on the outbound side: an effect
    /// nobody is near is **dropped**, not stored. That is what stops bytes
    /// growing with the number of players -- the alternative, keeping it in case
    /// someone walks past later, is how a per-client queue becomes a second copy
    /// of the world.
    fn publish_at(&mut self, pos: [i32; 3], effect: Effect) {
        for view in self.views.values_mut() {
            if view.perceives(pos) {
                view.push(effect);
            }
        }
    }

    /// Queue an effect for one named client -- for effects that are about a
    /// *client* rather than about a place. Opening a screen is the only one.
    fn publish_to(&mut self, who: PlayerId, effect: Effect) {
        if let Some(view) = self.views.get_mut(&who) {
            view.push(effect);
        }
    }

    /// Tell each client where the *other* players near them are (block 2.11).
    ///
    /// This is the traffic interest management exists to bound, and the reason
    /// the scaling criterion is not vacuous: without it there is nothing in the
    /// per-client queue that could grow with the player count, and a test
    /// claiming the shape is flat would be measuring an empty room.
    ///
    /// Cost is O(watchers x players) when everyone is standing on the same
    /// spot, and that is correct -- a thousand people in one square genuinely do
    /// all have to see each other. The claim interest management makes is about
    /// the *spread-out* case, which is the one a world of five thousand is in.
    ///
    /// Iterated in `PlayerId` order on both sides, so what a client is told, and
    /// in what order, does not depend on a hash seed (Rule 1).
    fn publish_player_states(&mut self) {
        let others: Vec<(PlayerId, FixedVec3, Angle, Angle)> = self
            .sim
            .players()
            .map(|(id, p)| (id, p.pos, p.yaw(), p.pitch()))
            .collect();

        for (&watcher, view) in self.views.iter_mut() {
            for &(who, pos, yaw, pitch) in &others {
                if who == watcher {
                    continue; // you are not news to yourself
                }
                let block = [
                    pos.x.floor_block(),
                    pos.y.floor_block(),
                    pos.z.floor_block(),
                ];
                if view.perceives(block) {
                    view.push(Effect::PlayerMoved {
                        who,
                        pos,
                        yaw,
                        pitch,
                    });
                }
            }
        }
    }

    /// Apply one action (§8.3). What changed comes back through
    /// [`drain_effects`](Self::drain_effects), not from here — the same way it
    /// will when the action arrived over a socket and the reply is a separate
    /// message rather than a return value.
    ///
    /// **The server raycasts here, not the client.** That is the point of the
    /// action being `Break` rather than `Break(block)`.
    pub fn apply(&mut self, action: Action) {
        self.apply_as(self.local, action);
    }

    /// The same, on behalf of a named client (block 2.11).
    ///
    /// Which client acted matters now, and not only for bookkeeping: `Open` is
    /// addressed to the person who clicked, where an edit is addressed to a
    /// *place* and reaches everyone standing near it. Over a socket this
    /// argument is "which connection sent this" -- the server's idea of who they
    /// are, never theirs, which is what makes section 3.4's rule structural.
    pub fn apply_as(&mut self, who: PlayerId, action: Action) {
        match action {
            Action::Break => {
                if let Some(block) = self.break_looked_at() {
                    self.publish_at(block, Effect::CloseIfAt(block));
                }
            }
            Action::Interact => {
                if let Some(screen) = self.interact() {
                    self.publish_to(who, Effect::Open(screen));
                }
            }
            Action::Place => {
                // An interactive block under the crosshair takes precedence
                // over placing -- otherwise a bench is unusable the moment you
                // are holding anything, which is most of the time.
                if let Some(screen) = self.interact() {
                    self.publish_to(who, Effect::Open(screen));
                    return;
                }
                self.place_held();
            }
            Action::ClickFurnace { pos, slot } => self.click_furnace(pos, slot),
        }
    }

    /// Everything a client needs to bring an empty replica up to date: every
    /// edit and every block entity, as ordinary [`Effect`]s.
    ///
    /// **The join handshake** (§8.3). A client that has just connected — or one
    /// whose world was replaced by a load — cannot be patched with a delta,
    /// because there is no delta from "a world it has never seen". The terrain
    /// is not in here and never will be: it is a pure function of the seed
    /// (§3.4), so the client generates it.
    ///
    /// The edits come out in position order and so are applied in position
    /// order, which is what makes a replica built from a snapshot identical to
    /// one built from the stream of edits that produced it (Rule 1).
    pub fn snapshot(&self) -> Vec<Effect> {
        self.snapshot_for(self.local)
    }

    /// The join handshake for one named client (block 2.11).
    ///
    /// **Filtered to what that client can perceive**, which is the difference
    /// between a handshake whose size depends on how long the world has been
    /// played and one whose size depends on how much has been built *near the
    /// joiner*. On a server that has been up for a month those are not the same
    /// number.
    ///
    /// A client with no view yet gets everything -- that is the singleplayer and
    /// early-test path, where "everything" is what it can perceive anyway, and
    /// silently sending nothing would be the worse failure.
    ///
    /// **No terrain, ever** (design §3). Terrain is a pure function of the seed,
    /// proven bit-identical on both CI platforms, so the client generates it.
    /// That is what makes the byte count scale with how much players have
    /// *changed* the world rather than with how much of it they can see, and
    /// `the_join_handshake_carries_no_terrain` is what keeps it true.
    pub fn snapshot_for(&self, who: PlayerId) -> Vec<Effect> {
        let view = self.views.get(&who);
        let visible = |pos: [i32; 3]| match view {
            Some(v) => v.perceives(pos),
            None => true,
        };
        let edits = self
            .world
            .edits()
            .filter(|(pos, _)| visible(*pos))
            .map(|(pos, block)| Effect::Edit { pos, block });
        let entities = self
            .world
            .block_entities()
            .filter(|(pos, _)| visible(**pos))
            .map(|(pos, f)| Effect::BlockEntity {
                pos: *pos,
                furnace: Some(*f),
            });
        edits.chain(entities).collect()
    }

    /// Tell the client to close whatever screen is showing the block at `pos`.
    ///
    /// `Action::Break` does this for itself; this exists for
    /// [`break_at`](Self::break_at), which is the entry point that bypasses the
    /// raycast and so bypasses the action.
    pub fn close_if_at(&mut self, pos: [i32; 3]) {
        self.publish_at(pos, Effect::CloseIfAt(pos));
    }

    /// Edit a block and record it for the client's replica.
    ///
    /// **Every authoritative block change goes through here.** One that did not
    /// would leave the client's world quietly wrong -- and quietly wrong in a
    /// replica is the failure mode with no symptom until someone walks into a
    /// wall that is not there.
    ///
    /// Public because that rule applies to tests too: a test that reaches past
    /// this into `server.world` is setting up a world the client will never be
    /// told about, and will then fail for a reason that has nothing to do with
    /// what it was testing.
    pub fn set_block(&mut self, pos: [i32; 3], block: BlockId) -> ChunkCoord {
        let cc = Arc::make_mut(&mut self.world).set_block(pos[0], pos[1], pos[2], block);
        self.publish_at(pos, Effect::Edit { pos, block });
        cc
    }

    /// Give the block at `pos` a furnace, and tell the client.
    pub fn add_furnace(&mut self, pos: [i32; 3]) {
        Arc::make_mut(&mut self.world).add_furnace(pos);
        self.note_block_entity(pos);
    }

    /// Set the furnace at `pos` to exactly `furnace`, and tell the client.
    ///
    /// The wholesale setter, for putting a world into a known state. Ordinary
    /// play does not use it -- smelting goes through [`tick_furnaces`](Self::tick_furnaces)
    /// and clicks through [`Action::ClickFurnace`], both of which journal for
    /// themselves.
    pub fn set_furnace(&mut self, pos: [i32; 3], furnace: Furnace) {
        Arc::make_mut(&mut self.world).put_furnace(pos, furnace);
        self.note_block_entity(pos);
    }

    /// Record the block entity at `pos` as it now stands, for the same reason.
    fn note_block_entity(&mut self, pos: [i32; 3]) {
        let furnace = self.world.furnace_at(pos).copied();
        self.publish_at(pos, Effect::BlockEntity { pos, furnace });
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
            self.publish_at(
                block,
                Effect::BlockEntity {
                    pos: block,
                    furnace: None,
                },
            );
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
        // borrowed while `self.sim.player_mut(self.local).inventory` is mutated. A helper
        // returning them all borrows the whole of `self`.
        if let (Some(registry), Some(terrain), Some(items)) = (
            self.blocks_registry.as_deref(),
            self.terrain,
            self.items.as_ref(),
        ) {
            let broken = self.world.block_at(x, y, z, terrain);
            let held = self.sim.player_mut(self.local).inventory.selected_stack();
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
                    if let Some(rest) = self.sim.player_mut(self.local).inventory.add(stack, items)
                    {
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

        self.set_block(block, BlockId::AIR)
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
        let inv = &mut self.sim.player_mut(self.local).inventory;
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
        let origin = self.sim.player_mut(self.local).pos.to_f32();
        let dir = self.sim.player_mut(self.local).look_dir_f32().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        let [x, y, z] = hit.block;
        match registry.interact(self.world.block_at(x, y, z, terrain)) {
            Interact::None => None,
            Interact::Bench => {
                // Width lives on `Crafting` (world state), not on the screen: a
                // 3x3 grid holding items in its outer cells is a different world
                // from a 2x2 one, and the hash already covers it.
                self.sim.player_mut(self.local).crafting.set_width(3);
                Some(Screen::Bench)
            }
            Interact::Furnace => {
                // A furnace placed before this block existed (or loaded from an
                // older save) has no entity yet; give it one on first use rather
                // than refusing to open.
                Arc::make_mut(&mut self.world).add_furnace([x, y, z]);
                self.note_block_entity([x, y, z]);
                Some(Screen::Furnace([x, y, z]))
            }
        }
    }

    /// A click on the open furnace's screen.
    ///
    /// Swap-on-click, matching the crafting cursor's feel: clicking a furnace
    /// slot with something held puts it in, clicking with an empty hand takes
    /// what is there. The output slot is take-only -- putting an ingot back
    /// into the output would be a way to duplicate work when the next smelt
    /// completes and stacks onto it.
    ///
    /// Uses the crafting cursor (`player.crafting.held()`) rather than a second
    /// one, so a player never has two things in hand at once and closing either
    /// screen has one rule for what happens to it.
    ///
    /// **Authority, despite looking like UI.** It moves items between a player's
    /// hand and a block entity, which is world state -- so it happens here and
    /// comes back as a `BlockEntity` effect, rather than the client editing a
    /// furnace it does not own.
    fn click_furnace(&mut self, pos: [i32; 3], slot: FurnaceSlot) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        let held = self.sim.player_mut(self.local).crafting.held();
        let world = Arc::make_mut(&mut self.world);
        let Some(f) = world.furnace_at_mut(pos) else {
            // A furnace that is not there cannot be clicked. This is the
            // validation that makes `ClickFurnace`'s named position a lookup
            // rather than something the server took on trust.
            return;
        };
        let slot = match slot {
            FurnaceSlot::Input => &mut f.input,
            FurnaceSlot::Fuel => &mut f.fuel,
            FurnaceSlot::Output => {
                // Take-only.
                if held.is_none() {
                    if let Some((id, count)) = f.output.take() {
                        if let Ok(stack) = items.new_stack(id, count) {
                            self.sim
                                .player_mut(self.local)
                                .crafting
                                .set_held(Some(stack));
                        }
                    }
                }
                self.note_block_entity(pos);
                return;
            }
        };
        match held {
            Some(stack) => {
                let previous = slot.replace((stack.item(), stack.count()));
                let give_back = previous.and_then(|(id, c)| items.new_stack(id, c).ok());
                self.sim.player_mut(self.local).crafting.set_held(give_back);
            }
            None => {
                let taken = slot.take().and_then(|(id, c)| items.new_stack(id, c).ok());
                self.sim.player_mut(self.local).crafting.set_held(taken);
            }
        }
        self.note_block_entity(pos);
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
        let centre = ChunkCoord::from_world_pos(self.sim.player_mut(self.local).pos.to_f32());
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
        // Which ones actually changed, so the client is told about those and
        // not about the hundred furnaces sitting idle with no fuel. Collected
        // rather than journalled in the loop because `world` is borrowed from
        // `self` for the whole of it.
        let mut changed = Vec::new();
        for pos in positions {
            let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
            if world.chunk_states().get(coord) != ChunkState::Active {
                continue;
            }
            // A chunk that woke this tick owes the ticks it slept through *plus*
            // this one -- the same total the two-pass version produced, which is
            // what keeps the dormancy gate test passing.
            let ticks = caught_up.get(&coord).copied().unwrap_or(0) + 1;
            if advance_furnace(world, pos, ticks, items, smelting) {
                changed.push(pos);
            }
        }
        for pos in changed {
            self.note_block_entity(pos);
        }
    }

    fn break_looked_at(&mut self) -> Option<[i32; 3]> {
        let origin = self.sim.player_mut(self.local).pos.to_f32();
        let dir = self.sim.player_mut(self.local).look_dir_f32().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        self.break_at(hit.block);
        Some(hit.block)
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
        let held = self.sim.player_mut(self.local).inventory.selected_stack()?;
        let block = registry.id_of(items.name_of(held.item())?)?;

        let origin = self.sim.player_mut(self.local).pos.to_f32();
        let dir = self.sim.player_mut(self.local).look_dir_f32().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        let target = [
            hit.block[0] + hit.normal[0],
            hit.block[1] + hit.normal[1],
            hit.block[2] + hit.normal[2],
        ];

        // Only now that the placement is certain to happen.
        let slot = self.sim.player_mut(self.local).inventory.selected_slot() as usize;
        self.sim
            .player_mut(self.local)
            .inventory
            .take_one(slot, items)?;

        // A block that owns state gets it the moment it is placed, rather than
        // on first use -- so a furnace someone never opens still ticks, and the
        // world hash covers it either way.
        let interactive = self
            .blocks_registry
            .as_deref()
            .map(|r| r.interact(block) == Interact::Furnace)
            .unwrap_or(false);
        let cc = self.set_block(target, block);
        if interactive {
            Arc::make_mut(&mut self.world).add_furnace(target);
            self.note_block_entity(target);
        }
        Some(cc)
    }
}

/// Advance one furnace by `ticks`, whether that is one ordinary tick or a
/// dormant chunk's whole backlog. Reports whether anything about it changed, so
/// the caller can tell a client about the ones that did (§8.3) and stay quiet
/// about the ones idling with no fuel.
fn advance_furnace(
    world: &mut World,
    pos: [i32; 3],
    ticks: u64,
    items: &ItemRegistry,
    smelting: &SmeltBook,
) -> bool {
    let Some(f) = world.furnace_at_mut(pos) else {
        return false;
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
    f.advance(ticks, &ctx).changed
}
/// How far from the player, in chunks, the simulation keeps running
/// (`PHASE2_ARCHITECTURE.md` §11.4).
///
/// **Deliberately unrelated to render distance.** Coupling them would let the
/// settings menu quietly change what the world simulates. Small, because
/// simulation is the expensive part and dormancy is what makes a big world
/// affordable; expected to grow once block 2.7 makes a dormant chunk nearly
/// free.
pub(crate) const SIM_RADIUS_CHUNKS: i32 = 4;

/// The vertical half-height of [`Server::hash`]'s region, in chunks.
///
/// Matches the world's own simulated band. Its constant is private to
/// `cubara-world` and this is a hash region rather than a lifecycle decision,
/// so it is stated here rather than reached for.
/// `the_hash_covers_every_chunk_that_is_simulating` is what stops the two
/// drifting: if the world's band grows past this, that test fails.
pub(crate) const SIM_HASH_VERTICAL_CHUNKS: i32 = 2;
/// The middle of block `b`, where an item dropped by breaking it appears.
fn drop_centre(b: [i32; 3]) -> FixedVec3 {
    let half = cubara_voxel::Fixed::from_raw(cubara_voxel::fixed::ONE / 2);
    FixedVec3::from_blocks(b[0], b[1], b[2]) + FixedVec3::new(half, half, half)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Server::hash`] states its own vertical band because `cubara-world`'s
    /// is private. That is a duplicated constant, so it needs a check: every
    /// chunk the world is actually simulating must be inside the region the
    /// hash covers.
    ///
    /// If the world's simulated band ever grows past the hash's, this fails —
    /// which is what stops two servers agreeing on a hash that quietly stopped
    /// covering the part of the world where they could disagree.
    #[test]
    fn the_hash_covers_every_chunk_that_is_simulating() {
        let mut server = Server::new();
        server.open(std::path::Path::new("/nonexistent-so-a-fresh-world"));
        // One tick is enough to establish the simulation radius.
        server.tick_sim(&InputFrame::default());
        server.tick_world();

        let region: std::collections::BTreeSet<_> = server.hash_region().into_iter().collect();
        let active: Vec<_> = server.world.chunk_states().active().collect();
        assert!(!active.is_empty(), "the radius was established");
        for coord in active {
            assert!(
                region.contains(&coord),
                "{coord:?} is simulating but is outside the hashed region"
            );
        }
    }

    /// A fresh server has no assets, and must still be usable — `Game::new()`
    /// builds one before a window exists, and a headless test that never loads
    /// anything should still be able to walk around.
    #[test]
    fn a_server_with_no_assets_still_ticks() {
        let mut server = Server::new();
        server.tick_sim(&InputFrame::default());
        server.tick_world();
        assert_eq!(server.sim.tick, 1);
    }
}
