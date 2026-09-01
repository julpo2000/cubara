//! Save/load: `level.ron` (the world header) and the save directory as a
//! whole (`docs/PHASE1_ARCHITECTURE.md` §7, issue #60).
//!
//! ```text
//! saves/<world>/
//!   level.ron                      # seed, tick, RNG, player, block id table
//!   region/r.<rx>.<ry>.<rz>.cbr    # 32×32×32 chunks = 512³ blocks
//! ```
//!
//! Lives here, in `cubara-sim`, not in `cubara-world` -- the header carries
//! tick/RNG/player state, and `cubara-world` must never know about the
//! player (§1's crate table). `cubara_world::region` owns the chunk data
//! half; this module owns the header and ties the two together. Same
//! reasoning [`crate::WorldHash`] (block 1.8) already established for why
//! this crate, not that one.

use crate::crafting::Crafting;
use crate::inventory::Inventory;
use std::collections::HashMap;
use std::path::Path;

use cubara_voxel::{
    Angle, BlockId, BlockRegistry, ChunkCoord, ItemId, ItemRegistry, ItemStack, ItemState,
};
use cubara_world::{region, TerrainBlocks, World, WORLDGEN_VERSION};
use serde::{Deserialize, Serialize};

use crate::player::Player;
use crate::rng::WorldRng;
use crate::Sim;

/// `level.ron`'s own schema version -- independent of
/// [`cubara_world::region::REGION_FORMAT_VERSION`] and of
/// [`cubara_world::WORLDGEN_VERSION`]; each names a different thing that
/// can change on its own schedule.
///
/// **4:** `yaw` and `pitch` are raw binary angles (`i32`) rather than radians
/// (`f32`). A save is world state, and world state is integers
/// (`docs/RESEARCH_MULTIPLAYER.md` §3.5). A version-3 save cannot be read as a
/// version-4 one -- the fields have the same names and different meanings,
/// which is exactly the case a version number exists for.
pub const FORMAT_VERSION: u16 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedRng {
    state: u64,
    inc: u64,
}

/// One item stack, by **name** rather than by id.
///
/// Item ids are assigned per registry by sorted name (§1.2), exactly like block
/// ids -- so a save storing raw ids would silently mean *different items* the
/// moment an item file is added or removed. The header's id table exists to
/// bridge that, and this is the form every saved stack takes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedStack {
    item: String,
    count: u8,
    /// `None` for a plain stack; `Some(n)` for a tool with `n` uses left.
    durability: Option<u16>,
}

/// A furnace, by position (§7, §8.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedFurnace {
    pos: (i32, i32, i32),
    input: Option<SavedStack>,
    fuel: Option<SavedStack>,
    output: Option<SavedStack>,
    burning: u32,
    progress: u32,
}

/// One item lying on the ground (§10.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedEntity {
    key: u64,
    stack: SavedStack,
    pos: (i64, i64, i64),
    vel: (i64, i64, i64),
    age: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedPlayer {
    pos: (i64, i64, i64),
    vel: (i64, i64, i64),
    /// Raw binary angles (`cubara_voxel::Angle`), not radians -- a save is
    /// world state, and world state is integers (§3.5).
    yaw: i32,
    pitch: i32,
    on_ground: bool,
    free_fly: bool,
    /// 36 entries, `None` for an empty slot. Block 2.8.
    #[serde(default)]
    inventory: Vec<Option<SavedStack>>,
    #[serde(default)]
    selected_slot: u8,
    /// The 3x3 crafting grid, its usable width, and whatever the cursor holds.
    #[serde(default)]
    grid: Vec<Option<SavedStack>>,
    #[serde(default = "default_grid_width")]
    grid_width: usize,
    #[serde(default)]
    held: Option<SavedStack>,
    /// Health in points, and the regeneration counter (§13.5). Block 2.9a.
    #[serde(default = "full_health")]
    health: u8,
    #[serde(default)]
    ticks_since_damage: u32,
    /// Where death returns the player to.
    #[serde(default)]
    spawn: (i64, i64, i64),
}

/// A save written before block 2.9a has no health field; a loaded player is
/// alive and well rather than dead on arrival.
fn full_health() -> u8 {
    crate::player::MAX_HEALTH
}

/// A save written before block 2.8 has no grid width; 2 is the inventory's own.
fn default_grid_width() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedHeader {
    format_version: u16,
    worldgen_version: u32,
    seed: u64,
    tick: u64,
    rng: SavedRng,
    player: SavedPlayer,
    /// The id → name table in force when this world was saved (§3.4, §7.2).
    /// Loading resolves each name against the *current* registry, so ids
    /// may be reassigned freely between runs.
    blocks: Vec<(u16, String)>,
    /// The item id → name table, for the same reason `blocks` exists (§8.1).
    #[serde(default)]
    items: Vec<(u16, String)>,
    /// Block entities, in position order (§7). In the world header rather than
    /// the chunk payload because that is where they live in memory: keyed by
    /// world position beside `World::edits`, since chunks are regenerated from
    /// the seed on load (§7.4) and so cannot carry player state.
    #[serde(default)]
    block_entities: Vec<SavedFurnace>,
    /// Items on the ground, and the counter that names them.
    #[serde(default)]
    entities: Vec<SavedEntity>,
    /// World state (§10.2): without it, keys would restart at 0 after a reload
    /// and two different histories could collide.
    #[serde(default)]
    next_entity_key: u64,
}

/// A save failed. Every variant names the problem.
#[derive(Debug)]
pub enum SaveError {
    Io(std::io::Error),
    Ron(ron::Error),
    Region(region::RegionError),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "save I/O error: {e}"),
            SaveError::Ron(e) => write!(f, "failed to write level.ron: {e}"),
            SaveError::Region(e) => write!(f, "failed to write region files: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// A load failed. Every variant names the problem -- in particular the two
/// hard errors §7.2 requires: [`LoadError::UnknownBlockName`] (a name the
/// current registry doesn't know -- a removed mod) and
/// [`LoadError::WorldgenVersionMismatch`] (the generator that made this
/// world isn't the one running now, so its unedited chunks would
/// regenerate into a different shape around the player's edits, §7.4).
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Ron(ron::de::SpannedError),
    Region(region::RegionError),
    /// This build's `level.ron` reader doesn't know this schema version.
    UnsupportedFormatVersion(u16),
    /// A block name the header's id table lists that the current registry
    /// doesn't have.
    UnknownBlockName(String),
    /// The generator that produced this world doesn't match the one
    /// running now.
    WorldgenVersionMismatch {
        expected: u32,
        found: u32,
    },
    /// A region file references a saved id the header's own table never
    /// listed -- a corrupt or hand-edited save, not a normal mismatch (that
    /// case is [`UnknownBlockName`], caught earlier, at the header).
    UnmappedChunkIds {
        chunk: ChunkCoord,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "load I/O error: {e}"),
            LoadError::Ron(e) => write!(f, "failed to parse level.ron: {e}"),
            LoadError::Region(e) => write!(f, "failed to read region files: {e}"),
            LoadError::UnsupportedFormatVersion(v) => write!(
                f,
                "level.ron format version {v} is not supported (expected {FORMAT_VERSION})"
            ),
            LoadError::UnknownBlockName(name) => write!(
                f,
                "save references block \"{name}\", which the current registry does not define \
                 (a removed mod?)"
            ),
            LoadError::WorldgenVersionMismatch { expected, found } => write!(
                f,
                "save was generated by worldgen version {found}, but this build is version \
                 {expected} -- refusing to load, since unedited chunks would regenerate \
                 differently around the player's edits"
            ),
            LoadError::UnmappedChunkIds { chunk } => write!(
                f,
                "chunk {chunk:?} references a block id not listed in level.ron's own id table \
                 (corrupt or hand-edited save)"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// A position in the saved form: **raw sub-units**, not blocks-as-float.
///
/// Saving `f32` would throw away the precision fixed-point exists to keep, and
/// would put a float back into a file two machines must agree on.
fn to_xyz(v: cubara_voxel::FixedVec3) -> (i64, i64, i64) {
    (v.x.raw(), v.y.raw(), v.z.raw())
}

/// The inverse of [`to_xyz`].
fn from_xyz(v: (i64, i64, i64)) -> cubara_voxel::FixedVec3 {
    cubara_voxel::FixedVec3::new(
        cubara_voxel::Fixed::from_raw(v.0),
        cubara_voxel::Fixed::from_raw(v.1),
        cubara_voxel::Fixed::from_raw(v.2),
    )
}

/// A furnace slot's `(id, count)` as an [`ItemStack`], so the same
/// [`to_saved`] path serialises it. Furnace slots never hold durability.
fn stack_of(slot: Option<(ItemId, u8)>, items: &ItemRegistry) -> Option<ItemStack> {
    let (id, count) = slot?;
    ItemStack::new(id, count, ItemState::None, items.max_stack(id)).ok()
}

/// A live stack in its saved form, or `None` for an empty slot.
fn to_saved(stack: Option<ItemStack>, items: &ItemRegistry) -> Option<SavedStack> {
    let stack = stack?;
    Some(SavedStack {
        item: items.name_of(stack.item())?.to_string(),
        count: stack.count(),
        durability: match stack.state() {
            ItemState::Durability { remaining } => Some(remaining),
            ItemState::None => None,
        },
    })
}

/// A saved stack resolved against *this* run's registry.
///
/// **An item whose name no longer exists is dropped, not fatal.** A world should
/// survive an item being renamed or removed; refusing to load the whole save is
/// a worse answer than a missing stack, and the same stance `with_oak` and
/// `with_ores` take for missing data.
fn from_saved(saved: &Option<SavedStack>, items: &ItemRegistry) -> Option<ItemStack> {
    let saved = saved.as_ref()?;
    let Some(id) = items.id_of(&saved.item) else {
        log::debug!("save names unknown item {:?}; dropping it", saved.item);
        return None;
    };
    let state = match saved.durability {
        Some(remaining) => ItemState::Durability { remaining },
        None => ItemState::None,
    };
    ItemStack::new(id, saved.count, state, items.max_stack(id)).ok()
}

/// Save `sim`/`world` to `dir` (created if it doesn't exist): `level.ron`
/// plus one region file per dirty region under `dir/region/`. `registry`
/// supplies the block id → name table (§7.2) and `items` the item one (§8.1);
/// `blocks` is what [`cubara_world::World::edited_chunk_at`] resolves
/// grass/soil/stone against, same as everywhere else that materializes a chunk.
pub fn save_world(
    dir: &Path,
    sim: &Sim,
    world: &World,
    registry: &BlockRegistry,
    items: &ItemRegistry,
    blocks: TerrainBlocks,
) -> Result<(), SaveError> {
    std::fs::create_dir_all(dir).map_err(SaveError::Io)?;

    let blocks_table: Vec<(u16, String)> = registry
        .ids()
        .filter_map(|id| registry.name_of(id).map(|name| (id.0, name.to_string())))
        .collect();

    let header = SavedHeader {
        format_version: FORMAT_VERSION,
        worldgen_version: WORLDGEN_VERSION,
        seed: world.seed(),
        tick: sim.tick,
        rng: SavedRng {
            state: sim.rng.state,
            inc: sim.rng.inc,
        },
        player: SavedPlayer {
            pos: to_xyz(sim.player.pos),
            vel: to_xyz(sim.player.velocity),
            yaw: sim.player.yaw.raw(),
            pitch: sim.player.pitch.raw(),
            on_ground: sim.player.on_ground,
            free_fly: sim.player.free_fly,
            inventory: (0..crate::inventory::SLOT_COUNT)
                .map(|i| to_saved(sim.player.inventory.slot(i), items))
                .collect(),
            selected_slot: sim.player.inventory.selected_slot(),
            grid: (0..cubara_voxel::MAX_GRID * cubara_voxel::MAX_GRID)
                .map(|i| to_saved(sim.player.crafting.cell(i), items))
                .collect(),
            grid_width: sim.player.crafting.width(),
            held: to_saved(sim.player.crafting.held(), items),
            health: sim.player.health,
            ticks_since_damage: sim.player.ticks_since_damage,
            spawn: to_xyz(sim.player.spawn),
        },
        blocks: blocks_table,
        items: items
            .ids()
            .filter_map(|id| items.name_of(id).map(|n| (id.0, n.to_string())))
            .collect(),
        block_entities: world
            .block_entities()
            .map(|(pos, f)| SavedFurnace {
                pos: (pos[0], pos[1], pos[2]),
                input: to_saved(stack_of(f.input, items), items),
                fuel: to_saved(stack_of(f.fuel, items), items),
                output: to_saved(stack_of(f.output, items), items),
                burning: f.burning,
                progress: f.progress,
            })
            .collect(),
        entities: sim
            .entities
            .sorted()
            .into_iter()
            .filter_map(|(key, d)| {
                Some(SavedEntity {
                    key: key.0,
                    stack: to_saved(Some(d.stack), items)?,
                    pos: to_xyz(d.pos),
                    vel: to_xyz(d.velocity),
                    age: d.age,
                })
            })
            .collect(),
        next_entity_key: sim.entities.next_key(),
    };

    let text = ron::ser::to_string_pretty(&header, ron::ser::PrettyConfig::default())
        .map_err(SaveError::Ron)?;
    std::fs::write(dir.join("level.ron"), text).map_err(SaveError::Io)?;

    region::save_regions(&dir.join("region"), world, blocks).map_err(SaveError::Region)?;
    Ok(())
}

/// Load a world saved by [`save_world`]. Regenerates every chunk
/// [`cubara_world::World::dirty_chunks`] didn't cover (§7.4's whole point);
/// `registry`/`blocks` play the same role as in `save_world`, resolved
/// against *this* run's registry, which need not assign the same ids the
/// save was made with -- that's exactly what the header's id table exists
/// to bridge (§7.2).
pub fn load_world(
    dir: &Path,
    registry: &BlockRegistry,
    items: &ItemRegistry,
    blocks: TerrainBlocks,
) -> Result<(Sim, World), LoadError> {
    let text = std::fs::read_to_string(dir.join("level.ron")).map_err(LoadError::Io)?;
    let header: SavedHeader = ron::from_str(&text).map_err(LoadError::Ron)?;

    if header.format_version != FORMAT_VERSION {
        return Err(LoadError::UnsupportedFormatVersion(header.format_version));
    }
    if header.worldgen_version != WORLDGEN_VERSION {
        return Err(LoadError::WorldgenVersionMismatch {
            expected: WORLDGEN_VERSION,
            found: header.worldgen_version,
        });
    }

    let mut remap: HashMap<u16, BlockId> = HashMap::with_capacity(header.blocks.len());
    for (saved_id, name) in &header.blocks {
        let runtime_id = registry
            .id_of(name)
            .ok_or_else(|| LoadError::UnknownBlockName(name.clone()))?;
        remap.insert(*saved_id, runtime_id);
    }

    let mut world = World::with_seed(header.seed);

    let region_dir = dir.join("region");
    if region_dir.is_dir() {
        for entry in std::fs::read_dir(&region_dir).map_err(LoadError::Io)? {
            let entry = entry.map_err(LoadError::Io)?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(region_coord) = region::parse_region_file_name(&name) else {
                continue;
            };
            let chunks =
                region::read_region_file(&entry.path(), region_coord).map_err(LoadError::Region)?;
            for (coord, chunk) in chunks {
                let remapped = chunk
                    .remap_ids(|id| remap.get(&id.0).copied())
                    .ok_or(LoadError::UnmappedChunkIds { chunk: coord })?;
                world.load_chunk_edits(coord, &remapped, blocks);
            }
        }
    }

    let player = Player {
        pos: from_xyz(header.player.pos),
        velocity: from_xyz(header.player.vel),
        on_ground: header.player.on_ground,
        free_fly: header.player.free_fly,
        yaw: Angle::from_raw(header.player.yaw),
        pitch: Angle::from_raw(header.player.pitch),
        // Block 2.8: restored by name, so a registry that assigns different ids
        // this run still lands the right items in the right slots (§8.1).
        inventory: {
            let mut inv = Inventory::new();
            for (i, saved) in header.player.inventory.iter().enumerate() {
                inv.set_slot(i, from_saved(saved, items));
            }
            inv.select(header.player.selected_slot);
            inv
        },
        health: header.player.health,
        ticks_since_damage: header.player.ticks_since_damage,
        // Transient and derived (§13.3): a loaded world starts the player on
        // the ground, and carrying a half-completed fall across a reload would
        // be a fall the player never made.
        fall_distance: cubara_voxel::Fixed::ZERO,
        spawn: from_xyz(header.player.spawn),
        crafting: {
            let mut c = Crafting::new(header.player.grid_width);
            for (i, saved) in header.player.grid.iter().enumerate() {
                c.set_cell(i, from_saved(saved, items));
            }
            c.set_held(from_saved(&header.player.held, items));
            c
        },
    };

    let sim = Sim {
        tick: header.tick,
        rng: WorldRng {
            state: header.rng.state,
            inc: header.rng.inc,
        },
        player,
        target: None, // recomputed by the first tick; not part of saved state
        entities: {
            // Block 2.8. Keys are restored as saved, not reassigned: an
            // `EntityKey` that came back would let two different histories hash
            // alike (§10.2).
            let mut e = crate::Entities::default();
            for saved in &header.entities {
                let Some(stack) = from_saved(&Some(saved.stack.clone()), items) else {
                    continue;
                };
                e.restore_item(
                    crate::EntityKey(saved.key),
                    crate::DroppedItem {
                        stack,
                        pos: from_xyz(saved.pos),
                        velocity: from_xyz(saved.vel),
                        age: saved.age,
                        // Recomputed on the first tick rather than saved: it is
                        // derived from the terrain under it, which is
                        // regenerated anyway.
                        on_ground: false,
                    },
                );
            }
            e.set_next_key(header.next_entity_key);
            e
        },
    };

    // Block entities, after the edits that placed their blocks (§7, §8.2).
    // Restored by name like everything else, and in the order the file lists
    // them -- which `save_world` wrote in position order, so the `BTreeMap` this
    // fills comes out identical either way.
    for f in &header.block_entities {
        let pos = [f.pos.0, f.pos.1, f.pos.2];
        world.add_furnace(pos);
        if let Some(furnace) = world.furnace_at_mut(pos) {
            let slot =
                |s: &Option<SavedStack>| from_saved(s, items).map(|st| (st.item(), st.count()));
            furnace.input = slot(&f.input);
            furnace.fuel = slot(&f.fuel);
            furnace.output = slot(&f.output);
            furnace.burning = f.burning;
            furnace.progress = f.progress;
        }
    }

    Ok((sim, world))
}
