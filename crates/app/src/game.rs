//! The game: world state, the simulation, and what input does to them.
//!
//! Deliberately separate from the renderer (`ARCHITECTURE.md` Rule 3). The renderer
//! draws what it is given; it does not decide where the player is looking or which
//! block a click breaks. When those lived on `Renderer` it could place blocks, which
//! is the boundary error the rule names — and the pattern that makes the reference
//! anti-pattern codebase impossible to change one system at a time.
//!
//! Nothing here touches the GPU, so all of it is testable without an adapter. It does
//! own the fixed-timestep accumulator (block 1.6, issue #57): [`Game::advance`] takes
//! a wall-clock `dt` and turns it into zero or more fixed [`cubara_sim::TICK_DT`]
//! steps, but never calls `Instant::now()` itself -- that stays in `main.rs`, per
//! `docs/PHASE1_ARCHITECTURE.md` §9, so this is testable with arbitrary, scripted
//! frame times instead of real wall-clock timing.

use std::sync::Arc;

use cubara_render::CameraPose;
use cubara_render::{swatch_color, HotbarSlot, InventoryPanel, PanelSlotKind};
use cubara_sim::{InputFrame, Player, PlayerId, SENSITIVITY_PER_PIXEL, TICK_DT};
use cubara_sim::{SlotRef, HOTBAR_WIDTH};
use cubara_voxel::{Angle, BlockRegistry, ChunkCoord, FixedVec3, ItemRegistry, RecipeBook};
use cubara_world::{Furnace, TerrainBlocks, World};

use cubara_server::headless::{Config, Session};
use cubara_server::net::Link;
use cubara_server::others::OtherPlayers;
use cubara_server::predict::Prediction;
use cubara_server::wire::{ClientMessage, ServerMessage};
use cubara_server::{Action, Effect, FurnaceSlot, Screen, Server};

use winit::keyboard::KeyCode;

/// Caps how many fixed steps a single [`Game::advance`] call runs -- a stalled or
/// backgrounded window can hand back a huge `dt` (seconds, not milliseconds), and
/// without a cap the sim would try to fully "catch up" by ticking hundreds of times
/// in one frame, which take longer than real time to run, which produces a bigger
/// backlog next frame: a spiral of death. Past the cap the leftover backlog is
/// dropped, not accumulated -- the sim falls behind wall-clock time rather than
/// locking up trying to chase it.
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Everything the player *is* and *does*: the world they're in and the simulation
/// running against it.
/// The asset loaders and the world directory, re-exported from the crate that
/// now owns them.
///
/// They moved to `cubara-server` because the *server* decides what a block
/// means (`RESEARCH_MULTIPLAYER.md` §3.4) and because a dedicated server has to
/// be able to load them with no window. The client still needs the names — it
/// draws items and builds the same registries — so it says where they went
/// rather than keeping a second copy.
pub use cubara_server::assets::{
    load_item_registry, load_ore_registry, load_recipe_book, load_structure_registry, world_dir,
};

pub struct Game {
    /// The authoritative half (`docs/RESEARCH_MULTIPLAYER.md` §8): the world,
    /// the simulation and the registries.
    ///
    /// `Game` is now the **client**, plus the wiring that runs a server
    /// in-process for singleplayer (§3.3). Everything left on `Game` itself is
    /// input, screen state or presentation — the §8.1 table is the sorting.
    ///
    /// **`None` when connected to somebody else's world**, which is the point of
    /// the `Option`: a client that can only exist next to a server is a client
    /// that cannot connect to one. Singleplayer is not a special case here — it
    /// is a client that happens to run the server in its own process, reached
    /// over the same [`Link`] a socket carries (design §5.1).
    ///
    /// A `Session` rather than a bare `Server` because hosting is more than
    /// ticking: accepting clients, collecting their input, flushing what they
    /// are owed, autosaving. All of that exists there already, and a second copy
    /// inside `Game` is how the two would drift apart.
    host: Option<Session>,
    /// How this client talks to whichever server it is on.
    ///
    /// `None` before there is one: `Game::new()` runs before a window exists and
    /// a world cannot open until assets are loaded, so there is a moment where a
    /// client has no server at all.
    link: Option<Link<ClientMessage, ServerMessage>>,
    /// What a host needs to run a world. Meaningless when connected to a remote
    /// one, because then there is no world here to run.
    cfg: Config,
    /// Which player this client drives, **as the server named it** in `Welcome`.
    ///
    /// Not assumed to be `PlayerId::LOCAL`: over a socket the id depends on who
    /// joined first, and a client that guessed would be a client acting as
    /// somebody else.
    me_id: PlayerId,
    /// **The client's own definitions** (block 2.12b).
    ///
    /// What a block drops, what an item stacks to, what a recipe makes. The
    /// client needs all of it to draw a hotbar and preview a craft, and it used
    /// to read every one out of `self.server` — another field access standing in
    /// for something a connected client has no way to reach.
    ///
    /// Loaded from `assets/` on this machine rather than sent over the wire.
    /// That is the same decision terrain rests on: definitions are data both
    /// sides already have, and shipping them would be shipping a copy of the
    /// game. What crosses the wire instead is a *fingerprint* of them, and
    /// `join::accept` refuses a server whose registries are not ours — so
    /// "both sides already have it" is checked rather than hoped for.
    assets: Option<ClientAssets>,
    /// **The client's own player** (block 2.12b part B).
    ///
    /// Everything this client believes about itself: its pose, predicted with
    /// block 2.13's machinery, and its inventory and crafting grid, replicated
    /// through `Effect::SelfItems`. Nothing here is authority — it is corrected
    /// by `Effect::SelfState` every tick.
    ///
    /// It exists because `Game` used to read `server.sim.player(server.local)`,
    /// which is a field access standing in for a round trip. Over a socket there
    /// is no such field: a connected client has no `Server` at all, and this is
    /// what it draws itself from instead.
    me: Prediction,
    /// Everyone else this client can see (block 2.12b).
    ///
    /// Fed by `Effect::PlayerMoved` and `PlayerGone`, which were no-ops here
    /// until there was something to draw. The *local* player is never in it:
    /// you are the camera.
    others: OtherPlayers,
    /// The client's own world (`RESEARCH_MULTIPLAYER.md` §8.2).
    ///
    /// **A replica, not a cache, and not the server's.** The instinct is to
    /// share one `World` in singleplayer; that defeats the exercise, because the
    /// seam only tells you something if the client cannot reach into the
    /// server's state, and an in-process shortcut is exactly what will not exist
    /// over a socket.
    ///
    /// Affordable only because terrain is a pure function of the seed (§3.4), so
    /// this copy is **generated, never received**. What crosses is the edit
    /// overlay and the block entities, which is already how a `World` is built
    /// and already what the save format persists.
    ///
    /// It may be wrong, briefly, and nothing may treat it as authority.
    world: Arc<World>,
    /// The player's pose as of the *previous* completed tick -- together with
    /// `sim.player` (the current tick), what [`Game::camera_pose`] interpolates
    /// between for smooth rendering of a 60 Hz sim at any frame rate (§9).
    prev_player: Player,
    /// The block registry, shared with `NodeStreaming` rather than loaded
    /// twice -- ids are per-registry (`PHASE2_ARCHITECTURE.md` §1.2), so two
    /// loads would be two id spaces and the same number would mean different
    /// materials on each side. `None` until `resumed` builds it.
    /// Which ids the terrain's grass/soil/stone are, in that registry.
    /// What items exist. Loaded by the app, not by `cubara-render`: items are
    /// not a render concern (Rule 3).
    /// Every recipe, loaded alongside the items they name.
    /// Whether the inventory screen is open. Screen state, not world state --
    /// what the *grid* holds is world state and lives on the player.
    inventory_open: bool,
    /// The chunk the simulation radius was last updated around. `None` until
    /// the first tick, so it always runs once.
    /// The furnace whose screen is open, by world position. `None` when the
    /// open screen is the plain inventory or a bench.
    open_furnace: Option<[i32; 3]>,
    /// Every smelting recipe, loaded alongside the items they name.
    /// Whether the break button is currently held. Read once per `advance`
    /// into [`InputFrame::breaking`].
    breaking: bool,
    /// Wall-clock seconds not yet consumed by a fixed tick. `f64`, not `f32`
    /// like everything else here -- this is the one value that keeps being
    /// added to across a whole play session (thousands of frames), and
    /// `f32`'s ~7 significant digits let rounding error accumulate enough to
    /// tip a close call across a tick boundary a step early or late (this
    /// is exactly what `frame_rate_independent_movement_reaches_the_same_state`
    /// caught: two runs summing to the same total elapsed time, chopped into
    /// frames differently, landed one tick apart with an `f32` accumulator).
    accumulator: f64,
    // Held movement keys, translated into `InputFrame::move_axes` once per
    // `advance` call.
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    /// Whether the free-fly toggle key is currently held -- tracked so
    /// `key_input` can tell a fresh press from OS key-repeat and only raise
    /// `fly_toggle_pending` on the rising edge.
    fly_toggle_held: bool,
    /// Jump / fly-toggle: `true` from the tick they were pressed until the
    /// next `advance` call hands them to `Sim::tick` as an `InputFrame`
    /// button edge, then cleared -- see `advance`'s doc comment for why
    /// they're consumed by only the first tick of a catch-up burst.
    jump_pending: bool,
    fly_toggle_pending: bool,
    /// Mouse motion (pixels) accumulated since the last `advance` call.
    /// Mouse motion accumulated since the last tick, already converted to
    /// [`Angle`]s.
    ///
    /// **Converted here, not in the simulation.** `InputFrame::look_delta` is
    /// what will cross a socket, and §3.5 requires that nothing crossing the
    /// wire is a float. Sensitivity is a setting on the machine holding the
    /// mouse, so this is also where it belongs.
    look_delta: (Angle, Angle),
    /// Wheel notches not yet turned into a hotbar step (see `scroll_hotbar`).
    scroll_pending: f32,
}

/// The definitions a client needs, held by the client.
///
/// Bundled rather than four fields on `Game` because they arrive together, are
/// replaced together, and are meaningless apart: `terrain` is derived from
/// `blocks`, and a recipe book resolved against one item registry cannot be read
/// against another.
pub struct ClientAssets {
    /// Kept for the join handshake: `join::accept` fingerprints it against the
    /// server's. It was left out when this struct was written, correctly --
    /// nothing read it then, and a field held "for later" is dead code with a
    /// promise attached. `connect` is the reader that earns it.
    pub blocks: Arc<BlockRegistry>,
    pub items: ItemRegistry,
    pub recipes: RecipeBook,
    /// Smelting recipes -- read for how long the item in a furnace takes, so
    /// the screen can show how far along it is.
    pub smelting: cubara_voxel::SmeltBook,
    /// Which ids the terrain is made of. Derived from `blocks` once, here,
    /// rather than recomputed at each of the places that raycast.
    pub terrain: TerrainBlocks,
}

/// **The colour this build wears**, chosen by which machine it was built for.
///
/// The owner's arrangement for three machines in one world: Linux red, Windows
/// yellow, macOS green. Hard-coded per client, which is the whole design —
/// a client announces its own colour when it joins and the server relays it
/// without ever interpreting it. A server that mapped platforms to colours
/// would be a server that knew what an operating system is, which is not its
/// business (Rule 3, in the direction nobody usually checks).
///
/// Anything else is grey, and deliberately drab: an unnamed platform should
/// look unnamed rather than quietly borrow one of the three.
pub const MY_SHIRT: cubara_server::wire::Shirt = if cfg!(target_os = "linux") {
    [204, 41, 41]
} else if cfg!(target_os = "windows") {
    [235, 209, 41]
} else if cfg!(target_os = "macos") {
    [51, 179, 61]
} else {
    [140, 140, 140]
};

/// What somebody is drawn in before they have said what they wear.
///
/// Only ever seen for the tick or two between a pose arriving and the colour
/// that goes with it -- normally they are in the same batch. Grey rather than a
/// guess: briefly drab is better than briefly wrong, because wrong is
/// indistinguishable from somebody else.
const UNKNOWN_SHIRT: [f32; 3] = [0.55, 0.55, 0.55];

/// What one slot of an open screen holds: an item and how many, or nothing.
type SlotStack = Option<(cubara_voxel::ItemId, u8)>;

/// What a player reads for an item id: `cubara:wooden_pick` is "Wooden Pick".
///
/// Derived rather than declared, because no item file carries a display name
/// and inventing one per item would be content nobody asked for. The namespace
/// is dropped, underscores become spaces, and each word is capitalised.
pub fn display_name(id: &str) -> String {
    let bare = id.rsplit(':').next().unwrap_or(id);
    bare.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The hotbar slot `steps` to the right of `current` (negative is left),
/// wrapping around the ends.
fn scrolled_slot(current: u8, steps: i32) -> u8 {
    (current as i32 + steps).rem_euclid(HOTBAR_WIDTH as i32) as u8
}

impl Game {
    /// Start above the terrain near the origin, looking out over it and slightly
    /// down (yaw ~35°, gentle downward pitch). Walking mode by default (not
    /// free-fly, per issue #53's Context: the point of this block is a world
    /// you land in and walk, not one you start already flying over) -- gravity
    /// carries the player down onto the terrain below.
    pub fn new() -> Self {
        // **No server yet, and no world.** A client is constructed before the
        // window exists, and a world cannot open until assets are loaded --
        // which happens once the renderer has validated its textures. Until
        // then this is a client with nothing to look at, which is exactly what
        // a client waiting to connect also is.
        //
        // The placeholder seed and player are replaced the moment a server is
        // joined, by `Welcome` and the first `SelfState`. In-process that is
        // milliseconds later, in `set_assets`.
        let player = Player::new(
            FixedVec3::from_blocks(0, 48, 0),
            Angle::from_radians(0.6),
            Angle::from_radians(-0.3),
        );
        // Hosting from the first tick, over a link, exactly as a connected
        // client would be. The world it hosts is empty until `set_assets`, which
        // is the same moment the old code stood the player on the ground.
        let cfg = Config {
            world: world_dir(),
            autosave_ticks: 0,
            ..Config::default()
        };
        // **Hosted, but not yet joined.** A server with no assets has no
        // spawn point -- "which block is solid" is a question about ids, and
        // there are none until `set_assets`. So the world exists and this
        // client is not in it yet, which is exactly the state a client waiting
        // to connect is in.
        let mut host = Session::around(Server::new(), &cfg);
        // Joined immediately, over a link, exactly as a connected client is.
        // A world with no assets still has somewhere to stand, so there is no
        // reason to make this client wait for the renderer.
        let mut link = host.attach();
        let me_id = welcome_id(&mut link).unwrap_or(PlayerId::LOCAL);

        Self {
            world: Arc::new(World::new()),
            me: Prediction::new(World::new().seed(), player),
            others: OtherPlayers::new(),
            host: Some(host),
            link: Some(link),
            cfg,
            me_id,
            prev_player: player,
            assets: None,
            inventory_open: false,
            open_furnace: None,
            breaking: false,
            accumulator: 0.0,
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
            fly_toggle_held: false,
            jump_pending: false,
            fly_toggle_pending: false,
            look_delta: (Angle::ZERO, Angle::ZERO),
            scroll_pending: 0.0,
        }
    }

    /// Which ids the terrain is made of, from the client's own definitions.
    ///
    /// A treeless default before assets are set, matching `Server::terrain` --
    /// `Game::new()` runs before a window exists, and a headless test that never
    /// loads anything should still be able to walk around.
    pub fn terrain(&self) -> TerrainBlocks {
        self.assets
            .as_ref()
            .map(|a| a.terrain)
            .unwrap_or(TerrainBlocks {
                oak: None,
                ores: cubara_world::OreSet::EMPTY,
                grass: cubara_voxel::BlockId::AIR,
                soil: cubara_voxel::BlockId::AIR,
                stone: cubara_voxel::BlockId::AIR,
            })
    }

    /// This client's player *as the host has it*, for tests that need to look
    /// past the client's own copy at what the world actually says.
    #[cfg(test)]
    fn host_player(&self) -> &Player {
        let who = self.me_id;
        self.server().sim.player(who)
    }

    #[cfg(test)]
    fn host_player_mut(&mut self) -> &mut Player {
        let who = self.me_id;
        self.server_mut().sim.player_mut(who)
    }

    /// Run the host one tick and take in what it produced, without advancing
    /// the client's own clock.
    ///
    /// What a test needs after reaching past the client to set something up on
    /// the host: the client learns about the world through messages now, so
    /// state poked directly into the server has not reached it yet. Naming the
    /// step is better than hiding it — the delay is real, and over a socket it
    /// is longer.
    /// [`settle`](Self::settle), returning which chunks it made stale.
    #[cfg(test)]
    fn settle_dirty(&mut self) -> Vec<ChunkCoord> {
        if let Some(host) = self.host.as_mut() {
            host.advance(1, &self.cfg);
        }
        self.sync()
    }

    #[cfg(test)]
    fn settle(&mut self) {
        if let Some(host) = self.host.as_mut() {
            host.advance(1, &self.cfg);
        }
        self.sync();
    }

    /// Leave this client's own world and join somebody else's.
    ///
    /// The host is dropped, which stops the world it was running: a machine that
    /// has connected somewhere is not also simulating a world nobody is in.
    /// After this every field that answers a question about the world answers it
    /// from messages, because there is nothing else left to ask.
    ///
    /// The registries stay: they were loaded from `assets/` on this machine and
    /// the handshake has just proved they match the server's. That check is the
    /// reason this can refuse rather than desynchronise -- ids cross the wire
    /// raw, and two machines whose assets differ disagree about every id from
    /// the first differing name onward.
    pub fn connect(&mut self, addr: &str) -> Result<(), String> {
        // The assets check lives in `join_over`, which is where the handshake
        // needs them -- opening a socket first and failing after would leave a
        // connection nobody closes.
        if self.assets.is_none() {
            return Err("connect before assets are loaded".to_string());
        }
        let link = cubara_server::net::connect(addr)
            .map_err(|e| format!("could not reach {addr}: {e}"))?;
        self.join_over(link)
            .map_err(|e| format!("could not join {addr}: {e}"))
    }

    /// The join itself, over a link that is already open.
    ///
    /// Split from [`connect`](Self::connect) because nothing about a handshake
    /// is a property of a socket: the same exchange happens over a channel, and
    /// a test that had to open a port to check it would be testing the operating
    /// system. `crates/server/tests/two_processes.rs` covers the socket.
    fn join_over(&mut self, mut link: Link<ClientMessage, ServerMessage>) -> Result<(), String> {
        let Some(assets) = self.assets.as_ref() else {
            return Err("joined before assets are loaded".to_string());
        };
        link.send(ClientMessage::Hello(MY_SHIRT));

        // Wait for the welcome, and only for that. A client that blocked on the
        // whole handshake would freeze its window on a slow connection; a client
        // that did not block at all would not know its own id, and every action
        // it sent before the answer arrived would be sent as nobody.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let welcome = loop {
            if let Some(m) = link
                .poll()
                .into_iter()
                .find(|m| matches!(m, ServerMessage::Welcome { .. }))
            {
                break m;
            }
            if link.is_closed() {
                return Err("the server closed the connection".to_string());
            }
            if std::time::Instant::now() > deadline {
                return Err("the server did not answer".to_string());
            }
            // Deliberately untested, and said rather than left ambiguous: a
            // test would need a socket that accepts and then stays silent for
            // the whole ten seconds, on every run on three platforms. The
            // failure this guards against is also the loudest kind -- a broken
            // deadline makes *every* join fail instantly with "the server did
            // not answer", which the first person to try `--connect` sees. The
            // opposite direction, that a prompt server is not timed out, is
            // covered by `a_joined_client_drives_the_player_the_server_named`.
            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        let joined = cubara_server::join::accept(&welcome, &assets.blocks, &assets.items)
            .map_err(|e| e.to_string())?;

        // Terrain is generated here from the seed, never received (§3). Nothing
        // of the old world survives: a different seed is a different world.
        self.world = Arc::new(World::with_seed(joined.seed));
        self.me = Prediction::new(joined.seed, *self.me.player());
        self.others = OtherPlayers::new();
        self.me_id = joined.you;
        self.link = Some(link);
        self.host = None;
        log::info!("joined as {:?} (seed {})", joined.you, joined.seed);
        Ok(())
    }

    /// Let the host run once so the client hears about the world it just got.
    ///
    /// Separate from the test-only `settle` because this one runs in the real
    /// app: `set_assets` moves the player onto ground the client has never been
    /// told about, and without a tick the first frame would render them at
    /// y = 48 and then snap.
    fn settle_after_assets(&mut self) {
        if let Some(host) = self.host.as_mut() {
            host.advance(1, &self.cfg);
        }
        self.sync();
        self.prev_player = *self.me.player();
    }

    /// Ask the server to do something.
    ///
    /// The one place an action leaves this client. In-process it is a channel
    /// push the host picks up on its next tick; over a socket it is a packet.
    /// Neither the caller nor this method can tell which, which is the point of
    /// there being one type of link (design §5.1).
    fn act(&mut self, action: Action) {
        if let Some(link) = self.link.as_mut() {
            link.send(ClientMessage::Act(action));
        }
    }

    /// The server this client is hosting.
    ///
    /// Panics when there is none, deliberately: everything that reaches for it
    /// is host-only work — saving, loading, standing a player on the ground —
    /// and a connected client asking to save somebody else's world is a bug in
    /// the caller rather than a condition to handle.
    fn server(&self) -> &Server {
        &self
            .host
            .as_ref()
            .expect("this client is not hosting a world")
            .server
    }

    fn server_mut(&mut self) -> &mut Server {
        &mut self
            .host
            .as_mut()
            .expect("this client is not hosting a world")
            .server
    }

    /// The client's replica (§8.2) -- what the renderer meshes and what the
    /// crosshair raycasts against. Never the server's world.
    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    /// The camera pose to render from: the sim's player pose, interpolated
    /// between its previous and current tick by the accumulator's leftover
    /// fraction. Render-side only (§9) -- never read back into the sim.
    pub fn camera_pose(&self) -> CameraPose {
        let alpha = (self.accumulator / TICK_DT as f64).clamp(0.0, 1.0) as f32;
        let player = self.prev_player.lerp(self.me.player(), alpha);
        CameraPose {
            // The renderer works in floats, and that is the correct side of the
            // seam for it: a wrong last bit in a camera matrix is a sub-pixel
            // difference (§3.5, presentation may be float).
            eye: glam::Vec3::from_array(player.pos.to_f32()),
            look_dir: player.look_dir_f32(),
        }
    }

    /// The other players in sight, ready to draw (block 2.12b).
    ///
    /// Interpolated at the same `alpha` the camera uses, so everyone on screen
    /// is showing the same instant.
    ///
    /// **Shirt colours.** The owner asked for a red and a green shirt, and the
    /// two are handed out by `PlayerId` order — lowest is red, next is green.
    /// By id rather than by "whoever I am not", because the id comes from the
    /// server: both machines then agree about who is red, and two people
    /// describing what they see to each other are describing the same thing.
    /// If they were assigned per-screen, you would each be red on your own.
    ///
    /// A third player onwards gets a colour hashed from their id through
    /// `swatch_color`, the same function that colours a block with no texture.
    /// Two shirts were what was asked for; running out of them silently is not
    /// something to leave for a stranger to discover.
    pub fn other_players(&self) -> Vec<cubara_render::PlayerView> {
        let alpha = (self.accumulator / TICK_DT as f64).clamp(0.0, 1.0) as f32;
        self.others
            .drawn(alpha)
            .into_iter()
            .map(|(_, pose)| cubara_render::PlayerView {
                eye: pose.pos,
                yaw: pose.yaw,
                shirt: match pose.shirt {
                    Some([r, g, b]) => [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
                    None => UNKNOWN_SHIRT,
                },
            })
            .collect()
    }

    /// The block the player is currently looking at, within [`REACH`], for
    /// the renderer to outline -- computed by the sim's own raycast each
    /// tick (`cubara_sim::Sim::tick`), not here. The renderer draws it; it
    /// does not decide it (`ARCHITECTURE.md` Rule 3, issue #52).
    pub fn selected_block(&self) -> Option<[i32; 3]> {
        self.me.player().target
    }

    /// Record a movement key going down/up. Unmapped keys are ignored (returns
    /// whether the key was one the game cares about).
    ///
    /// Space and F4 double as both a *held* signal (free-fly's vertical axis,
    /// respectively nothing) and a *rising-edge* signal (jump; the free-fly
    /// toggle) -- `cubara-sim` decides which one applies, based on which mode
    /// the player is currently in, so this method just reports both truthfully
    /// rather than guessing the mode here (Rule 3: no gameplay decisions on
    /// this side of the seam beyond packaging raw input).
    pub fn key_input(&mut self, key: KeyCode, pressed: bool) -> bool {
        match key {
            KeyCode::KeyW | KeyCode::ArrowUp => self.forward = pressed,
            KeyCode::KeyS | KeyCode::ArrowDown => self.back = pressed,
            KeyCode::KeyA | KeyCode::ArrowLeft => self.left = pressed,
            KeyCode::KeyD | KeyCode::ArrowRight => self.right = pressed,
            KeyCode::Space => {
                if pressed && !self.up {
                    self.jump_pending = true;
                }
                self.up = pressed;
            }
            KeyCode::ShiftLeft | KeyCode::ShiftRight | KeyCode::ControlLeft => self.down = pressed,
            KeyCode::F4 => {
                if pressed && !self.fly_toggle_held {
                    self.fly_toggle_pending = true;
                }
                self.fly_toggle_held = pressed;
            }
            // Hotbar selection. Applied on press only -- releasing a number
            // key must not reselect, and holding it must not repeat.
            KeyCode::Digit1
            | KeyCode::Digit2
            | KeyCode::Digit3
            | KeyCode::Digit4
            | KeyCode::Digit5
            | KeyCode::Digit6
            | KeyCode::Digit7
            | KeyCode::Digit8
            | KeyCode::Digit9 => {
                if pressed {
                    let slot = match key {
                        KeyCode::Digit1 => 0,
                        KeyCode::Digit2 => 1,
                        KeyCode::Digit3 => 2,
                        KeyCode::Digit4 => 3,
                        KeyCode::Digit5 => 4,
                        KeyCode::Digit6 => 5,
                        KeyCode::Digit7 => 6,
                        KeyCode::Digit8 => 7,
                        _ => 8,
                    };
                    self.select_hotbar(slot);
                }
            }
            _ => return false,
        }
        true
    }

    /// Feed a raw mouse-motion delta (pixels) toward the next tick's look input.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        let scale = |px: f32| Angle::from_raw((px * SENSITIVITY_PER_PIXEL as f32) as i32);
        self.look_delta.0 = self.look_delta.0.wrapping_add(scale(dx));
        self.look_delta.1 = self.look_delta.1.wrapping_add(scale(dy));
    }

    /// Advance the simulation by `dt` wall-clock seconds: zero or more fixed
    /// [`TICK_DT`] steps, capped at [`MAX_TICKS_PER_FRAME`]. Input is sampled
    /// once (currently-held keys + mouse motion since the last call) and that
    /// same [`InputFrame`] drives every step this call runs -- ticks never read
    /// live input themselves, so a scripted/replayed input sequence (block 1.8)
    /// reproduces exactly.
    ///
    /// The one-shot accumulated inputs -- `look_delta` (however far the mouse
    /// moved since the last tick) and the `jump`/`toggle_fly` key edges -- are
    /// held until a tick actually consumes them, and cleared after the *first*
    /// tick of any catch-up burst. Two failure modes both come from getting
    /// that wrong, and only `move_axes` (a genuinely *held* continuous state,
    /// correctly re-applied once per backlog tick) is exempt:
    ///
    /// - **Draining them on a sub-tick frame drops them.** The renderer runs
    ///   uncapped, so on a fast machine most frames are shorter than one 60 Hz
    ///   tick and run zero ticks. Sampling+clearing the accumulators every
    ///   frame (as this did) threw away ~59 of every 60 frames' mouse motion --
    ///   you could barely look around at high FPS. So: bail out before touching
    ///   them when there isn't a tick's worth of time yet.
    /// - **Re-applying them on every tick of a burst multiplies them.** A
    ///   frame-pacing hiccup forces 2+ ticks into one call; reusing the
    ///   unmodified total each time (correct for held `move_axes`) would turn
    ///   one frame's mouse motion, or one key press, into N. So: clear them
    ///   after the first tick.
    pub fn advance(&mut self, dt: f32) -> Vec<ChunkCoord> {
        self.accumulator += dt as f64;

        // Not a tick's worth of time yet: leave the accumulated one-shot inputs
        // alone so a later frame's tick can consume them, rather than sampling
        // and clearing them here where no tick would apply them. `move_axes` is
        // re-read from live held state on the frame that does tick, so nothing
        // is lost by returning early.
        if self.accumulator < TICK_DT as f64 {
            return Vec::new();
        }

        let mut input = InputFrame {
            move_axes: [
                (self.right as i32 - self.left as i32) as f32,
                (self.up as i32 - self.down as i32) as f32,
                (self.forward as i32 - self.back as i32) as f32,
            ],
            look_delta: [self.look_delta.0, self.look_delta.1],
            jump: self.jump_pending,
            toggle_fly: self.fly_toggle_pending,
            breaking: self.breaking,
        };
        // A tick below will consume these, so it's safe to clear them now.
        self.look_delta = (Angle::ZERO, Angle::ZERO);
        self.jump_pending = false;
        self.fly_toggle_pending = false;

        let mut ticks = 0;
        while self.accumulator >= TICK_DT as f64 {
            self.prev_player = *self.me.player();

            // The client acts on its own input first, and is corrected below.
            // In-process the correction arrives on the same tick, so the
            // reconciliation is a no-op -- which is the point: singleplayer is
            // not a special case of multiplayer, it is multiplayer with a very
            // short wire (design §5.1).
            let blocks = self.terrain();
            let seq = self
                .me
                .predict(input, Arc::make_mut(&mut self.world), blocks);

            // **Sent, not called.** The input leaves this client the same way
            // it would over a socket; what happens next is the server's, and
            // in-process that server happens to be a field away.
            // Only when the prediction handed out a number. `None` means it
            // has stalled -- two seconds without an acknowledgement -- and its
            // contract is that there is then nothing to send, because sending
            // would lengthen a queue nobody is draining
            // (`crates/server/tests/prediction.rs` asserts exactly that).
            //
            // This used to fall back to a second counter on `Game`, which made
            // two numbers for one thing: the one the prediction reconciles
            // against, and one incremented every tick and read *only* when the
            // first said not to send. They agree until a stall and drift after
            // it, which is the worst possible moment for a sequence number to
            // become a guess.
            if let (Some(seq), Some(link)) = (seq, self.link.as_mut()) {
                link.send(ClientMessage::Input { seq, frame: input });
            }
            // The host runs one tick: accepting, collecting what clients sent,
            // simulating, flushing what they are owed. All of `Session::advance`
            // rather than a hand-rolled copy of it, so hosting here and hosting
            // headlessly cannot drift.
            if let Some(host) = self.host.as_mut() {
                host.advance(1, &self.cfg);
            }
            // Mining advances *per tick*, not per frame -- §4.3, and the same
            // reason the tick loop exists. A catch-up burst of N ticks is N
            // ticks of progress, which is correct: that time really did pass.
            //
            // Between the server's two halves, which is where it has always
            // run: moving it would reorder the tick, and tick order is Rule 1.
            //
            // On the **server** since block 2.14. It used to be counted here,
            // which meant the client decided when a block had been mined long
            // enough -- and a client that skipped the counting broke blocks
            // instantly. `InputFrame::breaking` already carries the only thing
            // a client can honestly know, which is that the button is down.

            // What the server thinks of this client, every tick: its pose, and
            // its items when they have changed. These are the only two ways the
            // client learns about itself now -- exactly the two a remote one
            // would have.

            input.jump = false;
            input.toggle_fly = false;
            input.look_delta = [Angle::ZERO, Angle::ZERO];
            self.accumulator -= TICK_DT as f64;
            ticks += 1;
            if ticks >= MAX_TICKS_PER_FRAME {
                // Spiral-of-death guard: fall behind wall-clock time rather than
                // trying to fully catch up, which would only make the next
                // frame's backlog worse.
                self.accumulator = 0.0;
                break;
            }
        }
        // Once, after the whole burst: the replica catches up with everything
        // the server did, and reports what needs re-meshing. A catch-up of five
        // ticks that broke the same chunk five times is one re-mesh.
        self.sync()
    }

    /// Write the world to disk (#179). The server owns the world, so it owns
    /// the save; this is the client's shutdown reaching for it.
    pub fn save(&self) {
        self.server().save_to(&world_dir());
    }

    /// Replace this game's world with the one on disk, if there is one (#179).
    pub fn load(&mut self) -> bool {
        self.load_from(&world_dir())
    }

    /// [`load`](Self::load) from a specific directory.
    pub fn load_from(&mut self, dir: &std::path::Path) -> bool {
        let loaded = self.server_mut().load_from(dir);
        if loaded {
            // A load replaces the server's world wholesale, so the replica is
            // rebuilt rather than patched: there is no edit stream that turns
            // one world into another. Over a socket this is a fresh join.
            self.world = Arc::new(World::with_seed(self.server().world.seed()));
            self.resync();
            // A load replaces the player wholesale, so the previous pose the
            // client interpolates from is now a pose from a different world.
            // From the client's own copy, not the server's. A load replaces the
            // player wholesale, and the client learns about it the way a remote
            // one would: through `SelfState`, which the resync above delivered.
            self.prev_player = *self.me.player();
        }
        loaded
    }

    /// The player's health, reduced to what the renderer draws
    /// (`PHASE2_ARCHITECTURE.md` §13.1).
    ///
    /// Both numbers, so `cubara-render` never learns what full health is --
    /// it is told the points and the maximum and works out the hearts (Rule 3).
    pub fn health_view(&self) -> cubara_render::HealthView {
        cubara_render::HealthView {
            points: self.me.player().health,
            max_points: cubara_sim::MAX_HEALTH,
        }
    }

    /// Whether the break button is held. Held state rather than an edge, since
    /// mining advances for as long as it is down (§4.3).
    ///
    /// Releasing abandons any break in progress on the next tick -- that is
    /// [`tick_mining`](Self::tick_mining)'s doing, not this setter's, so the
    /// abandon rule lives in one place.
    pub fn set_breaking(&mut self, held: bool) {
        self.breaking = held;
    }

    /// Break (`place = false`) or place (`true`) the block the player is looking
    /// at, within [`REACH`]. Returns the [`ChunkCoord`] whose geometry is now
    /// stale so the caller can re-mesh it, or `None` if nothing was in reach.
    ///
    /// Placing puts the block against the hit face. Uses the sim's current
    /// (non-interpolated) pose -- editing is a gameplay decision, and
    /// interpolation is a render-only concern (§9) that must never feed back
    /// into it. Raycasts fresh rather than reusing [`Sim::target`](cubara_sim::Sim)
    /// -- that field only updates on the next tick, so it can go stale
    /// against `self.server().world` after an edit lands (e.g. two edits between
    /// ticks); issue #52 scoped out any change to raycasting itself anyway.
    /// Give the game the assets it needs to turn blocks into items and back.
    /// Called once, when the window and its registry exist.
    pub fn set_assets(
        &mut self,
        registry: Arc<BlockRegistry>,
        items: ItemRegistry,
        recipes: RecipeBook,
    ) {
        // The client keeps its own copy. Cloned rather than shared: over a
        // socket there is nothing to share, and a client holding an `Arc` into
        // the server's registry would be an in-process shortcut that stops
        // existing the moment somebody connects.
        self.assets = Some(ClientAssets {
            terrain: TerrainBlocks::from_registry(&registry),
            blocks: Arc::clone(&registry),
            smelting: cubara_server::assets::load_smelt_book(&items),
            items: items.clone(),
            recipes: recipes.clone(),
        });
        self.server_mut().set_assets(registry, items, recipes);
        // Now that the world has ids it has a spawn point, so this client can
        // join it -- through `attach`, which is the same `welcome` a socket
        // takes. Before this there was nowhere to stand.
        // The world now has ids, so the player can be stood on real ground.
        let who = self.me_id;
        self.server_mut().place_on_ground_as(who);
        // And the client learns where that is the way it learns everything now.
        self.settle_after_assets();
        // The server just moved the player onto the ground; the client's
        // interpolation would otherwise smear them there from y = 48.
        self.prev_player = *self.me.player();
    }

    /// Break the targeted block and put its drop in the inventory.
    ///
    /// **The break always happens; the drop is conditional.** Three things
    /// decide what you get (`PHASE2_ARCHITECTURE.md` §4, block 2.4a):
    ///
    /// 1. The block's [`DropRule`] -- its own name, a specific item, or
    ///    nothing.
    /// 2. Its `requires_tier` against the held tool's tier. **Too low a tier
    ///    still breaks the block, but yields nothing.** §4 chose that over
    ///    refusing to break: a block that will not break with no explanation
    ///    reads as a bug, where one that breaks and drops nothing teaches the
    ///    rule in one go.
    /// 3. Whether an item of that name exists at all.
    ///
    /// **Durability is spent only on a break that yielded something.** A
    /// failed-tier break costs nothing -- §4: you are not punished twice for
    /// the same mistake. Breaking bare-handed costs nothing either, since only
    /// a tool carries durability.
    ///
    /// **A drop that does not fit is lost.** There are no dropped-item entities
    /// yet (they need ECS, 2.5), so the remainder `Inventory::add` hands back is
    /// logged and discarded. Refusing to break the block instead would be a
    /// gameplay decision, and those are the owner's.
    ///
    /// **No longer on the game's own path**, since 2.4b: playing mines over
    /// several ticks via [`tick_mining`](Self::tick_mining), and both go
    /// through the same [`break_at`](Self::break_at). Kept as the instant-break
    /// entry point, which is what the drop and tier tests drive directly rather
    /// than holding a button for eight ticks to assert one drop.
    #[allow(dead_code)]
    /// Break the block the player is looking at, returning the chunk whose
    /// geometry is now stale.
    ///
    /// Reached only through [`apply`](Self::apply) -- the raycast is the
    /// server's, which is what stops a client naming its own target (§8.3).
    /// **Waits for the host**, which only a test can do. Play breaks blocks by
    /// holding the button: `InputFrame::breaking` crosses the wire every tick
    /// and the server counts the ticks (block 2.14), so nothing in the game
    /// reaches here. `place_block`, which *is* on the game's path, deliberately
    /// does not wait -- ticking the world on a mouse click would move the
    /// simulation off its own clock.
    #[cfg(test)]
    pub fn break_block(&mut self) -> Option<ChunkCoord> {
        let who = self.me_id;
        self.server_mut().break_looked_at_as(who);
        // The break happened on the host; its effects reach this client's queue
        // when the host next flushes, which is a tick.
        if let Some(host) = self.host.as_mut() {
            host.advance(1, &self.cfg);
        }
        self.sync().into_iter().next()
    }

    /// The block being dug and how far along, for the renderer's cracks.
    ///
    /// **`None` when joined to someone else's world**, not a panic: progress is
    /// counted on the server and not sent, so a remote client has nothing to
    /// draw from -- and a missing crack is a far smaller failure than a crash
    /// on the first frame of digging. Predicting it the way block 2.13 predicts
    /// a pose is the way to give remote clients cracks.
    pub fn cracking(&self) -> Option<([i32; 3], f32)> {
        self.host.as_ref()?.server.mining_target(self.me_id)
    }

    /// How far along the current break is, `0.0..1.0`, for the renderer to draw
    /// a crack overlay with. `None` when nothing is being mined.
    ///
    /// Read from the server, which has counted the ticks since block 2.14.
    /// In-process that is a field access; over a socket a client will have to
    /// predict it the way block 2.13 predicts a pose, because progress is not
    /// something the server sends. Nothing draws this yet -- the crack overlay
    /// is 2.4d -- so the prediction is not built: it would be machinery with no
    /// caller, and the shape it should take depends on what the overlay needs.
    #[allow(dead_code)]
    pub fn mining_progress(&self) -> Option<f32> {
        self.server().mining_progress(self.me_id)
    }

    /// Place the held block, or use an interactive block under the crosshair.
    /// **Asked for, not answered.** What the placement changed arrives on the
    /// next tick like every other effect, and `advance` invalidates it there --
    /// one frame later, which is exactly what a client on a socket gets. It used
    /// to return the dirty chunk, which only worked because the server was a
    /// field away.
    pub fn place_block(&mut self) {
        self.act(Action::Place);
        self.sync();
    }

    /// Use whatever the player is looking at, reporting whether a screen
    /// opened. The server decides *what* was used (§8.3).
    ///
    /// Nothing on the game's own path calls this: right-click goes through
    /// [`Action::Place`], which tries interacting first so a bench stays usable
    /// while holding a block. This is the direct entry point the interaction
    /// tests drive, and the shape a second input binding would use.
    #[allow(dead_code)]
    /// Ask the server to use whatever is under the crosshair, and report
    /// whether a screen opened.
    ///
    /// **Waits for the host**, which only a test can do: in play the answer
    /// arrives on the next frame's tick like every other effect, and nothing
    /// asks this question synchronously. Reached only from tests since actions
    /// became messages -- a client cannot know what a click did until it is
    /// told, and pretending otherwise here would be the in-process shortcut §2
    /// forbids.
    #[cfg(test)]
    fn interact(&mut self) -> bool {
        self.act(Action::Interact);
        self.settle();
        self.open_furnace.is_some() || self.inventory_open
    }

    /// Break a specific block, bypassing the raycast.
    ///
    /// The direct entry point the drop and tier tests drive, rather than
    /// holding a button for eight ticks to assert one drop. Play goes through
    /// [`Action::Break`], where the server chooses the target -- including
    /// [`tick_mining`](Self::tick_mining), which since §8.3 asks for a break
    /// rather than naming one, so nothing on the game's own path reaches here.
    #[allow(dead_code)]
    fn break_at(&mut self, block: [i32; 3]) -> ChunkCoord {
        let who = self.me_id;
        let cc = self.server_mut().break_at_as(who, block);
        // The server does not journal a screen-close for a targeted break --
        // `CloseIfAt` is `Action::Break`'s doing, and this bypasses it.
        self.server_mut().close_if_at(block);
        // The host has to run for the effects to reach this client's queue.
        if let Some(host) = self.host.as_mut() {
            host.advance(1, &self.cfg);
        }
        self.sync();
        cc
    }

    /// Take everything the server has done and apply it to this client:
    /// world edits onto the replica, block entities onto the replica, screens
    /// onto the screen state. Returns the chunks whose geometry is now stale.
    ///
    /// **The dirty chunks are derived, not received.** Each edit is applied to
    /// the client's own `World`, and the [`ChunkCoord`] its own `set_block`
    /// hands back is what needs re-meshing. A remote client would have to work
    /// it out exactly this way, because the server has no idea how its chunks
    /// are laid out on screen.
    fn sync(&mut self) -> Vec<ChunkCoord> {
        // Everything the server has said since last time. In-process the host
        // put it there a moment ago; over a socket it arrived on a thread. This
        // side cannot tell, and that is the point.
        let mut effects = Vec::new();
        if let Some(link) = self.link.as_mut() {
            for msg in link.poll() {
                match msg {
                    ServerMessage::Effects(v) => effects.extend(v),
                    // The id is read once, at the join. A second welcome would
                    // mean a reconnection, which nothing does yet.
                    ServerMessage::Welcome { .. } | ServerMessage::Tick(_) => {}
                }
            }
        }
        self.apply_effects(effects)
    }

    /// Rebuild the replica from the server's full state (§8.3's join
    /// handshake), rather than from a delta there is no way to compute.
    fn resync(&mut self) {
        // The join handshake, for this client specifically. `snapshot()` would
        // ask for the *local* client's, and a host that has gone headless has
        // none -- the whole point of the id coming from `Welcome`.
        let who = self.me_id;
        let snapshot = self.server().snapshot_for(who);
        self.apply_effects(snapshot);
    }

    /// The one place effects are applied, whether they arrived as a stream or
    /// as a snapshot.
    fn apply_effects(&mut self, effects: Vec<Effect>) -> Vec<ChunkCoord> {
        let mut dirty = Vec::new();
        for e in effects {
            match e {
                Effect::Edit { pos, block } => {
                    let cc =
                        Arc::make_mut(&mut self.world).set_block(pos[0], pos[1], pos[2], block);
                    if !dirty.contains(&cc) {
                        dirty.push(cc);
                    }
                }
                Effect::BlockEntity { pos, furnace } => {
                    let world = Arc::make_mut(&mut self.world);
                    match furnace {
                        Some(f) => world.put_furnace(pos, f),
                        None => {
                            world.remove_block_entity(pos);
                        }
                    }
                }
                Effect::Open(Screen::Bench) => {
                    self.open_furnace = None;
                    self.inventory_open = true;
                }
                Effect::Open(Screen::Furnace(pos)) => {
                    self.open_furnace = Some(pos);
                    self.inventory_open = true;
                }
                Effect::CloseIfAt(pos) => {
                    if self.open_furnace == Some(pos) {
                        self.open_furnace = None;
                        self.inventory_open = false;
                    }
                }
                // Block 2.11 replicates where other players are; this client has
                // nowhere to put them yet. **Deliberately a no-op, and
                // deliberately written out** rather than caught by a wildcard:
                // a `_ =>` arm here would mean the next effect anyone adds is
                // silently dropped by the replica, which is the kind of bug that
                // shows up as a desync weeks later.
                //
                // Drawing another player needs a model and an interpolator, and
                // interpolating a remote pose is exactly what design §8.4 says
                // not to build before there is real latency to build it against.
                Effect::PlayerMoved {
                    who,
                    pos,
                    yaw,
                    pitch,
                } => self.others.moved(who, pos, yaw, pitch),
                Effect::PlayerGone(who) => self.others.gone(who),
                // What somebody asked to be drawn in. Written out rather than
                // wildcarded, like every other arm here: an effect the replica
                // silently drops is a bug that surfaces weeks later.
                Effect::PlayerShirt { who, shirt } => self.others.wears(who, shirt),
                // This client's own items. Ignored here for the same reason
                // `SelfState` is: `Game` still owns the `Server` in this
                // process and reads the authoritative player straight out of
                // it, so a copy of what it already has is nothing to apply.
                //
                // It becomes the *only* way a client knows what it is carrying
                // the moment `Game` talks over a `Link` — which is the next
                // change on this branch, and the reason the message exists now.
                // The two ways this client learns about **itself**, and since
                // block 2.12b part B they are the only two. `Game` used to read
                // its player straight out of the `Server` it owns, which is a
                // field access standing in for a round trip; a connected client
                // has no such field.
                //
                // `SelfItems` before `SelfState` in this arm order is not
                // significant -- they are different fields of the same player --
                // but they are written out rather than wildcarded, because an
                // effect silently dropped by the replica is the kind of bug that
                // shows up as a desync weeks later.
                Effect::SelfItems(items) => items.apply_to(self.me.player_mut()),
                Effect::SelfState { seq, state } => {
                    let blocks = self.terrain();
                    self.me
                        .reconcile(seq, state, Arc::make_mut(&mut self.world), blocks);
                }
            }
        }
        dirty
    }

    /// The hotbar reduced to what drawing needs: a swatch colour and a count
    /// per slot, plus which is held.
    ///
    /// The reduction lives here, not in `cubara-render`: that crate does not
    /// know what an item is, nor that slots 0..9 of a 36-slot array are the
    /// hotbar (Rule 3). Returns `None` before assets are set, so the HUD simply
    /// does not draw rather than drawing nine empty boxes.
    ///
    /// Colours come from `swatch_color`, the same deterministic name hash a
    /// block with no texture file already uses -- so a stone block in the world
    /// and a stone item in the hand read as the same material. Real item icons
    /// are art that does not exist yet.
    pub fn hotbar_slots(&self) -> Option<[Option<HotbarSlot>; HOTBAR_WIDTH]> {
        let items = self.assets.as_ref().map(|a| &a.items)?;
        let inv = &self.me.player().inventory;
        let mut out = [None; HOTBAR_WIDTH];
        for (i, out_slot) in out.iter_mut().enumerate() {
            let Some(stack) = inv.slot(i) else { continue };
            let Some(name) = items.name_of(stack.item()) else {
                continue;
            };
            *out_slot = Some(HotbarSlot {
                color: swatch_color(name),
                count: stack.count(),
            });
        }
        Some(out)
    }

    /// The furnace screen currently open, if any -- read from the **replica**.
    ///
    /// Which is the interesting part: a furnace smelting away updates the screen
    /// because the server journals a `BlockEntity` effect every tick it changes
    /// and the client applies it, not because the client is looking at the
    /// server's furnace. That is the same path a remote client would use.
    pub fn open_furnace(&self) -> Option<Furnace> {
        let pos = self.open_furnace?;
        self.world.furnace_at(pos).copied()
    }

    /// Which ids the terrain is made of — delegated to the server, which owns
    /// the registries (§8.1).
    ///
    /// Whether the inventory screen is open.
    pub fn inventory_open(&self) -> bool {
        self.inventory_open
    }

    /// Open or close the screen. Closing is **refused** while the crafting grid
    /// cannot empty into the inventory (2.2b's `close`), so items in the grid
    /// are never eaten by walking away from them.
    pub fn toggle_inventory(&mut self) {
        if !self.inventory_open {
            self.inventory_open = true;
            return;
        }
        // A furnace screen has no crafting grid to empty -- its slots belong to
        // the block and stay in it. Only the cursor needs somewhere to go.
        if self.open_furnace.take().is_some() {
            self.inventory_open = false;
            // The cursor's contents go back to the inventory, and that is a rule
            // about items rather than about a window -- so the server applies
            // it. A furnace screen has no grid of its own, so `CloseScreen` has
            // nothing else to return.
            self.act(Action::CloseScreen);
            return;
        }
        let Some(items) = self.assets.as_ref().map(|a| &a.items) else {
            self.inventory_open = false;
            return;
        };
        // What happens to a half-finished craft is a rule about items, so the
        // server applies it and answers whether everything fitted. `false` keeps
        // the screen open, which is `Crafting::close`'s own rule: refusing to
        // close is more honest than eating what does not fit.
        let items = items.clone();
        let player = self.me.player_mut();
        let closed = player.crafting.close(&mut player.inventory, &items);
        if closed {
            player.crafting.set_width(2);
            self.act(Action::CloseScreen);
            self.inventory_open = false;
        } else {
            log::debug!("inventory full: the crafting grid still holds items, staying open");
        }
    }

    /// Route a click on the open screen. `(x, y)` is in window pixels.
    ///
    /// The layout that decides *which* slot lives in `cubara-render` and is the
    /// same one the screen is drawn from, so a click cannot land on a slot other
    /// than the one under the cursor.
    pub fn click_panel(&mut self, x: f32, y: f32, right: bool, width: u32, height: u32) {
        let (Some(items), Some(book)) = (
            self.assets.as_ref().map(|a| &a.items),
            self.assets.as_ref().map(|a| &a.recipes),
        ) else {
            return;
        };
        let panel = match self.open_furnace {
            Some(_) => InventoryPanel::layout_furnace(width, height),
            None => InventoryPanel::layout(width, height, self.me.player().crafting.width()),
        };
        let Some((kind, index)) = panel.hit(x, y) else {
            return;
        };
        if let Some(pos) = self.open_furnace {
            self.click_furnace(pos, kind, index);
            return;
        }
        let slot = match kind {
            PanelSlotKind::Inventory => SlotRef::Inventory(index),
            PanelSlotKind::Grid => SlotRef::Grid(index),
            PanelSlotKind::Result => SlotRef::Result,
            // A furnace slot cannot appear in the crafting layout; ignoring it
            // is the safe branch rather than mapping it to a grid cell.
            PanelSlotKind::Fuel => return,
        };
        // **Asked for, not done.** Moving an item between an inventory and a
        // grid changes world state, and a client that did it locally would be a
        // client that could conjure items (§3.4). The rules still live in
        // `Crafting::click`; what changed is who runs them.
        // **Predicted, then sent.** Run with the client's own definitions, which
        // the fingerprint check has already proved match the server's, so the
        // screen responds on the frame it was clicked. `SelfItems` replaces the
        // result a tick later; if the server disagreed, the disagreement is what
        // shows rather than a silent divergence.
        let (items, book) = (items.clone(), book.clone());
        let player = self.me.player_mut();
        let (crafting, inventory) = (&mut player.crafting, &mut player.inventory);
        crafting.click(slot, right, inventory, &items, &book);
        self.act(Action::ClickSlot { slot, right });
    }

    /// Route a click on the open furnace's screen.
    ///
    /// The inventory half is the player's own and stays here; the three furnace
    /// slots are world state, so they go to the server as an
    /// [`Action::ClickFurnace`] and come back as a `BlockEntity` effect. That
    /// translation -- `PanelSlotKind` (where it is drawn) to [`FurnaceSlot`]
    /// (what it is) -- is the client's job, because a server that spoke in panel
    /// layouts would be a server that knew what a screen looks like.
    fn click_furnace(&mut self, pos: [i32; 3], kind: PanelSlotKind, index: usize) {
        let Some(items) = self.assets.as_ref().map(|a| &a.items) else {
            return;
        };
        if kind == PanelSlotKind::Inventory {
            // `click_inventory_only` is `click`'s inventory case reached without
            // a grid, and a furnace screen has no grid -- so the same action
            // carries it. Left-click only, which is what that method already
            // restricted itself to.
            let _ = items;
            self.act(Action::ClickSlot {
                slot: SlotRef::Inventory(index),
                right: false,
            });
            return;
        }
        let slot = match kind {
            PanelSlotKind::Grid => FurnaceSlot::Input,
            PanelSlotKind::Fuel => FurnaceSlot::Fuel,
            PanelSlotKind::Result => FurnaceSlot::Output,
            PanelSlotKind::Inventory => return,
        };
        self.act(Action::ClickFurnace { pos, slot });
        // The furnace's new contents come back as a `BlockEntity` effect, and
        // the screen is drawn from the replica -- so without this the click
        // would appear to do nothing until the next tick.
        self.sync();
    }

    /// The screen's layout and contents, or `None` when it is closed.
    ///
    /// Walks the same layout the renderer draws from, filling one entry per
    /// slot -- which is what keeps `contents` and `slots()` in step without
    /// either side knowing the other's ordering.
    pub fn panel_view(
        &self,
        width: u32,
        height: u32,
    ) -> Option<(InventoryPanel, Vec<Option<HotbarSlot>>, Option<HotbarSlot>)> {
        let (panel, stacks) = self.panel_stacks(width, height)?;
        let items = self.assets.as_ref().map(|a| &a.items)?;
        let swatch = |(id, count): (cubara_voxel::ItemId, u8)| {
            items.name_of(id).map(|name| HotbarSlot {
                color: swatch_color(name),
                count,
            })
        };
        let contents = stacks.into_iter().map(|s| s.and_then(swatch)).collect();
        let held = self
            .me
            .player()
            .crafting
            .held()
            .and_then(|s| swatch((s.item(), s.count())));
        Some((panel, contents, held))
    }

    /// The open furnace's flame and progress, as fractions, or `None` when no
    /// furnace screen is open.
    ///
    /// **The flame is measured against the fuel in the slot**, because the
    /// furnace does not remember how long the item already alight burned for --
    /// adding that would change world state and the save format for a meter.
    /// With the slot empty the item alight is the last of it, and the flame
    /// shows full until it goes out: a meter that is briefly generous rather
    /// than one that needs a new field in every save.
    pub fn furnace_gauges(&self) -> Option<cubara_render::FurnaceGauges> {
        let f = self.open_furnace()?;
        let assets = self.assets.as_ref()?;
        let burn = match f.burning {
            0 => 0.0,
            left => {
                let full = f
                    .fuel
                    .and_then(|(id, _)| assets.items.burn_ticks(id))
                    .unwrap_or(left)
                    .max(left);
                left as f32 / full as f32
            }
        };
        let progress = f
            .input
            .and_then(|(id, _)| assets.smelting.for_input(id))
            .map(|r| f.progress as f32 / r.ticks.max(1) as f32)
            .unwrap_or(0.0);
        Some(cubara_render::FurnaceGauges { burn, progress })
    }

    /// The name to show for whatever is under the cursor on the open screen,
    /// or `None` over an empty slot, open space, or while carrying something
    /// (the carried stack sits exactly where the label would).
    pub fn hovered_item_name(&self, x: f32, y: f32, width: u32, height: u32) -> Option<String> {
        if self.me.player().crafting.held().is_some() {
            return None;
        }
        let (panel, stacks) = self.panel_stacks(width, height)?;
        let (kind, index) = panel.hit(x, y)?;
        let at = panel
            .slots()
            .iter()
            .position(|s| s.kind == kind && s.index == index)?;
        let (id, _) = stacks[at]?;
        let items = self.assets.as_ref().map(|a| &a.items)?;
        items.name_of(id).map(display_name)
    }

    /// The open screen's layout, and what each of its slots holds, in
    /// `panel.slots()` order.
    ///
    /// **The one answer to "what is in that slot"**, which drawing the screen
    /// and naming what is under the cursor both read. Two copies of this match
    /// would be two chances to label a slot with something other than what is
    /// drawn in it.
    fn panel_stacks(&self, width: u32, height: u32) -> Option<(InventoryPanel, Vec<SlotStack>)> {
        if !self.inventory_open {
            return None;
        }
        let items = self.assets.as_ref().map(|a| &a.items)?;
        let crafting = &self.me.player().crafting;
        let book = self.assets.as_ref().map(|a| &a.recipes);
        let furnace = self.open_furnace();
        let panel = match self.open_furnace {
            Some(_) => InventoryPanel::layout_furnace(width, height),
            None => InventoryPanel::layout(width, height, crafting.width()),
        };
        let of = |stack: cubara_voxel::ItemStack| (stack.item(), stack.count());

        // A furnace slot holds `(id, count)` rather than an `ItemStack`, since
        // nothing in a furnace has durability.
        let stacks = panel
            .slots()
            .iter()
            .map(|s| match (s.kind, furnace) {
                (PanelSlotKind::Inventory, _) => self.me.player().inventory.slot(s.index).map(of),
                (PanelSlotKind::Grid, Some(f)) => f.input,
                (PanelSlotKind::Fuel, Some(f)) => f.fuel,
                (PanelSlotKind::Result, Some(f)) => f.output,
                (PanelSlotKind::Grid, None) => crafting.cell(s.index).map(of),
                (PanelSlotKind::Result, None) => {
                    book.and_then(|b| crafting.result(b, items)).map(of)
                }
                // Only a furnace layout produces a fuel slot.
                (PanelSlotKind::Fuel, None) => None,
            })
            .collect();
        Some((panel, stacks))
    }

    /// Which hotbar slot is held, for the renderer.
    pub fn selected_hotbar_slot(&self) -> u8 {
        self.me.player().inventory.selected_slot()
    }

    /// Turn the mouse wheel by `lines` notches: positive is away from you
    /// (winit's "up"), which moves the selection **left**; toward you moves it
    /// right. Wraps at both ends.
    ///
    /// Fractions are kept rather than rounded: a touchpad reports a flick as
    /// many small deltas, and rounding each one would either never move or
    /// move once per event.
    pub fn scroll_hotbar(&mut self, lines: f32) {
        self.scroll_pending += lines;
        let steps = self.scroll_pending.trunc();
        if steps == 0.0 {
            return;
        }
        self.scroll_pending -= steps;
        let next = scrolled_slot(self.selected_hotbar_slot(), -(steps as i32));
        self.select_hotbar(next);
    }

    /// Select a hotbar slot (number keys 1-9, passed as 0-8).
    pub fn select_hotbar(&mut self, index: u8) {
        // Predicted, then sent -- a hotbar that only lit up after a round trip
        // would feel broken in singleplayer and worse over a wire. The same
        // predict-and-be-corrected shape movement already uses.
        self.me.player_mut().inventory.select(index);
        self.act(Action::SelectHotbar(index));
    }
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

/// The `PlayerId` a server named in its `Welcome`, if it has sent one.
///
/// Peeks at what is already queued rather than blocking: in-process the welcome
/// is there before `attach` returns, and over a socket a client that blocked
/// here would freeze its own window waiting for a packet.
fn welcome_id(link: &mut Link<ClientMessage, ServerMessage>) -> Option<PlayerId> {
    link.poll().into_iter().find_map(|m| match m {
        ServerMessage::Welcome { you, .. } => Some(you),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    /// A player is drawn in the colour **they** announced, not one this client
    /// picked for them.
    ///
    /// The whole arrangement: each machine hard-codes its own shirt, says so
    /// when it joins, and the server relays it without interpreting it. So two
    /// people looking at the same third person see the same colour, and nobody
    /// has to agree about an ordering.
    #[test]
    fn a_player_is_drawn_in_the_colour_they_announced() {
        let mut game = Game::new();
        let pos = cubara_voxel::FixedVec3::from_blocks(3, 40, 0);

        game.others
            .moved(PlayerId(7), pos, Angle::ZERO, Angle::ZERO);
        assert_eq!(
            game.other_players()[0].shirt,
            UNKNOWN_SHIRT,
            "somebody who has not said what they wear should be drawn drab, \
             not in a guess that could be somebody else's colour"
        );

        game.others.wears(PlayerId(7), [204, 41, 41]);
        let drawn = game.other_players();
        assert_eq!(drawn.len(), 1);
        let [r, g, b] = drawn[0].shirt;
        assert!(
            (r - 0.8).abs() < 0.01 && g < 0.2 && b < 0.2,
            "announced red came out as {:?}",
            drawn[0].shirt
        );
    }

    /// A shirt that arrives **before** the pose is not lost.
    ///
    /// They normally come in one batch, but nothing in the protocol promises an
    /// order, and a colour dropped because it was early would show as somebody
    /// permanently grey.
    #[test]
    fn a_shirt_that_arrives_before_the_pose_still_lands() {
        let mut game = Game::new();
        game.others.wears(PlayerId(4), [51, 179, 61]);
        game.others.moved(
            PlayerId(4),
            cubara_voxel::FixedVec3::from_blocks(3, 40, 0),
            Angle::ZERO,
            Angle::ZERO,
        );

        let [r, g, b] = game.other_players()[0].shirt;
        assert!(
            g > 0.6 && r < 0.3 && b < 0.3,
            "a shirt announced before the first pose was lost: got {:?}",
            [r, g, b]
        );
    }

    /// This build wears the colour its platform was given, and the three are
    /// tellable apart on a screen.
    ///
    /// Only one of the three exists in any one build, so the separation is
    /// checked against the literals. Two shirts that are nearly the same are
    /// worse than one colour, because the difference looks deliberate and is
    /// not readable.
    #[test]
    fn the_three_machines_wear_three_tellable_colours() {
        let linux = [204u8, 41, 41];
        let windows = [235u8, 209, 41];
        let mac = [51u8, 179, 61];

        let expected = if cfg!(target_os = "linux") {
            linux
        } else if cfg!(target_os = "windows") {
            windows
        } else if cfg!(target_os = "macos") {
            mac
        } else {
            [140, 140, 140]
        };
        assert_eq!(MY_SHIRT, expected, "this build wears the wrong colour");

        let apart = |a: [u8; 3], b: [u8; 3]| {
            let d = |i: usize| (a[i] as f32 - b[i] as f32).powi(2);
            (d(0) + d(1) + d(2)).sqrt()
        };
        for (a, b, what) in [
            (linux, windows, "Linux and Windows"),
            (linux, mac, "Linux and macOS"),
            (windows, mac, "Windows and macOS"),
        ] {
            assert!(
                apart(a, b) > 80.0,
                "{what} are too close to tell apart on a screen"
            );
        }
    }

    /// The figure the renderer draws is the size of the body the simulation
    /// collides with.
    ///
    /// `cubara-render` cannot import these — it depends on neither the sim nor
    /// the world crate, and taking a dependency so a placeholder can read three
    /// numbers would be the wrong trade. So they are written out in both places
    /// and tied together *here*, in the one crate that sees both.
    ///
    /// Without this, changing the player's height in `physics.rs` leaves every
    /// other player in the world drawn at the old size, and the only thing that
    /// would notice is somebody looking at a screenshot.
    #[test]
    fn the_drawn_figure_matches_the_simulated_body() {
        use cubara_sim::physics::{EYE_HEIGHT, HALF_WIDTH, HEIGHT};

        // Within one fixed-point step. The simulation stores these as `Fixed`,
        // so 1.8 comes back as 1.7999878 -- a float constant can only agree
        // with it to the nearest representable value, and demanding exactness
        // would fail on a difference nothing can see.
        let step = 2.0 / 65536.0;
        let agree = |drawn: f32, simulated: f32, what: &str| {
            assert!(
                (drawn - simulated).abs() < step,
                "{what}: drawn {drawn}, simulated {simulated}"
            );
        };

        agree(
            cubara_render::figure::PLAYER_HEIGHT,
            HEIGHT.to_f32(),
            "the figure is drawn at a different height than the body collides at",
        );
        agree(
            cubara_render::figure::PLAYER_WIDTH,
            HALF_WIDTH.to_f32() * 2.0,
            "the figure is drawn at a different width than the body collides at",
        );
        agree(
            cubara_render::figure::EYE_HEIGHT,
            EYE_HEIGHT.to_f32(),
            "the figure's eye is not where the simulation puts it",
        );
    }

    /// The client's own player keeps up with the server's, and does so through
    /// **messages only**.
    ///
    /// `Game` used to read `server.sim.player(server.local)` — a field access
    /// standing in for a round trip, and the one shortcut a connected client
    /// cannot take. Since block 2.12b part B the client keeps its own player,
    /// corrected by `Effect::SelfState` and `Effect::SelfItems` every tick.
    ///
    /// If either of those stops being published, or stops being applied, this
    /// is what notices — every other test here reads the *server's* player and
    /// would go on passing with the client frozen at spawn.
    #[test]
    fn the_clients_own_player_tracks_the_server_through_messages_alone() {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );

        let start = *game.me.player();
        game.forward = true;
        for _ in 0..90 {
            game.advance(TICK_DT);
        }

        let mine = game.me.player();
        let theirs = game.host_player();

        assert_ne!(
            mine.pos, start.pos,
            "the client's own player never moved, so it is not being corrected at all"
        );
        assert_eq!(
            mine.pos, theirs.pos,
            "the client and the server disagree about where the client is"
        );
        assert_eq!(
            mine.health, theirs.health,
            "health did not reach the client"
        );
    }

    /// Selecting a hotbar slot reaches the client's copy the same way.
    ///
    /// Inventory is *replicated*, not predicted — `PlayerState` deliberately
    /// leaves it out, because it changes through deliberate acts rather than
    /// through physics. So this travels by `SelfItems` and nothing else.
    #[test]
    fn a_hotbar_selection_reaches_the_clients_own_copy() {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        game.advance(TICK_DT);

        assert_eq!(game.host_player().inventory.selected_slot(), 0);
        game.select_hotbar(3);
        game.settle();
        game.advance(TICK_DT);

        assert_eq!(
            game.host_player().inventory.selected_slot(),
            3,
            "the hotbar selection never reached the client's own copy"
        );
    }

    use super::*;
    use cubara_voxel::FixedVec3;
    use cubara_voxel::{BlockId, ItemStack, ItemState};

    #[test]
    fn breaking_clears_the_targeted_block() {
        // No GPU involved — this is why gameplay does not belong on the renderer.
        let mut game = Game::new();
        // Look straight down from above the terrain.
        *game.host_player_mut() = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 60.0, 0.5]),
            Angle::ZERO,
            Angle::from_radians(-1.5),
        );
        let hit = game
            .world()
            .raycast([0.5, 60.0, 0.5], [0.0, -1.0, 0.0], 100.0, game.terrain())
            .expect("ground below");

        // Out of reach from 60 blocks up: nothing changes.
        assert_eq!(game.break_block(), None);
        assert!(game
            .world()
            .is_solid_at(hit.block[0], hit.block[1], hit.block[2], game.terrain()));
    }

    #[test]
    fn editing_within_reach_marks_a_chunk_dirty() {
        let mut game = Game::new();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        // Stand just above the surface, looking down — now it is within reach.
        let eye = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.5, 0.5]);
        *game.host_player_mut() = Player::new(eye, Angle::ZERO, Angle::from_radians(-1.5));

        let dirty = game.break_block();
        game.settle();
        assert!(
            !game.world().is_solid_at(
                ground.block[0],
                ground.block[1],
                ground.block[2],
                game.terrain()
            ),
            "the targeted block is now air"
        );
        let b = ground.block;
        assert_eq!(
            dirty,
            Some(ChunkCoord::from_world_pos([
                b[0] as f32,
                b[1] as f32,
                b[2] as f32
            ])),
            "the dirty chunk is the one containing the broken block"
        );
    }

    /// A game with the real registries wired in, standing just above the
    /// ground and looking down -- the fixture every break/place test needs.
    /// Uses the shipped `assets/`, not a synthetic registry: a block whose
    /// name has no matching item file is exactly the failure these tests
    /// should catch, and a fixture would hide it.
    fn game_looking_at_ground() -> (Game, [i32; 3]) {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let eye = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.5, 0.5]);
        *game.host_player_mut() = Player::new(eye, Angle::ZERO, Angle::from_radians(-1.5));
        (game, ground.block)
    }

    #[test]
    fn breaking_a_block_puts_its_item_in_the_inventory() {
        // Soil rather than whatever the surface is: grass drops soil, and this
        // test is about the same-name rule, not about grass.
        let (mut game, _) = game_looking_at_ground();
        let block = stand_over(&mut game, "cubara:soil");
        let terrain = game.server().terrain.expect("assets are set");
        let broken = game.world().block_at(block[0], block[1], block[2], terrain);
        let name = game
            .server_mut()
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(broken)
            .expect("the block has a name")
            .to_string();
        assert_eq!(name, "cubara:soil");

        game.break_block();

        game.settle();
        let items = game.server().items.as_ref().unwrap().clone();
        let stack = game
            .host_player_mut()
            .inventory
            .slot(0)
            .expect("slot 0 holds the drop");
        assert_eq!(
            items.name_of(stack.item()),
            Some(name.as_str()),
            "breaking {name} must yield the item of the same name"
        );
        assert_eq!(stack.count(), 1);
    }

    #[test]
    fn scrolling_toward_you_moves_right_and_wraps() {
        assert_eq!(scrolled_slot(0, 1), 1);
        assert_eq!(
            scrolled_slot(0, -1),
            HOTBAR_WIDTH as u8 - 1,
            "left of the first wraps"
        );
        assert_eq!(
            scrolled_slot(HOTBAR_WIDTH as u8 - 1, 1),
            0,
            "right of the last wraps"
        );
        assert_eq!(scrolled_slot(3, -2), 1);
    }

    #[test]
    fn the_wheel_selects_the_hotbar_one_notch_per_slot() {
        let mut game = game_with_assets();
        assert_eq!(game.selected_hotbar_slot(), 0);
        // One notch toward you (winit reports it as negative).
        game.scroll_hotbar(-1.0);
        assert_eq!(game.selected_hotbar_slot(), 1);
        game.scroll_hotbar(1.0);
        game.scroll_hotbar(1.0);
        assert_eq!(game.selected_hotbar_slot(), HOTBAR_WIDTH as u8 - 1);
        // The server hears about it, not only the client's own copy.
        game.settle();
        assert_eq!(
            game.host_player().inventory.selected_slot(),
            HOTBAR_WIDTH as u8 - 1
        );
    }

    #[test]
    fn a_touchpad_scroll_steps_only_once_it_adds_up_to_a_notch() {
        // Quarters, so the arithmetic is exact and the test is about the rule.
        let mut game = game_with_assets();
        game.scroll_hotbar(-0.75);
        assert_eq!(game.selected_hotbar_slot(), 0, "3/4 of a notch moved it");
        game.scroll_hotbar(-0.75);
        assert_eq!(game.selected_hotbar_slot(), 1, "1.5 notches did not");
        // The half left over counts toward the next one.
        game.scroll_hotbar(-0.5);
        assert_eq!(
            game.selected_hotbar_slot(),
            2,
            "the remainder was thrown away"
        );
    }

    /// The owner asked for it: digging grass gives soil, not grass.
    #[test]
    fn breaking_grass_yields_soil() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:grass");
        game.break_block();
        game.settle();
        assert_eq!(count_of(&game, "cubara:soil"), 1, "grass did not drop soil");
        assert_eq!(count_of(&game, "cubara:grass"), 0, "grass dropped itself");
    }

    #[test]
    fn placing_consumes_exactly_one_of_the_held_stack() {
        let (mut game, _) = game_looking_at_ground();
        // The terrain's own stone, resolved by name in both registries, so
        // this test does not depend on which materials happen to ship -- only
        // on the block and its item sharing a name, which is the drop policy.
        let stone = game.server().terrain.unwrap().stone;
        let stone_name = game
            .server()
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(stone)
            .expect("terrain stone has a name")
            .to_string();
        let items = game.server().items.as_ref().unwrap().clone();
        let held = items
            .id_of(&stone_name)
            .expect("every shipped block needs an item of the same name to be placeable");
        let stack = items.new_stack(held, 5).unwrap();
        game.host_player_mut().inventory.add(stack, &items);
        game.select_hotbar(0);
        game.settle();
        game.place_block();
        game.settle();

        game.settle();
        assert_eq!(
            game.host_player_mut().inventory.slot(0).map(|s| s.count()),
            Some(4),
            "placing spends exactly one"
        );
    }

    #[test]
    fn placing_with_an_empty_hand_changes_nothing() {
        let (mut game, _) = game_looking_at_ground();
        let before = game.world().edit_count();
        game.place_block();
        game.settle();
        assert_eq!(game.world().edit_count(), before);
    }

    #[test]
    fn placing_an_item_with_no_block_consumes_nothing() {
        // A stick is not a block. The click must do nothing *and* not quietly
        // spend the stick -- an action that fails silently should not also
        // cost you something.
        let (mut game, _) = game_looking_at_ground();
        let items = game.server().items.as_ref().unwrap().clone();
        let stick = items
            .id_of("cubara:stick")
            .expect("assets/items has a stick");
        game.host_player_mut()
            .inventory
            .add(items.new_stack(stick, 3).unwrap(), &items);
        game.select_hotbar(0);
        game.settle();
        let before = game.world().edit_count();
        game.place_block();
        game.settle();
        assert_eq!(game.world().edit_count(), before, "nothing was placed");
        assert_eq!(
            game.host_player_mut().inventory.slot(0).map(|s| s.count()),
            Some(3),
            "and nothing was consumed"
        );
    }

    #[test]
    fn breaking_with_a_full_inventory_still_breaks_the_block() {
        // The drop is lost -- there are no dropped-item entities until ECS
        // (2.5). Recorded behaviour, not a silent bug: what must not happen is
        // the block refusing to break, which would read as the game being stuck.
        let (mut game, block) = game_looking_at_ground();
        let items = game.server().items.as_ref().unwrap().clone();
        let filler = items.id_of("cubara:stick").unwrap();
        for _ in 0..cubara_sim::SLOT_COUNT {
            game.host_player_mut()
                .inventory
                .add(items.new_stack(filler, 64).unwrap(), &items);
        }

        game.break_block();

        game.settle();
        assert!(
            !game
                .world()
                .is_solid_at(block[0], block[1], block[2], game.terrain()),
            "the block breaks even when the drop has nowhere to go"
        );
    }

    #[test]
    fn number_keys_select_hotbar_slots_on_press_only() {
        let mut game = Game::new();
        assert!(game.key_input(KeyCode::Digit4, true));
        game.settle();
        assert_eq!(game.host_player_mut().inventory.selected_slot(), 3);

        // Releasing must not reselect -- otherwise every key-up would snap the
        // selection back to whichever number was let go of last.
        game.key_input(KeyCode::Digit1, true);
        game.key_input(KeyCode::Digit4, false);
        game.settle();
        assert_eq!(game.host_player_mut().inventory.selected_slot(), 0);
    }

    /// Place a bench right where the player is looking, and aim at it.
    fn game_facing_a_bench() -> (Game, [i32; 3]) {
        let (mut game, ground) = game_looking_at_ground();
        let bench = game
            .server_mut()
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:crafting_bench")
            .expect("assets/blocks defines the bench");
        game.server_mut().set_block(ground, bench);
        game.settle();
        (game, ground)
    }

    #[test]
    fn right_clicking_a_bench_opens_the_three_by_three_grid() {
        let (mut game, _) = game_facing_a_bench();
        let items = game.server().items.as_ref().unwrap().clone();
        // Holding something placeable, to prove interaction wins over placing.
        game.host_player_mut().inventory.add(
            items
                .new_stack(items.id_of("cubara:stone").unwrap(), 5)
                .unwrap(),
            &items,
        );
        game.select_hotbar(0);
        game.settle();
        let before = game.world().edit_count();

        game.place_block();
        game.settle();
        assert_eq!(game.world().edit_count(), before, "the world is unchanged");
        assert!(game.inventory_open(), "the screen opened");
        assert_eq!(game.host_player_mut().crafting.width(), 3, "at bench size");
    }

    #[test]
    fn right_clicking_anything_else_still_places() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.server().items.as_ref().unwrap().clone();
        game.host_player_mut().inventory.add(
            items
                .new_stack(items.id_of("cubara:stone").unwrap(), 5)
                .unwrap(),
            &items,
        );
        game.select_hotbar(0);
        game.settle();
        game.place_block();
        game.settle();
        assert!(!game.inventory_open(), "and no screen opened");
    }

    /// Put one `item` into grid `cell`, the way a player would: pick it up
    /// from a scratch inventory, then put it down. Going through `click`
    /// rather than reaching into the cells keeps these tests honest about the
    /// path the real game takes.
    fn load_cell(game: &mut Game, cell: usize, item: cubara_voxel::ItemId, count: u8) {
        let items = game.server().items.as_ref().unwrap().clone();
        let book = game.server().recipes.as_ref().unwrap().clone();
        let mut scratch = cubara_sim::Inventory::new();
        scratch.add(items.new_stack(item, count).unwrap(), &items);
        let mut c = game.host_player_mut().crafting;
        c.click(
            cubara_sim::SlotRef::Inventory(0),
            false,
            &mut scratch,
            &items,
            &book,
        );
        c.click(
            cubara_sim::SlotRef::Grid(cell),
            false,
            &mut scratch,
            &items,
            &book,
        );
        game.host_player_mut().crafting = c;
    }

    #[test]
    fn closing_a_bench_returns_its_outer_cells_and_narrows() {
        // Cell 8 is the bottom-right of a 3x3 -- unreachable at width 2.
        // Narrowing must not strand it. `Crafting::close` empties all nine
        // cells regardless of width deliberately, and this is the test that
        // says why that mattered.
        let (mut game, _) = game_facing_a_bench();
        game.place_block();
        game.settle();
        assert_eq!(game.host_player_mut().crafting.width(), 3);

        let stone = game
            .server_mut()
            .items
            .as_ref()
            .unwrap()
            .id_of("cubara:stone")
            .unwrap();
        load_cell(&mut game, 8, stone, 4);
        assert!(
            game.host_player_mut().crafting.cell(8).is_some(),
            "cell 8 is loaded"
        );

        game.toggle_inventory();

        game.settle();
        assert!(!game.inventory_open(), "it closed");
        assert_eq!(game.host_player_mut().crafting.width(), 2, "and narrowed");
        assert!(
            game.host_player_mut().crafting.cell(8).is_none(),
            "the outer cell was emptied, not stranded"
        );
        assert!(
            game.host_player_mut()
                .inventory
                .slots()
                .flatten()
                .any(|s| s.item() == stone),
            "and its contents came back to the inventory"
        );
    }

    #[test]
    fn a_wooden_pick_can_be_crafted_at_a_bench() {
        // The 3x3 recipe that is unreachable without this issue -- and the
        // first rung of the ladder that needs a bench at all.
        let (mut game, _) = game_facing_a_bench();
        game.place_block();
        game.settle();
        let (plank, stick) = {
            let items = game.server().items.as_ref().unwrap().clone();
            (
                items.id_of("cubara:plank").unwrap(),
                items.id_of("cubara:stick").unwrap(),
            )
        };
        // PPP / .S. / .S.
        for (cell, item) in [(0, plank), (1, plank), (2, plank), (4, stick), (7, stick)] {
            load_cell(&mut game, cell, item, 1);
        }

        let items = game.server().items.as_ref().unwrap().clone();
        let book = game.server().recipes.as_ref().unwrap().clone();
        let made = game
            .host_player()
            .crafting
            .result(&book, &items)
            .expect("the grid makes something");
        assert_eq!(
            items.name_of(made.item()),
            Some("cubara:wooden_pick"),
            "a bench makes the wooden pick"
        );
    }

    #[test]
    fn every_shipped_block_has_an_item_of_the_same_name() {
        // The drop policy is "block name -> item of the same name"
        // (PHASE2_ARCHITECTURE.md 4.1), so a block with no matching item file
        // silently drops nothing. That is not a crash, not a warning, and not
        // visible until someone mines it and wonders why their inventory is
        // empty -- which is exactly how this was found: three of the three
        // blocks the world is made of had no items.
        //
        // Until 2.4 replaces the policy with real `drops:` tables, this is what
        // keeps the two asset directories in step.
        let registry = cubara_render::load_registry();
        let items = load_item_registry();

        let missing: Vec<&str> = registry
            .ids()
            .filter(|&id| id != BlockId::AIR)
            .filter_map(|id| registry.name_of(id))
            .filter(|name| items.id_of(name).is_none())
            .collect();

        assert!(
            missing.is_empty(),
            "these blocks would drop nothing when broken -- add assets/items/<name>.ron              for each, or give 2.4's drop table an entry: {missing:?}"
        );
    }

    #[test]
    fn key_input_reports_whether_the_key_was_mapped() {
        let mut game = Game::new();
        assert!(game.key_input(KeyCode::KeyW, true));
        assert!(game.key_input(KeyCode::F4, true), "the free-fly toggle key");
        assert!(!game.key_input(KeyCode::KeyP, true), "unmapped key");
    }

    #[test]
    fn fly_toggle_flips_the_mode_on_a_single_press() {
        let mut game = Game::new();
        assert!(
            !game.host_player_mut().is_free_fly(),
            "walking is the default mode"
        );
        game.key_input(KeyCode::F4, true);
        game.advance(TICK_DT);
        assert!(game.host_player_mut().is_free_fly());
    }

    #[test]
    fn fly_toggle_edge_is_consumed_once_not_once_per_catchup_tick() {
        // A single key press must flip the mode exactly once, even when it
        // lands in a frame whose accumulator backlog forces several ticks to
        // run in one `advance` call -- reusing the same `InputFrame` across a
        // catch-up burst is correct for held movement, but a button edge
        // reapplied on every one of those ticks would flip the mode back and
        // forth instead of once. Two ticks makes a naive double-application
        // observable: it would leave the mode back at `false`.
        let mut game = Game::new();
        game.key_input(KeyCode::F4, true);
        game.advance(2.0 * TICK_DT);
        assert_eq!(game.server().sim.tick, 2);
        assert!(
            game.host_player_mut().is_free_fly(),
            "one press should flip the mode once (false -> true), not twice (-> false)"
        );
    }

    #[test]
    fn advance_by_exactly_one_tick_worth_of_time_runs_one_tick() {
        let mut game = Game::new();
        assert_eq!(game.server().sim.tick, 0);
        game.advance(TICK_DT);
        assert_eq!(game.server().sim.tick, 1);
    }

    #[test]
    fn sub_tick_dt_does_not_run_a_tick_yet() {
        let mut game = Game::new();
        game.advance(TICK_DT * 0.5);
        assert_eq!(
            game.server().sim.tick,
            0,
            "half a tick's worth of time isn't a tick"
        );
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.server().sim.tick, 1, "the other half completes it");
    }

    #[test]
    fn mouse_look_across_sub_tick_frames_is_not_dropped() {
        // The reported "can't look around on a fast machine" bug. The renderer
        // runs uncapped, so at high FPS most frames are shorter than one 60 Hz
        // tick and run zero ticks. Mouse motion arriving on those frames must
        // accumulate until a tick consumes it -- not be sampled and discarded
        // frame by frame, which dropped nearly all of it on a fast GPU.
        let mut spread = Game::new();
        for _ in 0..5 {
            spread.mouse_look(100.0, 0.0);
            spread.advance(TICK_DT * 0.1); // sub-tick: no tick runs yet
        }
        assert_eq!(
            spread.server().sim.tick,
            0,
            "5 * 0.1 tick < one tick: nothing simulated"
        );
        spread.advance(TICK_DT); // now cross the threshold -> exactly one tick
        assert_eq!(spread.server().sim.tick, 1);

        // The same 500 px of motion delivered in a single tick-sized frame.
        let mut once = Game::new();
        once.mouse_look(500.0, 0.0);
        once.advance(TICK_DT);
        assert_eq!(once.server().sim.tick, 1);

        assert_eq!(
            spread.host_player().look_dir(),
            once.host_player().look_dir(),
            "mouse motion spread over sub-tick frames must turn the player by \
             the same total as the same motion in one frame -- none dropped"
        );
    }

    #[test]
    fn a_multi_tick_catch_up_burst_applies_mouse_look_only_once() {
        // The opposite failure: `look_delta` is a one-shot accumulated total,
        // not a held state like `move_axes`. Reusing the unmodified frame across
        // every tick of a catch-up burst would multiply one frame's mouse motion
        // by however many ticks ran -- sporadic, inconsistent-feeling turns.
        let mut single = Game::new();
        single.mouse_look(1000.0, 0.0);
        single.advance(TICK_DT); // exactly one tick

        let mut burst = Game::new();
        burst.mouse_look(1000.0, 0.0);
        burst.advance(3.0 * TICK_DT); // three ticks in one catch-up burst

        assert_eq!(single.server().sim.tick, 1);
        assert_eq!(burst.server().sim.tick, 3);
        assert_eq!(
            single.host_player().look_dir(),
            burst.host_player().look_dir(),
            "the same single mouse-look delta must turn the player by the same \
             amount regardless of how many ticks ran in the same `advance` call"
        );
    }

    #[test]
    fn a_huge_dt_is_capped_not_caught_up_in_one_frame() {
        let mut game = Game::new();
        game.advance(1000.0 * TICK_DT); // a 1000-tick backlog in one call
        assert_eq!(
            game.server().sim.tick,
            MAX_TICKS_PER_FRAME as u64,
            "capped at MAX_TICKS_PER_FRAME, not fully caught up"
        );
        assert_eq!(
            game.accumulator, 0.0,
            "the leftover backlog is dropped, not carried into the next frame"
        );
    }

    #[test]
    fn frame_rate_independent_movement_reaches_the_same_state() {
        // The property Rule 1 exists for: the same input, driven by wildly
        // different frame timings that sum to the same elapsed time, must land
        // on identical sim state -- not "close", identical. 1000 ticks, per the
        // issue's own "Done when" (#57).
        let mut steady = Game::new();
        let mut jittery = Game::new();
        steady.key_input(KeyCode::KeyW, true);
        jittery.key_input(KeyCode::KeyW, true);

        let total_ticks = 1000u64;
        let total_time = total_ticks as f64 * TICK_DT as f64;

        // 1000 frames of exactly one tick's worth of time each.
        for _ in 0..total_ticks {
            steady.advance(TICK_DT);
        }

        // The same total time, spread over wildly uneven frame deltas -- some
        // under a tick, some several ticks at once (forcing the catch-up loop),
        // in `f64` so the *test's own* bookkeeping isn't what introduces drift
        // (`Game::advance`'s internal accumulator is `f64` for the same reason).
        let mut elapsed = 0.0f64;
        let deltas: [f64; 5] = [0.1, 3.7, 0.02, 1.0, 0.5].map(|m| m * TICK_DT as f64);
        let mut i = 0;
        while elapsed < total_time {
            let remaining = total_time - elapsed;
            let dt = deltas[i % deltas.len()].min(remaining);
            jittery.advance(dt as f32);
            elapsed += dt;
            i += 1;
        }

        assert_eq!(steady.server().sim.tick, jittery.server().sim.tick);
        assert_eq!(steady.host_player(), jittery.host_player());
    }

    /// Put `block` directly in front of the player and aim at it, so a test can
    /// choose which material it breaks rather than taking whatever terrain is
    /// underfoot. Returns the position it was placed at.
    fn stand_over(game: &mut Game, block_name: &str) -> [i32; 3] {
        let id = game
            .server_mut()
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of(block_name)
            .unwrap_or_else(|| panic!("no block {block_name}"));
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let b = ground.block;
        game.server_mut().set_block(b, id);
        // Drain it, so the setup's own edit is not mistaken for the first thing
        // the test does. A client only ever learns about a world through these.
        game.settle();
        b
    }

    /// Put the furnace at `pos` into a known state **through the server**, so
    /// the client's replica is told about it.
    ///
    /// Reaching into `game.server().world` directly is the shortcut a real client
    /// does not have (§8.2), so a test does not get it either -- a test that
    /// took it would be setting up a world the client never sees, and would
    /// then fail for a reason unrelated to what it asserts.
    fn edit_furnace(game: &mut Game, pos: [i32; 3], f: impl FnOnce(&mut cubara_world::Furnace)) {
        let mut furnace = game
            .server_mut()
            .world
            .furnace_at(pos)
            .copied()
            .unwrap_or_default();
        f(&mut furnace);
        game.server_mut().set_furnace(pos, furnace);
        game.settle();
    }

    /// Give the player `item` in the selected hotbar slot.
    fn hold(game: &mut Game, item: &str) {
        let items = game.server().items.as_ref().unwrap().clone();
        let id = items
            .id_of(item)
            .unwrap_or_else(|| panic!("no item {item}"));
        let stack = items.new_stack(id, 1).expect("a stack of one");
        let slot = game.host_player_mut().inventory.selected_slot() as usize;
        game.host_player_mut().inventory.set_slot(slot, Some(stack));
        // Let the client hear about it. Poking the host's inventory directly is
        // a test shortcut past the wire; without this the change would arrive on
        // the first measured tick and be mistaken for whatever that tick did.
        game.settle();
    }

    fn count_of(game: &Game, item: &str) -> u8 {
        let items = game.server().items.as_ref().unwrap().clone();
        let Some(id) = items.id_of(item) else {
            return 0;
        };
        (0..cubara_sim::SLOT_COUNT)
            .filter_map(|i| game.host_player().inventory.slot(i))
            .filter(|s| s.item() == id)
            .map(|s| s.count())
            .sum()
    }

    #[test]
    fn a_tool_below_the_required_tier_breaks_the_block_but_yields_nothing() {
        // §4's rule, and the reason it was chosen over refusing to break: the
        // block goes, the drop does not. Iron ore needs tier 2; a wooden pick
        // is tier 1.
        let (mut game, _) = game_looking_at_ground();
        let b = stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");

        game.break_block();

        game.settle();
        assert!(
            !game.world().is_solid_at(b[0], b[1], b[2], game.terrain()),
            "the block still breaks"
        );
        assert_eq!(count_of(&game, "cubara:raw_iron"), 0, "but yields nothing");
    }

    #[test]
    fn the_required_tier_yields_the_declared_drop_not_the_block() {
        // The other half: a stone pick is tier 2, so iron ore yields
        // `cubara:raw_iron` -- the declared drop, and *not* an item named after
        // the block, which is what the pre-2.4a policy would have given.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:stone_pick");

        game.break_block();

        game.settle();
        assert_eq!(count_of(&game, "cubara:raw_iron"), 1);
        assert_eq!(count_of(&game, "cubara:iron_ore"), 0);
    }

    #[test]
    fn stone_yields_cobble() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:wooden_pick");

        game.break_block();

        game.settle();
        assert_eq!(count_of(&game, "cubara:cobble"), 1);
        assert_eq!(count_of(&game, "cubara:stone"), 0);
    }

    #[test]
    fn stone_by_hand_yields_nothing() {
        // requires_tier 1, and the empty hand is tier 0.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");

        game.break_block();

        game.settle();
        assert_eq!(count_of(&game, "cubara:cobble"), 0);
    }

    #[test]
    fn leaves_yield_nothing_whatever_is_held() {
        // `drops: Nothing` ignores the tool entirely -- an iron pick is tier 3
        // and still gets no leaves.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:oak_leaves");
        hold(&mut game, "cubara:iron_pick");

        game.break_block();

        game.settle();
        assert_eq!(count_of(&game, "cubara:oak_leaves"), 0);
    }

    #[test]
    fn a_successful_break_costs_one_durability() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        let before = match game
            .host_player_mut()
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block();

        game.settle();
        let after = match game
            .host_player_mut()
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("still a pick, got {other:?}"),
        };
        assert_eq!(after, before - 1);
    }

    #[test]
    fn a_failed_tier_break_costs_no_durability() {
        // §4: you are not punished twice for the same mistake.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");
        let before = match game
            .host_player_mut()
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block();

        game.settle();
        let after = match game
            .host_player_mut()
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("still a pick, got {other:?}"),
        };
        assert_eq!(after, before, "the wasted swing cost nothing");
    }

    #[test]
    fn a_tool_at_zero_durability_leaves_the_slot() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.server().items.as_ref().unwrap().clone();
        let pick = items.id_of("cubara:stone_pick").unwrap();
        let nearly_dead = ItemStack::new(
            pick,
            1,
            ItemState::Durability { remaining: 1 },
            items.max_stack(pick),
        )
        .expect("a worn pick");
        let slot = game.host_player_mut().inventory.selected_slot() as usize;
        game.host_player_mut()
            .inventory
            .set_slot(slot, Some(nearly_dead));
        stand_over(&mut game, "cubara:stone");

        game.break_block();

        game.settle();
        assert!(
            game.host_player_mut().inventory.slot(slot).is_none(),
            "the spent tool is gone"
        );
        assert_eq!(
            count_of(&game, "cubara:cobble"),
            1,
            "the last break counted"
        );
    }

    #[test]
    fn breaking_bare_handed_is_not_an_error() {
        // Nothing to wear, and the hand is tier 0 -- soil requires nothing, so
        // this still yields.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:soil");

        game.break_block();

        game.settle();
        assert_eq!(count_of(&game, "cubara:soil"), 1);
    }

    /// Aim at `block_name` placed underfoot and hold the break button, then run
    /// `ticks` sim ticks. Returns how many ticks it took to break, or `None` if
    /// it had not broken by then.
    fn mine_for(game: &mut Game, ticks: u32) -> Option<u32> {
        game.set_breaking(true);
        for t in 1..=ticks {
            let dirty = game.advance(TICK_DT);
            if !dirty.is_empty() {
                return Some(t);
            }
        }
        None
    }

    #[test]
    fn mining_takes_ceil_hardness_over_speed_ticks() {
        // §4.3's formula, on the real assets — with the numbers **read** rather
        // than written down. The formula is what this test is about; stone's
        // hardness is tuning, and a test that hard-codes it fails every time
        // somebody adjusts how long digging feels, which teaches people to
        // edit tests when they change balance. That is a habit worth not
        // starting.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");

        let hardness = registry_hardness(&game, "cubara:stone");
        let speed = item_speed(&game, "cubara:stone_pick");
        let expected = hardness.div_ceil(speed);

        assert!(expected > 1, "a one-tick block would not test the formula");
        assert_eq!(mine_for(&mut game, expected * 3), Some(expected));
    }

    /// A block's hardness, from the loaded registry.
    fn registry_hardness(game: &Game, name: &str) -> u32 {
        let blocks = game.assets.as_ref().expect("assets").blocks.clone();
        let id = blocks.id_of(name).expect("a known block");
        blocks.hardness(id).expect("a breakable block")
    }

    /// An item's mining speed, from the loaded registry.
    fn item_speed(game: &Game, name: &str) -> u32 {
        let items = &game.assets.as_ref().expect("assets").items;
        items.speed(items.id_of(name).expect("a known item"))
    }

    #[test]
    fn a_faster_tool_breaks_the_same_block_in_fewer_ticks() {
        // The whole point of the block: the tool changes the time, not just
        // whether you get a drop. Expected times are derived from the registry
        // for the same reason as above.
        let tools = [
            None,
            Some("cubara:wooden_pick"),
            Some("cubara:stone_pick"),
            Some("cubara:iron_pick"),
        ];

        let mut previous: Option<u32> = None;
        for tool in tools {
            let (mut game, _) = game_looking_at_ground();
            stand_over(&mut game, "cubara:stone");
            let speed = match tool {
                Some(name) => {
                    hold(&mut game, name);
                    item_speed(&game, name)
                }
                None => 1,
            };
            let hardness = registry_hardness(&game, "cubara:stone");
            let expected = hardness.div_ceil(speed);

            assert_eq!(
                mine_for(&mut game, expected * 3),
                Some(expected),
                "{tool:?} should take ceil({hardness}/{speed}) ticks"
            );
            if let Some(slower) = previous {
                assert!(
                    expected <= slower,
                    "{tool:?} is not faster than the tool before it"
                );
            }
            previous = Some(expected);
        }
        assert!(
            previous.expect("at least one tool")
                < registry_hardness(&game_looking_at_ground().0, "cubara:stone"),
            "the best tool is no faster than a bare hand, so nothing is being tested"
        );
    }

    #[test]
    fn releasing_the_button_abandons_progress() {
        // §4.3: abandoned, not banked. Most of a break, then let go -- starting
        // again must cost the whole thing, not the remainder.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        let full = break_ticks(&game, "cubara:stone", "cubara:stone_pick");

        game.set_breaking(true);
        for _ in 0..(full - 2) {
            assert!(game.advance(TICK_DT).is_empty());
        }
        game.set_breaking(false);
        game.advance(TICK_DT);

        assert_eq!(
            mine_for(&mut game, full * 3),
            Some(full),
            "restarted from zero"
        );
    }

    /// How many ticks `block` takes with `tool`, from the registries.
    ///
    /// Derived rather than written down: these numbers are *tuning*, and a test
    /// that hard-codes them fails whenever somebody adjusts how long digging
    /// feels -- which teaches people to edit tests when they change balance.
    fn break_ticks(game: &Game, block: &str, tool: &str) -> u32 {
        registry_hardness(game, block).div_ceil(item_speed(game, tool))
    }

    #[test]
    fn switching_tools_abandons_progress() {
        // The stored break is keyed by tool as well as position.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:wooden_pick");

        game.set_breaking(true);
        for _ in 0..10 {
            assert!(game.advance(TICK_DT).is_empty(), "wooden pick needs 15");
        }
        hold(&mut game, "cubara:stone_pick");

        // A fresh start at the new tool's own cost, not the remainder that
        // would be left if the wooden pick's progress had carried over.
        let fresh = break_ticks(&game, "cubara:stone", "cubara:stone_pick");
        assert_eq!(mine_for(&mut game, fresh * 3), Some(fresh));
    }

    #[test]
    fn a_timed_break_applies_the_same_drop_rules_as_an_instant_one() {
        // `break_at` is shared, so 2.4a's tier gate still holds: iron ore
        // needs tier 2, and a wooden pick mines it (slowly) for nothing.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");

        let slow = break_ticks(&game, "cubara:iron_ore", "cubara:wooden_pick");
        assert_eq!(mine_for(&mut game, slow * 3), Some(slow));
        assert_eq!(count_of(&game, "cubara:raw_iron"), 0, "tier too low");

        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:stone_pick");
        let quick = break_ticks(&game, "cubara:iron_ore", "cubara:stone_pick");
        assert!(quick < slow, "the better tool should be faster");
        assert_eq!(mine_for(&mut game, quick * 3), Some(quick));
        assert_eq!(count_of(&game, "cubara:raw_iron"), 1);
    }

    #[test]
    fn cracks_are_on_the_block_being_dug_and_grow_until_it_breaks() {
        let (mut game, _) = game_looking_at_ground();
        let dug = stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        assert_eq!(game.cracking(), None, "not digging");

        let total = break_ticks(&game, "cubara:stone", "cubara:stone_pick");
        game.set_breaking(true);
        game.advance(TICK_DT);
        let (block, first) = game.cracking().expect("digging");
        assert_eq!(block, dug, "cracks on a different block from the one dug");
        for _ in 1..(total - 1) {
            game.advance(TICK_DT);
        }
        let (_, later) = game.cracking().expect("still digging");
        assert!(later > first, "{later} did not grow past {first}");
        game.advance(TICK_DT);
        assert_eq!(game.cracking(), None, "broken, so nothing left to crack");
    }

    #[test]
    fn mining_progress_reports_a_fraction_that_climbs_to_the_break() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        assert_eq!(game.mining_progress(), None, "nothing started yet");

        // Every tick but the last, so the count comes from the registry rather
        // than from how long digging happened to take when this was written.
        let total = break_ticks(&game, "cubara:stone", "cubara:stone_pick");
        game.set_breaking(true);
        let mut last = 0.0;
        for _ in 0..(total - 1) {
            game.advance(TICK_DT);
            let p = game.mining_progress().expect("a break is in progress");
            assert!(p > last, "progress must climb: {p} after {last}");
            assert!(p < 1.0, "not finished yet: {p}");
            last = p;
        }
        game.advance(TICK_DT);
        assert_eq!(game.mining_progress(), None, "finished, so nothing pending");
    }

    #[test]
    fn mining_is_tick_identical_across_two_runs() {
        // Rule 1: same inputs, same tick, same result. Two independent games
        // driven identically must break on the same tick.
        let run = || {
            let (mut game, _) = game_looking_at_ground();
            stand_over(&mut game, "cubara:iron_ore");
            hold(&mut game, "cubara:stone_pick");
            mine_for(&mut game, 40)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn one_frame_of_several_ticks_advances_mining_by_all_of_them() {
        // A catch-up burst is N ticks of progress, unlike `jump`, which is a
        // one-shot. That difference is deliberate (see `InputFrame::breaking`).
        //
        // Asserted as **progress made**, not as a block finishing. The first
        // version needed a block breakable inside one five-tick frame, which
        // tied it to how long digging happens to take -- and it duly broke the
        // day those times were tuned. What the burst does is add five ticks of
        // work; whether that finishes anything is a different question, asked
        // by `a_frame_longer_than_the_catch_up_cap_still_only_mines_the_cap`.
        // Two identical games: one given a frame worth a single tick, the
        // other a frame worth five.
        let mut one_tick = mining_stone_with_an_iron_pick();
        let mut five_ticks = mining_stone_with_an_iron_pick();

        one_tick.advance(TICK_DT);
        let after_one = one_tick.mining_progress().expect("a break in progress");
        // 5.5 rather than exactly 5.0: `TICK_DT * 5.0` in `f32` can land a
        // hair under five ticks' worth once widened to the `f64` accumulator.
        // The surplus stays in the accumulator; the cap still limits it to five.
        five_ticks.advance(TICK_DT * 5.5);
        let after_five = five_ticks.mining_progress().expect("a break in progress");

        assert!(
            after_five > after_one * 3.0,
            "five ticks in one frame advanced mining by {after_five}, barely \
             more than one tick's {after_one} -- the burst is not being applied"
        );
    }

    /// A game standing on stone, holding an iron pick, button already down.
    fn mining_stone_with_an_iron_pick() -> Game {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:iron_pick");
        game.set_breaking(true);
        game
    }

    #[test]
    fn a_frame_longer_than_the_catch_up_cap_still_only_mines_the_cap() {
        // The spiral-of-death guard applies to mining too: a frame worth 20
        // ticks runs five, so a break needing eight is not finished by it.
        // Mining must not be a way to smuggle progress past the cap.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        game.set_breaking(true);

        assert!(
            game.advance(TICK_DT * 20.0).is_empty(),
            "capped at five ticks, and stone at speed 4 needs eight"
        );
    }

    /// Place a furnace at the block the player is looking at and open it.
    fn open_a_furnace(game: &mut Game) -> [i32; 3] {
        let pos = stand_over(game, "cubara:furnace");
        game.server_mut().add_furnace(pos);
        game.settle();
        game.open_furnace = Some(pos);
        game.inventory_open = true;
        pos
    }

    fn item(game: &Game, name: &str) -> cubara_voxel::ItemId {
        game.server()
            .items
            .as_ref()
            .unwrap()
            .id_of(name)
            .expect(name)
    }

    #[test]
    fn right_clicking_a_furnace_opens_its_screen() {
        // And it reads `Interact` off the registry rather than comparing names,
        // which is what block 2.4c generalised.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:furnace");

        assert!(game.interact(), "the furnace is interactive");
        assert!(game.inventory_open());
        assert!(
            game.open_furnace().is_some(),
            "a furnace screen, not a bench"
        );
    }

    #[test]
    fn a_bench_still_opens_the_three_by_three_grid() {
        // The same registry lookup must keep the bench working -- Rule 5's
        // "one implementation" cuts both ways.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:crafting_bench");

        assert!(game.interact());
        assert!(game.inventory_open());
        assert!(game.open_furnace().is_none(), "a bench, not a furnace");
        assert_eq!(game.host_player_mut().crafting.width(), 3);
    }

    #[test]
    fn a_furnace_smelts_raw_iron_into_an_ingot_over_ticks() {
        // The last rung of REQUIREMENTS #5: ore you mined becomes metal you can
        // craft with, checked end to end rather than trusting the unit tests.
        // (A log burns 800 ticks now, so one would do; four is not the point.)
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 1));
            f.fuel = Some((log, 4));
        });

        for _ in 0..210 {
            game.advance(TICK_DT);
        }

        let f = game.open_furnace().expect("still open");
        assert_eq!(
            f.output,
            Some((item(&game, "cubara:iron_ingot"), 1)),
            "one ingot"
        );
        assert_eq!(f.input, None, "the raw iron was consumed");
    }

    #[test]
    fn furnace_gauges_report_burn_left_and_progress_made() {
        let (mut game, _) = game_looking_at_ground();
        assert_eq!(game.furnace_gauges(), None, "no furnace open");
        let pos = open_a_furnace(&mut game);
        let idle = game.furnace_gauges().expect("a furnace is open");
        assert_eq!((idle.burn, idle.progress), (0.0, 0.0), "empty, so not lit");
        let raw = item(&game, "cubara:raw_iron");
        let plank = item(&game, "cubara:plank");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 2));
            f.fuel = Some((plank, 3));
        });

        // A quarter of a plank's burn and of the recipe: 50 of 200 ticks.
        // `edit_furnace` settles, which is the first of them.
        for _ in 0..49 {
            game.advance(TICK_DT);
        }
        let g = game.furnace_gauges().unwrap();
        assert!((g.progress - 0.25).abs() < 0.011, "progress {}", g.progress);
        assert!((g.burn - 0.75).abs() < 0.011, "burn {}", g.burn);
    }

    /// The owner's tuning: one plank smelts one ingot. It was ten, and a player
    /// who put a few planks in saw nothing come out.
    #[test]
    fn one_plank_smelts_one_ingot() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let plank = item(&game, "cubara:plank");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 2));
            f.fuel = Some((plank, 1));
        });
        for _ in 0..450 {
            game.advance(TICK_DT);
        }
        let f = game.open_furnace().unwrap();
        assert_eq!(f.output.map(|o| o.1), Some(1), "one plank, one ingot");
        assert_eq!(f.input, Some((raw, 1)), "and not a second one");
    }

    /// Mined stone comes up as cobble, and cobble goes back down as a block.
    #[test]
    fn cobble_can_be_placed() {
        let (mut game, ground) = game_looking_at_ground();
        hold(&mut game, "cubara:cobble");
        game.place_block();
        game.settle();
        let registry = game.server().blocks_registry.clone().unwrap();
        let terrain = game.terrain();
        let above = [ground[0], ground[1] + 1, ground[2]];
        let placed = game.world().block_at(above[0], above[1], above[2], terrain);
        assert_eq!(registry.name_of(placed), Some("cubara:cobble"));
        assert_eq!(count_of(&game, "cubara:cobble"), 0, "the held one was used");
    }

    #[test]
    fn an_item_id_reads_as_capitalised_words() {
        assert_eq!(display_name("cubara:wooden_pick"), "Wooden Pick");
        assert_eq!(display_name("cubara:stick"), "Stick");
        assert_eq!(display_name("iron_ingot"), "Iron Ingot", "no namespace");
    }

    /// Where on a 1280x720 screen the centre of `kind`/`index` is.
    fn slot_centre(panel: &InventoryPanel, kind: PanelSlotKind, index: usize) -> (f32, f32) {
        let s = panel
            .slots()
            .iter()
            .find(|s| s.kind == kind && s.index == index)
            .unwrap_or_else(|| panic!("no {kind:?} {index}"));
        (s.x + s.size / 2.0, s.y + s.size / 2.0)
    }

    #[test]
    fn hovering_a_slot_names_what_is_in_it_and_nothing_else() {
        let mut game = game_with_assets();
        let items = game.server().items.as_ref().unwrap().clone();
        let stick = items.new_stack(item(&game, "cubara:stick"), 3).unwrap();
        let plank = items.new_stack(item(&game, "cubara:plank"), 2).unwrap();
        game.host_player_mut().inventory.set_slot(4, Some(stick));
        game.host_player_mut().inventory.set_slot(5, Some(plank));
        game.settle();
        game.toggle_inventory();

        let (w, h) = (1280, 720);
        let panel = InventoryPanel::layout(w, h, 2);
        let at = |kind, index| slot_centre(&panel, kind, index);
        let name = |g: &Game, (x, y): (f32, f32)| g.hovered_item_name(x, y, w, h);

        // Two different items side by side, so naming the neighbour is visible.
        assert_eq!(
            name(&game, at(PanelSlotKind::Inventory, 4)).as_deref(),
            Some("Stick")
        );
        assert_eq!(
            name(&game, at(PanelSlotKind::Inventory, 5)).as_deref(),
            Some("Plank")
        );
        assert_eq!(
            name(&game, at(PanelSlotKind::Inventory, 6)),
            None,
            "an empty slot"
        );
        assert_eq!(name(&game, (1.0, 1.0)), None, "open space");

        // Carrying something: the carried stack is where the label would be.
        game.click_panel(
            at(PanelSlotKind::Inventory, 4).0,
            at(PanelSlotKind::Inventory, 4).1,
            false,
            w,
            h,
        );
        assert_eq!(
            name(&game, at(PanelSlotKind::Inventory, 5)),
            None,
            "shown while carrying"
        );

        game.toggle_inventory();
        assert_eq!(
            name(&game, at(PanelSlotKind::Inventory, 5)),
            None,
            "shown with no screen"
        );
    }

    #[test]
    fn a_furnace_with_no_fuel_smelts_nothing() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 1)));

        for _ in 0..400 {
            game.advance(TICK_DT);
        }

        let f = game.open_furnace().unwrap();
        assert_eq!(f.output, None);
        assert_eq!(f.input, Some((raw, 1)), "nothing consumed either");
    }

    #[test]
    fn clicking_the_furnace_slots_puts_items_in_and_takes_them_out() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let stack = game
            .server_mut()
            .items
            .as_ref()
            .unwrap()
            .new_stack(raw, 3)
            .unwrap();
        game.host_player_mut().crafting.set_held(Some(stack));

        // Into the input slot.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        game.settle();
        assert_eq!(
            game.open_furnace().unwrap().input,
            Some((raw, 3)),
            "the held stack went in"
        );
        assert!(
            game.host_player_mut().crafting.held().is_none(),
            "hand is empty"
        );

        // And back out.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        game.settle();
        assert_eq!(game.open_furnace().unwrap().input, None);
        assert_eq!(
            game.host_player_mut().crafting.held().map(|s| s.count()),
            Some(3)
        );
    }

    #[test]
    fn the_output_slot_is_take_only() {
        // Putting something back into the output would let the next completed
        // smelt stack onto it, which is work out of nothing.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let stack = game
            .server_mut()
            .items
            .as_ref()
            .unwrap()
            .new_stack(raw, 1)
            .unwrap();
        game.host_player_mut().crafting.set_held(Some(stack));

        game.click_furnace(pos, PanelSlotKind::Result, 0);

        game.settle();
        assert_eq!(game.open_furnace().unwrap().output, None, "nothing went in");
        assert!(
            game.host_player_mut().crafting.held().is_some(),
            "still held"
        );
    }

    #[test]
    fn breaking_a_furnace_takes_its_state_with_it() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 5)));

        game.break_at(pos);

        assert!(game.world().furnace_at(pos).is_none(), "the entity is gone");
        assert!(!game.inventory_open(), "and its screen closed");
    }

    #[test]
    fn placing_a_furnace_gives_it_state_immediately() {
        // Not on first use: a furnace nobody opens must still tick, and the
        // world hash must cover it either way.
        let (mut game, _) = game_looking_at_ground();
        let furnace = game
            .server_mut()
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:furnace")
            .unwrap();
        let items = game.server().items.as_ref().unwrap().clone();
        let id = items.id_of("cubara:furnace").unwrap();
        let stack = items.new_stack(id, 1).unwrap();
        let slot = game.host_player_mut().inventory.selected_slot() as usize;
        game.host_player_mut().inventory.set_slot(slot, Some(stack));

        game.place_block();
        game.settle();
        let _ = furnace;
        assert_eq!(
            game.world().block_entities().count(),
            1,
            "the placed furnace owns state"
        );
    }

    #[test]
    fn closing_a_furnace_screen_keeps_its_contents_in_the_block() {
        // Unlike a crafting grid, whose cells are emptied into the inventory on
        // close: a furnace's slots belong to the block, not to the screen.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 2)));

        game.toggle_inventory();

        game.settle();
        assert!(!game.inventory_open(), "closed");
        assert_eq!(
            game.world().furnace_at(pos).unwrap().input,
            Some((raw, 2)),
            "contents stayed in the furnace"
        );
    }

    #[test]
    fn a_drop_that_does_not_fit_falls_on_the_floor_instead_of_vanishing() {
        // Block 2.5's whole reason for existing. Before this, the item was
        // logged and destroyed.
        let (mut game, _) = game_looking_at_ground();
        let b = stand_over(&mut game, "cubara:soil");
        // Fill every slot with something that will not stack with soil.
        let raw = item(&game, "cubara:raw_iron");
        for i in 0..cubara_sim::SLOT_COUNT {
            let full = game
                .server_mut()
                .items
                .as_ref()
                .unwrap()
                .new_stack(raw, 64)
                .unwrap();
            game.host_player_mut().inventory.set_slot(i, Some(full));
        }

        game.break_at(b);

        assert_eq!(
            game.server().sim.entities.len(),
            1,
            "the drop is on the floor"
        );
        let (_, d) = game.server().sim.entities.sorted()[0];
        assert_eq!(
            game.server()
                .items
                .as_ref()
                .unwrap()
                .name_of(d.stack.item()),
            Some("cubara:soil")
        );
    }

    #[test]
    fn a_broken_furnace_spills_its_contents() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 4));
            f.fuel = Some((log, 2));
        });

        game.break_at(pos);

        // Three would-be-lost stacks: input, fuel, and the furnace's own drop
        // goes to the inventory, so two entities plus whatever did not fit.
        assert!(
            game.server().sim.entities.len() >= 2,
            "input and fuel are on the floor, got {}",
            game.server().sim.entities.len()
        );
    }

    #[test]
    fn walking_over_a_dropped_item_picks_it_up() {
        let (mut game, _) = game_looking_at_ground();
        let stack = {
            let items = game.server().items.as_ref().unwrap().clone();
            let id = items.id_of("cubara:cobble").unwrap();
            items.new_stack(id, 7).unwrap()
        };
        // Right where the player is standing.
        let at = game.host_player_mut().pos;
        game.server_mut()
            .sim
            .entities
            .spawn_item(stack, at, FixedVec3::ZERO);

        game.advance(TICK_DT);

        assert_eq!(game.server().sim.entities.len(), 0, "collected");
        assert_eq!(count_of(&game, "cubara:cobble"), 7);
    }

    #[test]
    fn a_dormant_chunk_ends_where_a_continuously_ticked_one_would() {
        // **The phase 2 exit gate's dormant test** (§11.3), and the reason
        // block 2.4c insisted `Furnace::advance` take an elapsed count.
        //
        // Run the same furnace two ways for the same number of ticks: once with
        // the player standing next to it the whole time, and once with the
        // player far away so the chunk sleeps and is caught up on return.
        for total in [50u64, 199, 200, 201, 450] {
            let continuous = {
                let (mut game, _) = game_looking_at_ground();
                let pos = open_a_furnace(&mut game);
                load_furnace(&mut game, pos);
                for _ in 0..total {
                    game.advance(TICK_DT);
                }
                game.world().furnace_at(pos).copied().expect("still there")
            };

            let slept = {
                let (mut game, _) = game_looking_at_ground();
                let pos = open_a_furnace(&mut game);
                load_furnace(&mut game, pos);
                // Exactly `total` ticks here too, or the comparison is against
                // a different amount of elapsed time rather than against
                // dormancy: one nearby, the middle away, one back home.
                let home = game.host_player_mut().pos;
                game.advance(TICK_DT);
                game.host_player_mut().pos = home + FixedVec3::from_f32([4000.0, 0.0, 0.0]);
                for _ in 0..total - 2 {
                    game.advance(TICK_DT);
                }
                // Come back: the chunk wakes and catches up.
                game.host_player_mut().pos = home;
                game.advance(TICK_DT);
                game.world().furnace_at(pos).copied().expect("still there")
            };

            assert_eq!(
                continuous.output, slept.output,
                "output diverged after {total} ticks"
            );
            assert_eq!(
                continuous.input, slept.input,
                "input diverged after {total} ticks"
            );
        }
    }

    /// A furnace with enough raw iron and fuel to run for a long while.
    fn load_furnace(game: &mut Game, pos: [i32; 3]) {
        let raw = item(game, "cubara:raw_iron");
        let log = item(game, "cubara:oak_log");
        edit_furnace(game, pos, |f| {
            f.input = Some((raw, 8));
            f.fuel = Some((log, 32));
        });
    }

    #[test]
    fn a_chunk_the_player_leaves_goes_dormant() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        load_furnace(&mut game, pos);
        game.advance(TICK_DT);

        let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
        // Read from the *server*: which chunks simulate is authority (§8.1),
        // and the client's replica has no lifecycle at all -- it is told about
        // edits, not about what is ticking.
        assert_eq!(
            game.server().world.chunk_states().get(coord),
            cubara_world::ChunkState::Active,
            "active while the player is here"
        );

        game.host_player_mut().pos += FixedVec3::from_f32([4000.0, 0.0, 0.0]);
        game.advance(TICK_DT);

        assert!(
            matches!(
                game.server().world.chunk_states().get(coord),
                cubara_world::ChunkState::Dormant { .. }
            ),
            "dormant once they leave"
        );
    }

    #[test]
    fn a_furnace_in_a_dormant_chunk_does_not_tick() {
        // Not "does nothing" -- deferred. The catch-up test above is the other
        // half, and together they are what makes dormancy invisible.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        load_furnace(&mut game, pos);
        game.advance(TICK_DT);
        let after_one = game.world().furnace_at(pos).copied().unwrap();

        game.host_player_mut().pos += FixedVec3::from_f32([4000.0, 0.0, 0.0]);
        for _ in 0..500 {
            game.advance(TICK_DT);
        }

        let now = game.world().furnace_at(pos).copied().unwrap();
        assert_eq!(
            now.progress, after_one.progress,
            "a dormant furnace did not advance"
        );
    }

    #[test]
    fn falling_onto_the_ground_actually_hurts() {
        // Through the real physics and the real tick loop, not the damage
        // formula in isolation: the formula is unit-tested in `cubara-sim`, and
        // what this asserts is that a fall *reaches* it.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        // Ten blocks: past the 3-block safe distance, but **survivable**.
        // A longer drop would deal more than full health, and death restores
        // it -- so a lethal fall reads as "no damage" here. That is what the
        // first version of this test measured, and it is why the lethal case
        // has its own test below, asserting the respawn instead.
        *game.host_player_mut() = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 11.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        );
        let full = game.host_player_mut().health;

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.host_player_mut().on_ground {
                break;
            }
        }

        assert!(game.host_player_mut().on_ground, "it landed");
        assert!(
            game.host_player_mut().health < full,
            "landing from ten blocks left {} of {full} health",
            game.host_player_mut().health
        );
    }

    #[test]
    fn a_lethal_fall_returns_you_to_spawn_with_your_things() {
        // The owner's decision (§13.4): death costs position, not progress.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let spawn = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.0, 0.5]);
        *game.host_player_mut() = Player::new(spawn, Angle::ZERO, Angle::ZERO);
        // Give them something to lose, then drop them from lethal height.
        hold(&mut game, "cubara:iron_pick");
        let carried = game.host_player_mut().inventory;
        game.host_player_mut().pos =
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 60.0, 0.5]);

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.host_player_mut().pos.y <= spawn.y && game.host_player_mut().on_ground {
                break;
            }
        }

        assert_eq!(
            game.host_player_mut().health,
            cubara_sim::MAX_HEALTH,
            "respawned at full health"
        );
        assert_eq!(
            game.host_player_mut().inventory,
            carried,
            "and kept the pick"
        );
        // **Position too.** Without this the test passed while respawn did not
        // actually move anyone: `physics::step` wrote `player.pos` from its own
        // local box *after* the damage was applied, silently undoing the
        // respawn. Health and inventory alone could not see that.
        assert!(
            game.host_player_mut().pos.distance_squared(spawn)
                < (5 * cubara_voxel::fixed::ONE as i128 / 2).pow(2),
            "respawned at {:?} rather than near spawn {spawn:?}",
            game.host_player_mut().pos
        );
    }

    #[test]
    fn walking_off_a_low_step_does_not_hurt() {
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        *game.host_player_mut() = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 2.5, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        );
        let full = game.host_player_mut().health;

        for _ in 0..300 {
            game.advance(TICK_DT);
        }

        assert_eq!(game.host_player_mut().health, full, "a short drop is free");
    }

    #[test]
    fn free_fly_never_hurts_however_far_you_descend() {
        // It is a debug mode; dropping out of the sky in it must not kill you
        // (§13.3). The fall distance is cleared every tick it is active.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        *game.host_player_mut() = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 80.0, 0.5]),
            Angle::ZERO,
            Angle::from_radians(-1.5),
        );
        // Toggle free-fly on, then descend through the whole drop.
        game.fly_toggle_pending = true;
        game.down = true;
        for _ in 0..600 {
            game.advance(TICK_DT);
        }
        let health_in_flight = game.host_player_mut().health;
        assert_eq!(
            health_in_flight,
            cubara_sim::MAX_HEALTH,
            "free-fly descent cost health"
        );
    }

    #[test]
    fn the_game_does_not_start_by_killing_the_player() {
        // **The test that was missing.** Every other test in this file
        // repositions the player just above the ground before doing anything,
        // so none of them started the game the way the app does -- and the app
        // started it 32 blocks above the terrain, which block 2.9a turned into
        // 29 damage against 20 health. The player died on the first landing,
        // respawned at the same mid-air point, and died again, forever.
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );

        for _ in 0..600 {
            game.advance(TICK_DT);
        }

        assert!(
            game.host_player_mut().on_ground,
            "the player never settled on the ground"
        );
        let health = game.host_player().health;
        assert_eq!(
            health,
            cubara_sim::MAX_HEALTH,
            "starting the game cost {} health",
            cubara_sim::MAX_HEALTH - health
        );
    }

    #[test]
    fn the_spawn_point_is_somewhere_survivable() {
        // The other half: respawning must not drop the player into a lethal
        // fall, or death becomes a loop rather than a setback.
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );

        // Kill them outright, then let the world run.
        game.host_player_mut().take_damage(cubara_sim::MAX_HEALTH);
        for _ in 0..600 {
            game.advance(TICK_DT);
        }

        assert_eq!(
            game.host_player_mut().health,
            cubara_sim::MAX_HEALTH,
            "respawning cost health, so death loops"
        );
        assert!(game.host_player_mut().on_ground, "and it landed");
    }

    #[test]
    fn there_is_solid_stone_however_far_down_you_go() {
        // The world has no floor. Generation never had `y` bounds -- what was
        // missing was streaming and simulating anywhere but chunk layers 0..=2.
        let (game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        let registry = game.server().blocks_registry.as_ref().unwrap();
        for y in [-1, -100, -5_000, -100_000] {
            let block = game.world().block_at(0, y, 0, terrain);
            assert_eq!(
                registry.name_of(block),
                Some("cubara:stone"),
                "expected stone at y = {y}"
            );
            assert!(game.world().is_solid_at(0, y, 0, terrain), "solid at {y}");
        }
    }

    #[test]
    fn there_is_open_sky_however_far_up_you_go() {
        let (game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        for y in [100, 5_000, 100_000] {
            assert!(
                !game.world().is_solid_at(0, y, 0, terrain),
                "expected air at y = {y}"
            );
        }
    }

    #[test]
    fn a_block_can_be_placed_and_broken_far_below_the_old_world_floor() {
        // y = 0 used to be the bottom of the world. Editing below it has to
        // persist like any other edit -- the overlay is keyed by world
        // position and never had a floor either.
        let (mut game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        let deep = [3, -2_000, 7];

        let cc = game.server_mut().set_block(deep, BlockId::AIR);
        game.settle();
        assert_eq!(cc, ChunkCoord::from_block(deep[0], deep[1], deep[2]));

        // Asserted against the **server's** world, not the replica. It used to
        // be the replica, and block 2.11 is why it moved: this test is about the
        // edit overlay having no floor -- its own comment says so -- and the
        // overlay is the server's. Two thousand blocks down is far outside any
        // client's replication radius, so the replica *correctly* never hears
        // about it now; `a_deep_edit_is_not_replicated_to_a_client_who_cannot_see_it`
        // pins that as the deliberate behaviour it is.
        assert!(
            !game
                .server_mut()
                .world
                .is_solid_at(deep[0], deep[1], deep[2], terrain),
            "the deep block was mined out"
        );

        // And its neighbours are still stone, so the edit is local.
        assert!(game
            .server_mut()
            .world
            .is_solid_at(deep[0] + 1, deep[1], deep[2], terrain));
    }

    /// Interest management, stated as the behaviour it is (block 2.11).
    ///
    /// An edit nobody can perceive is not queued for them. This is the property
    /// that stops a client's byte count depending on how much of the world other
    /// people are digging up, and it is worth a test of its own precisely
    /// because it looks like a bug when you meet it in the test above.
    #[test]
    fn a_deep_edit_is_not_replicated_to_a_client_who_cannot_see_it() {
        let (mut game, _) = game_looking_at_ground();
        let deep = [3, -2_000, 7];

        game.server_mut().set_block(deep, BlockId::AIR);
        let effects = {
            let who = game.me_id;
            game.server_mut().drain_effects_for(who)
        };
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                cubara_server::Effect::Edit { pos, .. } if *pos == deep
            )),
            "an edit two thousand blocks below the player was sent to them anyway"
        );

        // The same edit, made where the player is standing, *is* sent -- so the
        // assertion above is about distance and not about `set_block` being mute.
        let near = {
            let p = game.host_player().pos.to_f32();
            [p[0] as i32, p[1] as i32 - 3, p[2] as i32]
        };
        game.server_mut().set_block(near, BlockId::AIR);
        let effects = {
            let who = game.me_id;
            game.server_mut().drain_effects_for(who)
        };
        assert!(
            effects.iter().any(|e| matches!(
                e,
                cubara_server::Effect::Edit { pos, .. } if *pos == near
            )),
            "an edit at the player's feet did not reach them"
        );
    }

    #[test]
    fn a_chunk_far_below_the_old_floor_generates_and_meshes() {
        // Generating at depth must produce a real chunk, not an empty or
        // panicking one -- `ChunkCoord` is i32 and `region_of` uses div_euclid,
        // both of which were already correct for negative coordinates.
        let (game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        let deep = ChunkCoord::new(0, -64, 0);
        let chunk = game
            .world()
            .chunk_at(deep, terrain)
            .expect("a chunk that deep still generates");
        let registry = game.server().blocks_registry.as_ref().unwrap();
        assert_eq!(
            registry.name_of(chunk.get(0, 0, 0)),
            Some("cubara:stone"),
            "a chunk a thousand blocks down is solid rock"
        );
    }

    #[test]
    fn the_simulation_follows_the_player_downward() {
        // A furnace a long way below the old world floor must tick when the
        // player is next to it -- the simulated band moves with them now.
        let (mut game, _) = game_looking_at_ground();
        let deep = [0, -1_000, 0];
        game.server_mut().add_furnace(deep);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, deep, |f| {
            f.input = Some((raw, 2));
            f.fuel = Some((log, 4));
        });
        // Stand next to it.
        game.host_player_mut().pos = cubara_voxel::FixedVec3::from_f32([0.5, -1_000.0, 0.5]);
        game.host_player_mut().spawn = game.host_player_mut().pos;

        for _ in 0..250 {
            game.advance(TICK_DT);
        }

        let f = game.world().furnace_at(deep).expect("still there");
        assert!(
            f.progress > 0 || f.output.is_some(),
            "a furnace at y = -1000 never ticked"
        );
    }

    /// A `Game` with assets wired, saving into a scratch directory rather than
    /// the real world folder.
    fn game_with_assets() -> Game {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        game
    }

    #[test]
    fn a_world_survives_being_closed_and_reopened() {
        // **#179's test, and the one that was missing.** The save *format* was
        // tested from the start; nothing tested that a player action reaches
        // it. `save_world`/`load_world` were never called from this crate at
        // all, so the world was not still there after you closed it -- which is
        // the one sentence ROADMAP.md uses to describe phase 1's result.
        let dir = std::env::temp_dir().join(format!(
            "cubara-app-roundtrip-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mined;
        let carried;
        {
            let mut game = game_with_assets();
            // Do something a player would: take a block, and move.
            let ground = game
                .world()
                .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
                .expect("ground");
            mined = ground.block;
            game.host_player_mut().pos =
                cubara_voxel::FixedVec3::from_f32([0.5, mined[1] as f32 + 3.5, 0.5]);
            game.break_at(mined);
            carried = game.host_player_mut().inventory;

            game.server_mut().save_to(&dir);
        }

        let mut reopened = game_with_assets();
        assert!(reopened.load_from(&dir), "the save did not load");

        assert!(
            !reopened
                .world()
                .is_solid_at(mined[0], mined[1], mined[2], reopened.terrain()),
            "the mined block came back"
        );
        assert_eq!(
            reopened.host_player().inventory,
            carried,
            "the inventory did not survive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loading_a_save_without_this_client_in_it_does_not_unseat_it() {
        // **The crash `run.bat` hit on the first click.** The window hosts its
        // own world and attaches to it, so this client is not
        // `PlayerId::LOCAL` -- and then it loads `saves/world`, which replaced
        // every player with the save's. A save written before the client was
        // attached holds only player 0, so afterwards this client drove a
        // player that did not exist, and the first action on it panicked.
        let dir = std::env::temp_dir().join(format!(
            "cubara-app-unseated-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        {
            // A world played by its local player only: the save has player 0.
            let mut old = cubara_server::Server::new();
            let items = load_item_registry();
            let recipes = load_recipe_book(&items);
            old.set_assets(
                std::sync::Arc::new(cubara_render::load_registry()),
                items,
                recipes,
            );
            old.save_to(&dir);
        }

        let mut game = game_with_assets();
        assert_ne!(
            game.me_id,
            PlayerId::LOCAL,
            "this client is player 0, so the save would contain it by coincidence \
             and the test would not be testing anything"
        );
        assert!(game.load_from(&dir), "the save did not load");

        assert!(
            game.server().sim.get(game.me_id).is_some(),
            "loading removed the player this client is driving"
        );
        // And the things that crashed: a click, and holding the button.
        game.place_block();
        game.set_breaking(true);
        for _ in 0..30 {
            game.advance(TICK_DT);
        }
        // A player joining after the load must not be handed this client's id.
        let body = *game.me.player();
        let next = game.server_mut().sim.join(body);
        assert_ne!(next, game.me_id, "an id was handed out twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_save_is_a_normal_first_run() {
        let mut game = game_with_assets();
        let empty = std::env::temp_dir().join("cubara-no-such-world-12345");
        let _ = std::fs::remove_dir_all(&empty);
        assert!(
            !game.load_from(&empty),
            "reported a load that did not happen"
        );
    }

    // ── The client's replica world (RESEARCH_MULTIPLAYER §8.2) ──────────────

    /// The claim the whole section rests on: **terrain is generated, never
    /// sent.** Two worlds built from one seed agree everywhere, and nothing
    /// crossed the seam to make that true.
    ///
    /// If this ever stops holding, the replica stops being affordable and the
    /// design changes -- so it is worth a test of its own rather than being
    /// implied by the ones below.
    #[test]
    fn the_replica_generates_the_same_terrain_as_the_server_with_nothing_sent() {
        let (game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        assert_eq!(
            game.world().seed(),
            game.server().world.seed(),
            "same seed, which is all the client is given"
        );
        for y in -40..80 {
            assert_eq!(
                game.world().is_solid_at(3, y, 7, terrain),
                game.server().world.is_solid_at(3, y, 7, terrain),
                "the two worlds disagree about (3, {y}, 7)"
            );
        }
    }

    /// They are genuinely two worlds, not one behind an accessor.
    ///
    /// An edit written straight into the server's world -- bypassing the
    /// journal, which is the one thing production code may never do -- must
    /// **not** appear on the client. That is what proves there is no in-process
    /// shortcut left: if this test fails, `Game::world()` is the server's world
    /// again and the seam is decorative.
    #[test]
    fn an_edit_that_skips_the_journal_never_reaches_the_client() {
        let (mut game, _) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        let at = [11, 30, 11];
        let stone = game
            .server_mut()
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:stone")
            .unwrap();

        Arc::make_mut(&mut game.server_mut().world).set_block(at[0], at[1], at[2], stone);
        game.settle();

        assert!(
            game.server()
                .world
                .is_solid_at(at[0], at[1], at[2], terrain),
            "the server has it"
        );
        assert!(
            !game.world().is_solid_at(at[0], at[1], at[2], terrain),
            "and the client was never told, because nothing told it"
        );
    }

    /// The ordinary path: an edit made through the server reaches the replica,
    /// and the chunk the client must re-mesh is the one it worked out itself.
    #[test]
    fn an_edit_reaches_the_replica_and_names_its_own_dirty_chunk() {
        let (mut game, ground) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");

        game.server_mut().set_block(ground, BlockId::AIR);
        let dirty = game.settle_dirty();

        assert!(
            !game
                .world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the replica applied the edit"
        );
        assert_eq!(
            dirty,
            vec![ChunkCoord::from_block(ground[0], ground[1], ground[2])],
            "and derived the stale chunk from its own world, not from the server"
        );
    }

    /// A catch-up burst that edits one chunk repeatedly is one re-mesh, not
    /// five. `sync` runs once after the whole burst, which is where that
    /// falls out.
    #[test]
    fn a_burst_of_edits_in_one_chunk_is_one_dirty_chunk() {
        let (mut game, ground) = game_looking_at_ground();
        for dy in 0..4 {
            game.server_mut()
                .set_block([ground[0], ground[1] - dy, ground[2]], BlockId::AIR);
        }
        assert_eq!(
            game.settle_dirty().len(),
            1,
            "four edits, one chunk, one re-mesh"
        );
    }

    /// The furnace screen is drawn from the **replica**, so a furnace smelting
    /// away has to be replicated every tick it changes -- otherwise the panel
    /// would freeze the moment it opened.
    ///
    /// This is the block-entity half of §8.3 doing real work rather than being
    /// a message type nobody sends.
    #[test]
    fn a_smelting_furnace_updates_the_clients_screen_through_block_entity_effects() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 1));
            f.fuel = Some((log, 4));
        });

        let before = game.open_furnace().expect("open");
        for _ in 0..210 {
            game.advance(TICK_DT);
        }
        let after = game.open_furnace().expect("still open");

        assert_ne!(
            before, after,
            "the client's copy of the furnace moved with the server's"
        );
        assert_eq!(
            after,
            game.server().world.furnace_at(pos).copied().unwrap(),
            "and matches it exactly"
        );
    }

    /// The join handshake (§8.3): a replica with nothing in it is brought up to
    /// date from a snapshot, not from a delta -- because there is no delta from
    /// a world it has never seen.
    ///
    /// Driven the way a load does it, since that is the one thing today that
    /// replaces a world wholesale.
    #[test]
    fn a_snapshot_rebuilds_a_replica_that_has_seen_nothing() {
        let (mut game, ground) = game_looking_at_ground();
        let terrain = game.server().terrain.expect("assets are set");
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 2)));
        game.server_mut().set_block(ground, BlockId::AIR);
        game.settle();

        // Throw the replica away, exactly as a load does, and rejoin.
        game.world = Arc::new(World::with_seed(game.server().world.seed()));
        assert!(
            game.world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the fresh replica has the untouched terrain"
        );
        game.resync();

        assert!(
            !game
                .world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the snapshot carried the edit"
        );
        assert_eq!(
            game.world().furnace_at(pos).copied(),
            game.server().world.furnace_at(pos).copied(),
            "and the block entity"
        );
    }

    /// Playing must keep the two worlds in step. Mine a block by holding the
    /// button, the way the game does, and the replica ends up agreeing with the
    /// server about every edit in the chunk.
    #[test]
    fn mining_through_the_action_path_keeps_the_two_worlds_in_step() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        let ticks = break_ticks(&game, "cubara:stone", "cubara:stone_pick");
        mine_for(&mut game, ticks * 3).expect("it broke");

        let server_edits: Vec<_> = game.server().world.edits().collect();
        let client_edits: Vec<_> = game.world().edits().collect();
        assert_eq!(
            server_edits, client_edits,
            "the replica saw every edit the server made, and no others"
        );
    }
}

#[cfg(test)]
mod connect_tests {
    use super::*;

    /// A client leaves its own world and joins one running elsewhere.
    ///
    /// Driven against a `Session` in this process rather than a spawned binary:
    /// what is being checked is that `Game` can live with no host at all, and a
    /// local link proves that as well as a socket does while keeping the test
    /// deterministic. `crates/server/tests/two_processes.rs` is where a real
    /// socket is exercised.
    fn hosted_elsewhere() -> (Session, Config) {
        let cfg = Config {
            world: std::path::PathBuf::from("cubara-nonexistent-connect-fixture"),
            autosave_ticks: 0,
            ..Config::default()
        };
        (Session::open(&cfg), cfg)
    }

    fn a_client() -> Game {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        game
    }

    /// Closing the inventory is **refused** while the crafting grid cannot
    /// empty, and refusing it is what stops items being eaten.
    ///
    /// `Crafting::close` reports whether everything fitted. Ignoring that answer
    /// and closing anyway narrows the grid back to 2x2, which drops whatever is
    /// in the outer cells — silently, with no message and nothing on the floor.
    ///
    /// Found by `scripts/check-tests-can-fail.sh`: the guard was there and
    /// nothing could see it removed.
    #[test]
    fn the_inventory_refuses_to_close_on_a_grid_that_cannot_empty() {
        let mut game = a_client();
        let items = load_item_registry();
        let plank = items.id_of("cubara:plank").expect("plank is an item");
        // **Full** stacks. A first version filled every slot with a stack of
        // one, and the grid emptied fine -- a plank stacks to 64, so there was
        // room for it in any of them. "The inventory is full" means no slot can
        // take another of this item, not that every slot is occupied.
        let full = items
            .new_stack(plank, items.max_stack(plank))
            .expect("a full stack");
        let stack = items.new_stack(plank, 1).expect("a stack");
        {
            let inv = &mut game.me.player_mut().inventory;
            for i in 0..cubara_sim::SLOT_COUNT {
                inv.set_slot(i, Some(full));
            }
        }
        // A bench-sized grid with something in a cell only 3x3 has.
        {
            let crafting = &mut game.me.player_mut().crafting;
            crafting.set_width(3);
            crafting.set_cell(8, Some(stack));
        }
        game.inventory_open = true;

        game.toggle_inventory();

        assert!(
            game.inventory_open(),
            "the screen closed over a grid that could not empty"
        );
        assert_eq!(
            game.me.player().crafting.cell(8),
            Some(stack),
            "the item in the grid was eaten by closing the screen"
        );
    }

    /// Joining replaces the world, and **drops the host**: a machine that has
    /// connected somewhere is not also simulating a world nobody is in.
    #[test]
    fn joining_a_server_gives_up_the_one_this_client_was_hosting() {
        let (mut remote, cfg) = hosted_elsewhere();
        let mut game = a_client();
        assert!(game.host.is_some(), "a fresh client hosts its own world");

        let link = remote.attach();
        game.join_over(link).expect("the handshake is accepted");

        assert!(
            game.host.is_none(),
            "a connected client kept hosting a second world"
        );
        assert_eq!(
            game.world().seed(),
            remote.server.world.seed(),
            "the client generates the server's world, from the seed it was sent"
        );
        let _ = cfg;
    }

    /// Cracks are drawn from the host's count, which a joined client does not
    /// have. It must draw nothing -- `server()` would panic.
    #[test]
    fn a_joined_client_has_no_cracks_rather_than_a_crash() {
        let (mut remote, _cfg) = hosted_elsewhere();
        let mut game = a_client();
        let link = remote.attach();
        game.join_over(link).expect("joined");
        game.set_breaking(true);
        assert_eq!(game.cracking(), None);
    }

    /// The id comes from the server, not from an assumption.
    #[test]
    fn a_joined_client_drives_the_player_the_server_named() {
        let (mut remote, _cfg) = hosted_elsewhere();
        let mut game = a_client();

        let link = remote.attach();
        game.join_over(link).expect("joined");

        assert_ne!(
            game.me_id,
            PlayerId::LOCAL,
            "the joining client was handed the host's own player"
        );
        assert!(
            remote.server.sim.get(game.me_id).is_some(),
            "the id the client believes is its own is not a player on the server"
        );
    }

    /// A server whose assets are not ours is refused, and the refusal says which.
    ///
    /// Ids cross the wire raw, so a mismatch is not a cosmetic difference: every
    /// id from the first differing name onward means something else, and stone
    /// arrives as iron. Refusing is the only honest answer, and naming the
    /// registry is the difference between a minute and an evening.
    #[test]
    fn a_server_with_different_assets_is_refused_by_name() {
        let (mut remote, _cfg) = hosted_elsewhere();
        let mut game = a_client();

        // Pretend this client's item table is not the server's.
        let wrong = cubara_voxel::ItemRegistry::from_defs(vec![(
            std::path::PathBuf::from("x.ron"),
            cubara_voxel::ItemDef {
                name: "cubara:not_a_real_item".to_string(),
                max_stack: 64,
                durability: None,
                tier: 0,
                speed: None,
                burn_ticks: None,
                rarity: cubara_voxel::Rarity::Common,
            },
        )])
        .expect("valid");
        game.assets.as_mut().unwrap().items = wrong;

        let link = remote.attach();
        let err = game
            .join_over(link)
            .expect_err("a mismatch must be refused");
        assert!(
            err.contains("items"),
            "the refusal must say which registry differs; got {err:?}"
        );
        assert!(
            game.host.is_some(),
            "a refused join must leave this client hosting the world it had"
        );
    }
}
