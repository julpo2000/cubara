//! The phase 2 exit gate's own "real gate" (`ROADMAP.md`).
//!
//! > *The survival replay test: a fixed, scripted input sequence runs headlessly
//! > and completes the loop — chop a tree, craft a tool, mine iron ore, smelt
//! > it, take damage — then asserts a world-state hash. It runs single-threaded
//! > and multi-threaded and must agree.*
//!
//! `ROADMAP.md` says why it is the criterion that matters: *if a scripted agent
//! can survive in the world without a human, the world is playable; if it
//! cannot, no screenshot proves it is.* Every other phase 2 test checks one
//! system in isolation. This is the only place where worldgen, the item
//! registry, the drop and tier rules, the recipe matcher, block entities, the
//! tick loop and the damage model all have to be simultaneously right, in that
//! order, against the **real shipped assets** rather than a synthetic fixture.
//!
//! # Why "eat" is not in that list
//!
//! It was, until the project owner decided on 2026-09-05 that hunger, food and
//! hostile mobs are deferred out of phase 2 — block 2.9b, see
//! `docs/PHASE2_ARCHITECTURE.md` §9. `ROADMAP.md`'s gate was amended in the same
//! change, deliberately and in the open, because moving a gate is the owner's
//! call and nobody else's. "Take damage" survives as a scripted fall, which
//! since block 2.9a is the only thing in this world that hurts.
//!
//! # What the script drives, and what it does not
//!
//! Every *action* goes through the real server path. `Action::Break`,
//! `Action::Place` and `Action::Interact` raycast from where the player is
//! actually standing and actually looking, and the look is steered by feeding
//! `InputFrame::look_delta` through `tick_sim` exactly as a client does.
//! Crafting goes through the recipe matcher, the furnace through
//! `Action::ClickFurnace` and `tick_world`, the damage through physics.
//!
//! What the script does *not* do is walk. It teleports between stations, for one
//! reason: a script that walked would be asserting that this seed's terrain is
//! traversable, which is not what the gate is about, and it would break on any
//! worldgen change for a reason having nothing to do with survival.
//!
//! The two buried stations — stone and iron ore — break through
//! [`Server::break_at`] rather than a raycast, because reaching them with a ray
//! would mean scripting a mineshaft. That is a smaller concession than it looks:
//! `break_at` is the documented shared tail that *both* the instant and the
//! timed break funnel into, so the drop rule, the tier gate and the tool wear
//! this test cares about are the same code either way. The raycast path is
//! exercised above ground, where covering it is honest, and
//! [`every_station_of_the_loop_actually_ran`] asserts that it really was.

use cubara_server::{Action, Effect, FurnaceSlot, Screen, Server};
use cubara_sim::{InputFrame, Player, MAX_HEALTH, SLOT_COUNT};
use cubara_voxel::{Angle, BlockId, Fixed, FixedVec3, ItemId, MAX_GRID};

/// A quarter turn. `Angle` is a turn split into 2³², so every cardinal
/// direction is an exact constant and **no trigonometry happens in this file**.
/// That is deliberate: `atan2` is exactly the kind of function
/// `docs/RESEARCH_MULTIPLAYER.md` §3.5 says two platforms may disagree about,
/// and the pinned hash below is what would notice.
const QUARTER: i32 = 1_073_741_824;

/// An eighth of a turn — 45° down, which is how you look at the ground in front
/// of you. Comfortably inside `PITCH_LIMIT` (about 88°).
const EIGHTH: i32 = QUARTER / 2;

/// Every stance the script will try when it wants to look at a block:
/// `(yaw, pitch, where to stand relative to the target)`.
///
/// `look_dir` is `[cos(p)·sin(y), sin(p), −cos(p)·cos(y)]`, so yaw 0 looks along
/// −Z and therefore wants to stand at +Z, and a negative pitch looks downward.
/// The first four are level looks at a block's sides; the last four look down at
/// it from above and behind, which is the only way to see the **top** face of a
/// block set into flat ground — and the top face is where you put things down.
///
/// Exact constants, every one: no `atan2` anywhere in this file, deliberately.
const STANCES: [(i32, i32, [i32; 3]); 8] = [
    (0, 0, [0, 0, 2]),
    (QUARTER, 0, [-2, 0, 0]),
    (QUARTER.wrapping_mul(2), 0, [0, 0, -2]),
    (QUARTER.wrapping_mul(3), 0, [2, 0, 0]),
    (0, -EIGHTH, [0, 2, 2]),
    (QUARTER, -EIGHTH, [-2, 2, 0]),
    (QUARTER.wrapping_mul(2), -EIGHTH, [0, 2, -2]),
    (QUARTER.wrapping_mul(3), -EIGHTH, [2, 2, 0]),
];

/// The world-state hash the whole script lands on.
///
/// **Pinned, not merely self-consistent.** Two runs of the same binary agreeing
/// proves nothing about a *third* machine, and this test's whole value is that
/// Windows CI and macOS CI must reach the same number. A cross-platform
/// divergence anywhere in worldgen, the assigned item ids, the tick order or the
/// damage model surfaces here as a failure on one OS and not the other, which
/// `ARCHITECTURE.md` Rule 1 calls a CI failure rather than a paragraph.
///
/// If this value moves, something about the simulation moved. Update it only
/// together with a sentence saying what moved and why that is correct.
///
/// **Moved once, in block 2.10** (`0x6B0A_E217_5FC1_296D` before it). The world
/// holds many players now, so the hash folds a player *count*, the id counter,
/// and each player's id alongside their state — where it used to fold one
/// player's fields bare. The run this test drives is unchanged and its single
/// player ends in exactly the same condition; what changed is the encoding
/// around them, which is the whole point of a version-style bump being visible
/// here.
const KNOWN_SURVIVAL_HASH: u64 = 0x7E1A_65EB_A55A_BB2D;

/// How many logs the script fells: three become planks, three become furnace
/// fuel. Oak burns 80 ticks and an ingot takes 200, so three is the smallest
/// number that finishes the smelt.
const LOGS_WANTED: usize = 6;

/// Cobble the script needs: three for the stone pick, eight for the furnace.
const COBBLE_WANTED: usize = 11;

/// How many of the felled logs must come down through the full `Action::Break`
/// raycast rather than the buried fallback.
///
/// Not all six, and the reason is the game being honest rather than the fixture
/// being lax: an oak's upper trunk is enclosed by its own leaves, so no level
/// ray reaches it from two blocks away, and the script does not clear the canopy
/// first. Three is what the default seed's nearest oak actually offers from
/// outside. The assertion's job is to prove the raycast path is genuinely
/// exercised — not that it is the only path used, which it never was.
const MIN_RAYCAST_BREAKS: usize = 3;

/// How far the player falls at the damage station. Well past `SAFE_FALL` (3),
/// and well short of lethal, so the assertion is "hurt", not "respawned".
const FALL_BLOCKS: i32 = 12;

/// A grid cell, by row and column. The grid is always backed by `MAX_GRID`
/// columns whatever its current width, so a row is not `width` apart.
const fn cell(row: usize, col: usize) -> usize {
    row * MAX_GRID + col
}

/// A scripted survival run, plus the bookkeeping that proves it happened.
struct Fixture {
    server: Server,
    /// Tracked rather than read back: `Player`'s angles are `pub(crate)`, and
    /// the fixture only ever moves them by the same `InputFrame` a client sends,
    /// so it always knows what they are.
    yaw: Angle,
    pitch: Angle,
    /// Breaks that went through `Action::Break`'s raycast rather than
    /// `break_at`. Asserted on, so the raycast path cannot quietly stop being
    /// covered.
    raycast_breaks: usize,
    /// The stations the script completed, in order. A script that silently
    /// skipped smelting must not be able to pass by hashing a world in which it
    /// never happened.
    log: Vec<&'static str>,
}

impl Fixture {
    /// A fresh **default-seed** world with the real assets loaded, the player
    /// standing on the ground and looking due −Z.
    ///
    /// The default seed, not a bespoke one: it is the world a player actually
    /// gets, and it happens to put a tree, stone and iron ore all within forty
    /// blocks of spawn. A survival test on a seed nobody plays would be a
    /// weaker claim about the game.
    fn new() -> Self {
        let mut server = Server::new();
        // A path that cannot exist, so this is always a fresh world rather than
        // whatever the developer last played.
        server.open(std::path::Path::new("cubara-nonexistent-survival-fixture"));
        // Known angles to count deltas from. `Server::new`'s default look is a
        // presentation choice; this test needs an arithmetic one.
        let pos = server.sim.player(server.local).pos;
        *server.sim.player_mut(server.local) = Player::new(pos, Angle::ZERO, Angle::ZERO);
        server.place_player_on_ground();
        Self {
            server,
            yaw: Angle::ZERO,
            pitch: Angle::ZERO,
            raycast_breaks: 0,
            log: Vec::new(),
        }
    }

    fn item_id(&self, name: &str) -> ItemId {
        self.server
            .items
            .as_ref()
            .expect("assets loaded")
            .id_of(name)
            .unwrap_or_else(|| panic!("the shipped assets define no item {name}"))
    }

    /// How many of `name` the player is carrying, across every slot.
    fn carrying(&self, name: &str) -> u32 {
        let Some(items) = self.server.items.as_ref() else {
            return 0;
        };
        let Some(id) = items.id_of(name) else {
            return 0;
        };
        self.server
            .sim
            .player(self.server.local)
            .inventory
            .slots()
            .flatten()
            .filter(|s| s.item() == id)
            .map(|s| s.count() as u32)
            .sum()
    }

    /// One tick of the whole server, with `input`.
    fn tick(&mut self, input: &InputFrame) {
        self.server.tick_sim(input);
        self.server.tick_world();
    }

    /// Turn to face `yaw`/`pitch` the way a client does — as a `look_delta` on a
    /// real input frame, never by writing the field.
    ///
    /// `apply_look` *adds* the yaw delta and *subtracts* the pitch one (screen y
    /// grows downward), which is why the pitch term is the other way round.
    fn look(&mut self, yaw: Angle, pitch: Angle) {
        let input = InputFrame {
            look_delta: [yaw.wrapping_sub(self.yaw), self.pitch.wrapping_sub(pitch)],
            ..InputFrame::default()
        };
        self.tick(&input);
        self.yaw = yaw;
        self.pitch = pitch;
    }

    /// Put the player at the centre of block `b`, standing still.
    ///
    /// Velocity and fall distance are cleared, because a teleport is not a fall
    /// — leaving them set would hand the damage station a fall the player never
    /// made, and the damage station is supposed to earn its damage.
    fn stand_at(&mut self, b: [i32; 3]) {
        let half = Fixed::from_raw(cubara_voxel::fixed::ONE / 2);
        let p = self.server.sim.player_mut(self.server.local);
        p.pos = FixedVec3::from_blocks(b[0], b[1], b[2]) + FixedVec3::new(half, half, half);
        p.velocity = FixedVec3::ZERO;
        p.fall_distance = Fixed::ZERO;
    }

    fn block_at(&self, b: [i32; 3]) -> BlockId {
        self.server
            .world
            .block_at(b[0], b[1], b[2], self.server.terrain())
    }

    fn is(&self, b: [i32; 3], name: &str) -> bool {
        self.server
            .blocks_registry
            .as_deref()
            .and_then(|r| r.name_of(self.block_at(b)))
            == Some(name)
    }

    /// What the server's own raycast says the player is looking at right now —
    /// the block and the face, exactly as `Action::Break` and `Action::Place`
    /// will read them a moment later.
    fn looking_at(&self) -> Option<([i32; 3], [i32; 3])> {
        let origin = self.server.sim.player(self.server.local).pos.to_f32();
        let dir = self
            .server
            .sim
            .player(self.server.local)
            .look_dir_f32()
            .to_array();
        self.server
            .world
            .raycast(origin, dir, cubara_sim::REACH, self.server.terrain())
            .map(|h| (h.block, h.normal))
    }

    /// Take up a stance from which the player really is looking at `target` —
    /// and, when `face` is given, at that particular face of it.
    ///
    /// Every stance is *verified* against the world's own raycast rather than
    /// predicted, which is the only way to be sure: whether a given look reaches
    /// a block depends on what is between them, and that is terrain, not
    /// geometry the fixture can reason about. `false` means nothing worked,
    /// which is the buried case.
    fn aim(&mut self, target: [i32; 3], face: Option<[i32; 3]>) -> bool {
        for (yaw, pitch, offset) in STANCES {
            let from = [
                target[0] + offset[0],
                target[1] + offset[1],
                target[2] + offset[2],
            ];
            if self.block_at(from) != BlockId::AIR {
                continue;
            }
            self.stand_at(from);
            self.look(Angle::from_raw(yaw), Angle::from_raw(pitch));
            let Some((hit, normal)) = self.looking_at() else {
                continue;
            };
            if hit == target && face.is_none_or(|f| f == normal) {
                return true;
            }
        }
        false
    }

    /// Look at `target` from any direction at all.
    fn face(&mut self, target: [i32; 3]) -> bool {
        self.aim(target, None)
    }

    /// Break `target` by standing next to it and clicking — the full
    /// `Action::Break` path, raycast and reach included.
    fn chop(&mut self, target: [i32; 3]) -> bool {
        if !self.face(target) {
            return false;
        }
        self.server.apply(Action::Break);
        self.raycast_breaks += 1;
        true
    }

    /// Break `target` outright, for blocks no ray can reach.
    fn dig(&mut self, target: [i32; 3]) {
        self.server.break_at(target);
    }

    /// Every block of `name` within `radius` of the player, nearest first.
    ///
    /// Sorted by `(distance, x, y, z)` — a total order, so *which* log the
    /// script fells is fixed rather than whatever the scan reached first.
    fn find(&self, name: &str, radius: i32) -> Vec<[i32; 3]> {
        let p = self.server.sim.player(self.server.local).pos.to_f32();
        let (cx, cy, cz) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let mut found = Vec::new();
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let b = [cx + dx, cy + dy, cz + dz];
                    if self.is(b, name) {
                        found.push((dx.abs() + dy.abs() + dz.abs(), b));
                    }
                }
            }
        }
        found.sort();
        found.into_iter().map(|(_, b)| b).collect()
    }

    /// Move one `id` out of the inventory, reporting whether it was there.
    fn take_one(&mut self, id: ItemId) -> bool {
        for slot in 0..SLOT_COUNT {
            let Some(items) = self.server.items.as_ref() else {
                return false;
            };
            if self
                .server
                .sim
                .player_mut(self.server.local)
                .inventory
                .slot(slot)
                .map(|s| s.item())
                != Some(id)
            {
                continue;
            }
            if self
                .server
                .sim
                .player_mut(self.server.local)
                .inventory
                .take_one(slot, items)
                .is_some()
            {
                return true;
            }
        }
        false
    }

    /// Hold `name` in the hotbar's first slot and select it — what the tier
    /// check in `break_at` reads.
    fn equip(&mut self, name: &str) {
        let id = self.item_id(name);
        assert!(self.take_one(id), "nothing to equip: no {name}");
        // Whatever was already in the first hotbar slot goes back into the bag.
        // Overwriting it would silently destroy items, and the script would then
        // fail several stations later at the point where they were needed —
        // exactly the kind of bug this fixture exists to catch, so it must not
        // be one the fixture itself commits.
        //
        // **The order matters.** Stowing before the tool goes in puts the
        // displaced stack straight back into the slot it just left — `add`
        // fills the first free slot, and that is the one — where `set_slot`
        // then destroys it. Seat the tool first, and the stow lands elsewhere.
        let displaced = self
            .server
            .sim
            .player_mut(self.server.local)
            .inventory
            .take(0);
        let items = self.server.items.as_ref().expect("assets loaded");
        let stack = items.new_stack(id, 1).expect("one of anything is a stack");
        self.server
            .sim
            .player_mut(self.server.local)
            .inventory
            .set_slot(0, Some(stack));
        self.server
            .sim
            .player_mut(self.server.local)
            .inventory
            .select(0);
        if let Some(displaced) = displaced {
            let items = self.server.items.as_ref().expect("assets loaded");
            let rest = self
                .server
                .sim
                .player_mut(self.server.local)
                .inventory
                .add(displaced, items);
            assert!(
                rest.is_none(),
                "no room to stow what the hotbar was holding"
            );
        }
    }

    /// Lay `pattern` into the crafting grid and take the result, `times` over,
    /// banking each result into the inventory.
    ///
    /// `pattern` is `(cell index, item name)`. Matching trims first, so where in
    /// the grid it sits does not matter — only its shape does.
    fn craft(&mut self, pattern: &[(usize, &str)], times: usize, what: &'static str) {
        for _ in 0..times {
            for &(index, name) in pattern {
                let id = self.item_id(name);
                assert!(self.take_one(id), "ran out of {name} while crafting {what}");
                let items = self.server.items.as_ref().expect("assets loaded");
                let stack = items.new_stack(id, 1).expect("one is a stack");
                self.server
                    .sim
                    .player_mut(self.server.local)
                    .crafting
                    .set_cell(index, Some(stack));
            }
            // Disjoint field borrows: the registries hang off `server.items` and
            // `server.recipes`, the grid off `server.sim`.
            let items = self.server.items.as_ref().expect("assets loaded");
            let book = self.server.recipes.as_ref().expect("recipes loaded");
            let made = self
                .server
                .sim
                .player_mut(self.server.local)
                .crafting
                .take_result(book, items);
            assert!(made, "the grid did not match a recipe for {what}");

            let held = self
                .server
                .sim
                .player_mut(self.server.local)
                .crafting
                .held()
                .expect("a result");
            self.server
                .sim
                .player_mut(self.server.local)
                .crafting
                .set_held(None);
            let items = self.server.items.as_ref().expect("assets loaded");
            let rest = self
                .server
                .sim
                .player_mut(self.server.local)
                .inventory
                .add(held, items);
            assert!(rest.is_none(), "no room in the inventory to bank {what}");
        }
        self.log.push(what);
    }

    /// Put `item` down on top of `support` by looking at its upper face and
    /// clicking — `Action::Place`'s real path, which builds against the normal
    /// of whatever the ray hit. Returns where the block landed.
    fn place_on(&mut self, item: &str, support: [i32; 3]) -> [i32; 3] {
        let target = [support[0], support[1] + 1, support[2]];
        self.equip(item);
        assert!(
            self.aim(support, Some([0, 1, 0])),
            "no stance sees the top face of {support:?}, so {item} cannot go there"
        );
        self.server.apply(Action::Place);
        assert!(
            self.is(target, item),
            "{item} did not land at {target:?}; it went somewhere else"
        );
        target
    }

    /// Surface blocks of `name` with open sky directly above — somewhere a block
    /// can actually be put down. Nearest first, and at least `apart` blocks from
    /// each other so one placement cannot block the approach to the next.
    fn building_spots(&self, name: &str, radius: i32, apart: i32, want: usize) -> Vec<[i32; 3]> {
        let mut spots: Vec<[i32; 3]> = Vec::new();
        for b in self.find(name, radius) {
            let above = [b[0], b[1] + 1, b[2]];
            if self.block_at(above) != BlockId::AIR {
                continue;
            }
            if spots.iter().any(|s| {
                (s[0] - b[0]).abs() < apart
                    && (s[2] - b[2]).abs() < apart
                    && (s[1] - b[1]).abs() < apart
            }) {
                continue;
            }
            spots.push(b);
            if spots.len() == want {
                break;
            }
        }
        spots
    }
}

/// The whole loop, run once. Returns the finished world, for hashing.
fn run_survival_script() -> Fixture {
    let mut f = Fixture::new();

    // --- Chop a tree ------------------------------------------------------
    // Bare hands: oak needs no tier, which is what makes it the first rung.
    let logs = f.find("cubara:oak_log", 40);
    assert!(
        logs.len() >= LOGS_WANTED,
        "the default seed grew only {} logs within 40 blocks of spawn; the ladder \
         starts at a tree, so this fixture cannot run",
        logs.len()
    );
    for log in logs.iter().take(LOGS_WANTED) {
        if !f.chop(*log) {
            f.dig(*log);
        }
    }
    assert_eq!(
        f.carrying("cubara:oak_log"),
        LOGS_WANTED as u32,
        "felling {LOGS_WANTED} logs did not yield {LOGS_WANTED} logs"
    );
    assert!(
        f.raycast_breaks > 0,
        "not one log came down through the raycast path; the fixture proved nothing \
         about Action::Break"
    );
    f.log.push("chopped");

    // --- Craft: planks, a bench, then the bench's own 3x3 -----------------
    // Three logs become twelve planks; the other three are the furnace's fuel.
    f.craft(&[(cell(0, 0), "cubara:oak_log")], 3, "planks");
    f.craft(
        &[
            (cell(0, 0), "cubara:plank"),
            (cell(0, 1), "cubara:plank"),
            (cell(1, 0), "cubara:plank"),
            (cell(1, 1), "cubara:plank"),
        ],
        1,
        "bench",
    );

    // Put the bench down and open it. The 3x3 grid is world state that
    // `Interact` sets, not a screen the client invents — which is why a headless
    // test can reach it at all.
    let spots = f.building_spots("cubara:grass", 10, 3, 2);
    assert_eq!(
        spots.len(),
        2,
        "no two open patches of ground near spawn to build the bench and the \
         furnace on"
    );
    let on_top = f.place_on("cubara:crafting_bench", spots[0]);
    assert!(f.face(on_top), "cannot look at the bench just placed");
    f.server.apply(Action::Interact);
    let opened = f.server.drain_effects();
    assert!(
        opened.contains(&Effect::Open(Screen::Bench)),
        "interacting with the bench did not open it: {opened:?}"
    );
    assert_eq!(
        f.server.sim.player_mut(f.server.local).crafting.width(),
        3,
        "the bench opened but the grid is still 2x2"
    );
    f.log.push("bench opened");

    // --- Craft a tool -----------------------------------------------------
    f.craft(
        &[(cell(0, 0), "cubara:plank"), (cell(1, 0), "cubara:plank")],
        1,
        "sticks",
    );
    f.craft(
        &[
            (cell(0, 0), "cubara:plank"),
            (cell(0, 1), "cubara:plank"),
            (cell(0, 2), "cubara:plank"),
            (cell(1, 1), "cubara:stick"),
            (cell(2, 1), "cubara:stick"),
        ],
        1,
        "wooden pick",
    );

    // --- Mine stone, which needs that tool --------------------------------
    f.equip("cubara:wooden_pick");
    let stone = f.find("cubara:stone", 20);
    assert!(
        stone.len() >= COBBLE_WANTED,
        "not enough stone within 20 blocks of spawn"
    );
    for b in stone.iter().take(COBBLE_WANTED) {
        f.dig(*b);
    }
    assert_eq!(
        f.carrying("cubara:cobble"),
        COBBLE_WANTED as u32,
        "stone did not come up as cobble — the wooden pick's tier is not being read"
    );
    f.log.push("mined stone");

    // --- Craft the stone pick and the furnace -----------------------------
    f.craft(
        &[
            (cell(0, 0), "cubara:cobble"),
            (cell(0, 1), "cubara:cobble"),
            (cell(0, 2), "cubara:cobble"),
            (cell(1, 1), "cubara:stick"),
            (cell(2, 1), "cubara:stick"),
        ],
        1,
        "stone pick",
    );
    f.craft(
        &[
            (cell(0, 0), "cubara:cobble"),
            (cell(0, 1), "cubara:cobble"),
            (cell(0, 2), "cubara:cobble"),
            (cell(1, 0), "cubara:cobble"),
            (cell(1, 2), "cubara:cobble"),
            (cell(2, 0), "cubara:cobble"),
            (cell(2, 1), "cubara:cobble"),
            (cell(2, 2), "cubara:cobble"),
        ],
        1,
        "furnace",
    );

    // --- Mine iron ore, which needs the stone pick ------------------------
    f.equip("cubara:stone_pick");
    let ore = f.find("cubara:iron_ore", 30);
    let vein = *ore
        .first()
        .expect("iron ore within 30 blocks of spawn; the ladder ends at iron");
    f.dig(vein);
    assert_eq!(
        f.carrying("cubara:raw_iron"),
        1,
        "iron ore yielded nothing to a stone pick — the tier gate is wrong"
    );
    f.log.push("mined iron");

    // --- Smelt it ---------------------------------------------------------
    let furnace_at = f.place_on("cubara:furnace", spots[1]);

    // Loading a furnace is a click with something on the cursor, so the ore and
    // the fuel go onto the cursor first — the same two steps a player takes.
    let raw = f.item_id("cubara:raw_iron");
    assert!(f.take_one(raw), "the raw iron went missing before smelting");
    let items = f.server.items.as_ref().expect("assets loaded");
    let ore_stack = items.new_stack(raw, 1).expect("one is a stack");
    f.server
        .sim
        .player_mut(f.server.local)
        .crafting
        .set_held(Some(ore_stack));
    f.server.apply(Action::ClickFurnace {
        pos: furnace_at,
        slot: FurnaceSlot::Input,
    });

    let log_id = f.item_id("cubara:oak_log");
    let fuel = f.carrying("cubara:oak_log") as u8;
    assert!(fuel >= 3, "only {fuel} logs left; a 200-tick smelt needs 3");
    for _ in 0..fuel {
        assert!(f.take_one(log_id), "counted more logs than there were");
    }
    let items = f.server.items.as_ref().expect("assets loaded");
    let fuel_stack = items.new_stack(log_id, fuel).expect("a stack of logs");
    f.server
        .sim
        .player_mut(f.server.local)
        .crafting
        .set_held(Some(fuel_stack));
    f.server.apply(Action::ClickFurnace {
        pos: furnace_at,
        slot: FurnaceSlot::Fuel,
    });
    assert!(
        f.server
            .sim
            .player_mut(f.server.local)
            .crafting
            .held()
            .is_none(),
        "the furnace did not take the fuel"
    );

    // Run it. 200 ticks of smelting plus slack; the loop stops the moment the
    // ingot exists, so the bound is a guard against hanging, not a wait.
    let ingot = f.item_id("cubara:iron_ingot");
    let mut smelted = false;
    for _ in 0..400 {
        f.tick(&InputFrame::default());
        if f.server
            .world
            .furnace_at(furnace_at)
            .and_then(|fu| fu.output)
            .map(|(id, _)| id)
            == Some(ingot)
        {
            smelted = true;
            break;
        }
    }
    assert!(smelted, "400 ticks and the furnace produced no iron ingot");

    // Take it out, and bank it.
    f.server
        .sim
        .player_mut(f.server.local)
        .crafting
        .set_held(None);
    f.server.apply(Action::ClickFurnace {
        pos: furnace_at,
        slot: FurnaceSlot::Output,
    });
    let held = f
        .server
        .sim
        .player_mut(f.server.local)
        .crafting
        .held()
        .expect("the output slot handed the ingot over");
    f.server
        .sim
        .player_mut(f.server.local)
        .crafting
        .set_held(None);
    let items = f.server.items.as_ref().expect("assets loaded");
    let rest = f
        .server
        .sim
        .player_mut(f.server.local)
        .inventory
        .add(held, items);
    assert!(rest.is_none(), "no room to bank the ingot");
    assert_eq!(
        f.carrying("cubara:iron_ingot"),
        1,
        "the ingot did not reach the inventory"
    );
    f.log.push("smelted");

    // --- Take damage ------------------------------------------------------
    // A fall, which since block 2.9a is the one thing in this world that hurts.
    assert_eq!(
        f.server.sim.player_mut(f.server.local).health,
        MAX_HEALTH,
        "the script hurt the player before the station that is supposed to"
    );
    let drop_from = [on_top[0], on_top[1] + FALL_BLOCKS, on_top[2]];
    f.stand_at(drop_from);
    let mut landed = false;
    for _ in 0..200 {
        f.tick(&InputFrame::default());
        if f.server.sim.player_mut(f.server.local).on_ground {
            landed = true;
            break;
        }
    }
    assert!(landed, "the player never landed");
    assert!(
        f.server.sim.player_mut(f.server.local).health < MAX_HEALTH,
        "a {FALL_BLOCKS}-block fall cost nothing; SAFE_FALL is 3"
    );
    assert!(
        f.server.sim.player_mut(f.server.local).health > 0,
        "the fall was meant to hurt, not to kill"
    );
    f.log.push("took damage");

    f
}

/// The gate's criterion, in one test: the loop completes, and the world it
/// leaves behind hashes the same however many threads compute it.
#[test]
fn the_survival_loop_completes_and_hashes_the_same_at_any_worker_count() {
    let f = run_survival_script();

    let single = f.server.hash_with_workers(1);
    // A fixed count, not `available_parallelism()`: on a CI runner reporting one
    // core that would silently become the same code path as "forced to one" and
    // prove nothing about merge order. The same reasoning as the phase 1
    // determinism harness, and the same number.
    let multi = f.server.hash_with_workers(6);
    assert_eq!(
        single, multi,
        "the world-state hash after a full survival run depends on how many \
         threads computed it"
    );

    assert_eq!(
        single.value(),
        KNOWN_SURVIVAL_HASH,
        "the survival run's world hash changed. If this fires on only one of \
         macOS/Windows CI, the simulation has diverged cross-platform, which \
         ARCHITECTURE.md Rule 1 calls a CI failure rather than a paragraph. If it \
         fires on both, something in worldgen, the assets, the tick order or the \
         damage model moved — say what, and why the new value is right."
    );
}

/// The same script twice must land in the same place. Cheap to state, and it is
/// what separates "deterministic" from "happened to agree with itself once".
#[test]
fn two_runs_of_the_same_script_agree() {
    let a = run_survival_script();
    let b = run_survival_script();
    assert_eq!(
        a.server.hash().value(),
        b.server.hash().value(),
        "two identical scripted runs reached different worlds"
    );
}

/// Guards against a vacuous pass. A hash assertion cannot tell the difference
/// between "the loop ran" and "nothing happened, twice", so the stations report
/// themselves and this checks the register.
#[test]
fn every_station_of_the_loop_actually_ran() {
    let f = run_survival_script();
    assert_eq!(
        f.log,
        vec![
            "chopped",
            "planks",
            "bench",
            "bench opened",
            "sticks",
            "wooden pick",
            "mined stone",
            "stone pick",
            "furnace",
            "mined iron",
            "smelted",
            "took damage",
        ],
        "the survival loop did not run its stations in order"
    );
    assert!(
        f.raycast_breaks >= MIN_RAYCAST_BREAKS,
        "only {} of {LOGS_WANTED} logs came down through Action::Break; at least          {MIN_RAYCAST_BREAKS} must, or the raycast path is not being covered",
        f.raycast_breaks
    );
    assert_eq!(f.carrying("cubara:iron_ingot"), 1, "no ingot at the end");
    assert!(
        f.server.sim.player(f.server.local).health < MAX_HEALTH,
        "nothing hurt"
    );
}
