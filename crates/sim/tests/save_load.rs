//! Save/load's own bar (`docs/PHASE1_ARCHITECTURE.md` §7.5, issue #60):
//! round trip, a committed cross-platform fixture, regeneration, byte
//! stability, and the two hard-error guards.

use std::path::PathBuf;

use cubara_sim::{
    load_world, save_world, InputFrame, LoadError, Player, Sim, WorldHash, FORMAT_VERSION,
};
use cubara_voxel::{
    BlockId, BlockRegistry, ChunkCoord, DropRule, Faces, Interact, Material, Shape,
};
use cubara_world::{TerrainBlocks, World, WORLDGEN_VERSION};

/// No real registry loaded from disk here (that's `cubara-render`'s job) --
/// three synthetic materials, same pattern `cubara_world`'s own tests use,
/// enough to give save/load's id table something real to remap.
/// The items the fixture worlds use. Block 2.8 made both save entry points take
/// an `ItemRegistry`, because a saved stack is stored by *name* and has to be
/// resolved against whatever ids this run assigned (§8.1).
fn test_items() -> cubara_voxel::ItemRegistry {
    let def = |name: &str, durability: Option<u16>| {
        (
            std::path::PathBuf::from(format!("{name}.ron")),
            cubara_voxel::ItemDef {
                name: name.to_string(),
                max_stack: if durability.is_some() { 1 } else { 64 },
                durability,
                tier: 0,
                speed: None,
                burn_ticks: None,
                rarity: cubara_voxel::Rarity::Common,
            },
        )
    };
    cubara_voxel::ItemRegistry::from_defs(vec![
        def("cubara:stone", None),
        def("cubara:soil", None),
        def("cubara:grass", None),
        def("cubara:raw_iron", None),
        def("cubara:oak_log", None),
        def("cubara:stone_pick", Some(130)),
    ])
    .expect("fixture items are valid")
}

fn test_registry() -> BlockRegistry {
    BlockRegistry::from_materials(vec![
        (
            PathBuf::from("grass.ron"),
            Material {
                name: "cubara:grass".to_string(),
                solid: true,
                faces: Faces::All("grass".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        ),
        (
            PathBuf::from("soil.ron"),
            Material {
                name: "cubara:soil".to_string(),
                solid: true,
                faces: Faces::All("soil".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        ),
        (
            PathBuf::from("stone.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        ),
    ])
    .expect("fixture registry is valid")
}

/// A fresh, unique scratch directory for one test -- process id + thread id
/// keeps parallel test runs from colliding, same pattern
/// `cubara_voxel::registry`'s and `cubara_world::region`'s own filesystem
/// tests use. Removed at the end of each test.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cubara-sim-save-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A generous, fixed region around the origin -- everywhere every test
/// below's edits and short walk can plausibly land, not tuned to any one
/// scenario's exact path.
fn hash_region() -> Vec<ChunkCoord> {
    (-3..=3)
        .flat_map(|x| (0..=2).flat_map(move |y| (-3..=3).map(move |z| ChunkCoord::new(x, y, z))))
        .collect()
}

#[test]
fn round_trip_edit_hash_save_load_hash_is_equal() {
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x00AB_CDEF_0123_4567;

    let mut world = World::with_seed(seed);
    let mut sim = Sim::new(seed, Player::new(glam::vec3(0.5, 40.0, 0.5), 0.6, -0.2));

    // A scripted edit sequence, then some ticks so tick/RNG/player state are
    // all non-trivial too, not just the world's edit overlay.
    world.set_block(0, 0, 0, BlockId::AIR);
    world.set_block(2, 1, -1, blocks.stone);
    let walking = InputFrame {
        move_axes: [0.0, 0.0, 1.0],
        look_delta: [0.3, 0.0],
        ..InputFrame::default()
    };
    for _ in 0..30 {
        sim.tick(&mut world, &walking, blocks);
    }
    let _ = sim.roll(); // advance the RNG stream too, so it's not still at its seeded start

    let region = hash_region();
    let before = WorldHash::compute(&sim, &world, &region, blocks, 1);

    let dir = scratch_dir("round-trip");
    save_world(&dir, &sim, &world, &registry, &test_items(), blocks).expect("save");
    let (loaded_sim, loaded_world) =
        load_world(&dir, &registry, &test_items(), blocks).expect("load");
    let after = WorldHash::compute(&loaded_sim, &loaded_world, &region, blocks, 1);

    assert_eq!(
        before, after,
        "hash before saving must equal hash after loading"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unedited_chunk_is_bit_identical_after_a_save_load_round_trip() {
    // The aggressive test of Rule 1 §7.4 names directly: an unedited chunk
    // isn't written at all, so this only passes if worldgen regenerates it
    // exactly, through the real save/load pipeline (not just calling
    // WorldGen twice, which `cross_platform_block_array_is_bit_identical`
    // in `cubara_world::worldgen` already covers).
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x00A5_A5A5_A5A5_A5A5;
    let unedited = ChunkCoord::new(6, 1, -6); // far from the edit below

    let mut world = World::with_seed(seed);
    let sim = Sim::new(seed, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
    world.set_block(0, 0, 0, BlockId::AIR); // an edit elsewhere, so save/load has real work

    let original = world
        .edited_chunk_at(unedited, blocks)
        .write_payload()
        .expect("encode original");

    let dir = scratch_dir("regeneration");
    save_world(&dir, &sim, &world, &registry, &test_items(), blocks).expect("save");
    let (_, loaded_world) = load_world(&dir, &registry, &test_items(), blocks).expect("load");
    let after_load = loaded_world
        .edited_chunk_at(unedited, blocks)
        .write_payload()
        .expect("encode after load");

    assert_eq!(
        original, after_load,
        "an unedited chunk must regenerate bit-identical"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_placed_block_keeps_its_own_material_across_a_round_trip() {
    // The property block 2.1c (#141) added: the edit overlay records *which*
    // block, not just "solid". A round trip has to preserve that, or breaking
    // an oak log and reloading would hand you stone.
    //
    // Placing grass deep underground is what makes this a real test: the
    // terrain there is stone, so a regression that flattened edits back to one
    // material -- exactly what the pre-#141 bool did -- reads back stone and
    // fails, rather than passing by coincidence because the two agreed.
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x0011_2233_4455_6677;
    assert_ne!(
        blocks.grass, blocks.stone,
        "this test needs two distinguishable materials to mean anything"
    );

    let (x, y, z) = (4, -60, 2);
    let mut world = World::with_seed(seed);
    world.set_block(x, y, z, blocks.grass);
    let sim = Sim::new(seed, Player::new(glam::vec3(0.5, 40.0, 0.5), 0.0, 0.0));

    let dir = scratch_dir("placed-material-round-trip");
    save_world(&dir, &sim, &world, &registry, &test_items(), blocks).expect("save");
    let (_, loaded) = load_world(&dir, &registry, &test_items(), blocks).expect("load");
    std::fs::remove_dir_all(&dir).ok();

    // Read it back through the chunk the renderer would get, rather than
    // adding a test-only accessor.
    let coord = ChunkCoord::from_block(x, y, z);
    let chunk = loaded.edited_chunk_at(coord, blocks);
    let size = 16i32;
    let local = |v: i32| v.rem_euclid(size) as usize;
    assert_eq!(
        chunk.get(local(x), local(y), local(z)),
        blocks.grass,
        "a placed block must come back as itself, not as the terrain's material"
    );
}

#[test]
fn saving_the_same_world_twice_produces_byte_identical_files() {
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x0044_4455_5566_6677;
    let mut world = World::with_seed(seed);
    world.set_block(1, 1, 1, blocks.stone);
    world.set_block(-9, 4, 20, BlockId::AIR);
    let sim = Sim::new(seed, Player::new(glam::vec3(1.0, 2.0, 3.0), 0.5, -0.1));

    let dir_a = scratch_dir("byte-stability-a");
    let dir_b = scratch_dir("byte-stability-b");
    save_world(&dir_a, &sim, &world, &registry, &test_items(), blocks).expect("save a");
    save_world(&dir_b, &sim, &world, &registry, &test_items(), blocks).expect("save b");

    assert_eq!(
        std::fs::read(dir_a.join("level.ron")).unwrap(),
        std::fs::read(dir_b.join("level.ron")).unwrap(),
        "level.ron"
    );

    let mut names_a: Vec<_> = std::fs::read_dir(dir_a.join("region"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut names_b: Vec<_> = std::fs::read_dir(dir_b.join("region"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names_a.sort();
    names_b.sort();
    assert_eq!(names_a, names_b);
    for name in &names_a {
        assert_eq!(
            std::fs::read(dir_a.join("region").join(name)).unwrap(),
            std::fs::read(dir_b.join("region").join(name)).unwrap(),
            "{name:?}"
        );
    }

    std::fs::remove_dir_all(&dir_a).ok();
    std::fs::remove_dir_all(&dir_b).ok();
}

/// The fixture's known hash -- this implementation's own computed value for
/// `tests/fixtures/save_fixture/` (committed to the repo), run once and
/// pinned here, same as `cubara_sim::tests::determinism`'s own fixture. Not
/// cross-checked against another implementation; what matters is that it
/// stays exactly this value on every future run, on every platform.
///
/// **Changed once, deliberately, in block 2.1b (#136):** the player's
/// inventory joined the world-state hash. The committed fixture file itself is
/// untouched -- what moved is the digest taken over the world it loads to, and
/// a loaded player now hashes an (empty) inventory that did not exist before.
/// Note the fixture's *bytes* are unchanged, so this is not a save-format
/// change; block 2.8 is what will make the inventory survive a round trip.
///
/// That is the only reason this may be re-blessed: a stated change to what is
/// hashed. If it moves without one, save/load has diverged and blessing it is
/// the same mistake as blessing a golden image to make a rendering test pass.
///
/// | Value | Why it changed |
/// |---|---|
/// | `0x2a6e_809c_6c6c_8930` | block 1.9 (#60), the original pin |
/// | `0xf1cf_74d0_987d_efb4` | block 2.1b (#136), inventory added to the hash |
/// | `0x01ba_197a_381f_63c2` | block 2.2b (#148), crafting grid + cursor added |
///
/// Three moves in one session is worth noticing rather than shrugging at. All
/// three are the same *kind* of change -- new player state joining the digest --
/// and each is a one-line addition to `write_sim`. If a fourth arrives for a
/// different reason, that is the signal these fixtures are pinned at the wrong
/// level and want their own issue.
const FIXTURE_HASH: u64 = 0x01ba_197a_381f_63c2;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/save_fixture")
}

/// A `Sim`/`World` with a fixed, non-trivial history: some edits, then
/// enough ticks that tick/RNG/player state are all non-default too -- what
/// [`the_committed_fixture_loads_to_a_known_hash`]'s fixture is built from.
fn fixture_state() -> (Sim, World) {
    const SEED: u64 = 0x00FE_ED5A_7E5E_ED01;
    let mut world = World::with_seed(SEED);
    let mut sim = Sim::new(SEED, Player::new(glam::vec3(0.5, 40.0, 0.5), 0.3, -0.1));

    // Resolved from the fixture's own registry, not `BlockId::STONE`: ids are
    // assigned by sorted name, so the constant is `cubara:grass` here, not
    // stone. `World::chunk_at`'s doc comment tells that story; this is the same
    // trap, one layer up.
    let blocks = TerrainBlocks::from_registry(&test_registry());
    world.set_block(0, 0, 0, BlockId::AIR);
    world.set_block(3, 2, -1, blocks.stone);
    world.set_block(-5, 10, 5, BlockId::AIR);

    let walking = InputFrame {
        move_axes: [0.0, 0.0, 1.0],
        look_delta: [0.5, 0.0],
        ..InputFrame::default()
    };
    for _ in 0..120 {
        sim.tick(&mut world, &walking, blocks);
    }
    (sim, world)
}

#[test]
fn the_committed_fixture_loads_to_a_known_hash() {
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let dir = fixture_dir();

    // Regenerating the reference is deliberate, never automatic -- same
    // convention as the golden-image tests
    // (`cargo test -p cubara-render --test golden`): only do this when the
    // format change is intended, and inspect the new fixture before
    // committing it.
    //
    //   CUBARA_BLESS=1 cargo test -p cubara-sim --test save_load the_committed_fixture
    if std::env::var_os("CUBARA_BLESS").is_some() {
        let (sim, world) = fixture_state();
        let _ = std::fs::remove_dir_all(&dir);
        save_world(&dir, &sim, &world, &registry, &test_items(), blocks)
            .expect("regenerate fixture");
        let hash = WorldHash::compute(&sim, &world, &hash_region(), blocks, 1);
        eprintln!("BLESSED {} (hash = {:#x})", dir.display(), hash.value());
        return;
    }

    let (sim, world) = load_world(&dir, &registry, &test_items(), blocks).expect("fixture loads");
    let hash = WorldHash::compute(&sim, &world, &hash_region(), blocks, 1);
    assert_eq!(
        hash.value(),
        FIXTURE_HASH,
        "the committed fixture's hash changed -- if this fires on only one of \
         macOS/Windows CI, save/load has diverged cross-platform. If intended:\n    \
         CUBARA_BLESS=1 cargo test -p cubara-sim --test save_load the_committed_fixture"
    );
}

#[test]
fn loading_with_a_registry_missing_a_saved_block_name_is_a_named_error() {
    let full_registry = test_registry(); // grass, soil, stone
    let blocks = TerrainBlocks::from_registry(&full_registry);
    let seed = 0x0066_6677_7788_8899;
    let world = World::with_seed(seed);
    let sim = Sim::new(seed, Player::new(glam::Vec3::ZERO, 0.0, 0.0));

    let dir = scratch_dir("missing-name");
    save_world(&dir, &sim, &world, &full_registry, &test_items(), blocks).expect("save");

    // A registry missing "cubara:soil" -- as if that mod were uninstalled.
    let partial_registry = BlockRegistry::from_materials(vec![
        (
            PathBuf::from("grass.ron"),
            Material {
                name: "cubara:grass".to_string(),
                solid: true,
                faces: Faces::All("grass".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        ),
        (
            PathBuf::from("stone.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        ),
    ])
    .expect("partial registry is valid");

    let err = load_world(&dir, &partial_registry, &test_items(), blocks).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("cubara:soil"),
        "message should name the missing block: {message}"
    );
    match err {
        LoadError::UnknownBlockName(name) => assert_eq!(name, "cubara:soil"),
        other => panic!("expected UnknownBlockName, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_a_worldgen_version_mismatch_is_a_named_error() {
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x0088_8899_99AA_AABB;
    let world = World::with_seed(seed);
    let sim = Sim::new(seed, Player::new(glam::Vec3::ZERO, 0.0, 0.0));

    let dir = scratch_dir("version-mismatch");
    save_world(&dir, &sim, &world, &registry, &test_items(), blocks).expect("save");

    let level_path = dir.join("level.ron");
    let text = std::fs::read_to_string(&level_path).unwrap();
    let needle = format!("worldgen_version: {WORLDGEN_VERSION}");
    assert!(text.contains(&needle), "level.ron:\n{text}");
    let tampered = text.replacen(&needle, "worldgen_version: 999999", 1);
    std::fs::write(&level_path, tampered).unwrap();

    let err = load_world(&dir, &registry, &test_items(), blocks).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("999999"),
        "message should name the mismatch: {message}"
    );
    match err {
        LoadError::WorldgenVersionMismatch { expected, found } => {
            assert_eq!(expected, WORLDGEN_VERSION);
            assert_eq!(found, 999999);
        }
        other => panic!("expected WorldgenVersionMismatch, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_an_unsupported_format_version_is_a_named_error() {
    let registry = test_registry();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x00CC_CCDD_DDEE_EEFF;
    let world = World::with_seed(seed);
    let sim = Sim::new(seed, Player::new(glam::Vec3::ZERO, 0.0, 0.0));

    let dir = scratch_dir("format-version-mismatch");
    save_world(&dir, &sim, &world, &registry, &test_items(), blocks).expect("save");

    let level_path = dir.join("level.ron");
    let text = std::fs::read_to_string(&level_path).unwrap();
    // Against the constant, not a literal: this test hardcoded "1" and broke
    // the moment block 2.8 bumped the version, which is a test rotting rather
    // than a bug being caught.
    let needle = format!("format_version: {FORMAT_VERSION}");
    let tampered = text.replacen(&needle, "format_version: 7", 1);
    assert_ne!(text, tampered, "the replace must have found something");
    std::fs::write(&level_path, tampered).unwrap();

    let err = load_world(&dir, &registry, &test_items(), blocks).unwrap_err();
    assert!(
        matches!(err, LoadError::UnsupportedFormatVersion(7)),
        "{err:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Phase 2's exit-gate round-trip criterion, verbatim:
///
/// > save the world mid-script, reload it, run the rest of the script, and land
/// > on the same hash as the uninterrupted run.
///
/// Deliberately not "the fields round-trip". That weaker test would pass while
/// the format quietly forgot anything the *hash* covers -- which is exactly the
/// state block 2.8 exists to add, and exactly how this would have shipped
/// half-done.
#[test]
fn a_world_saved_mid_script_finishes_where_an_uninterrupted_one_does() {
    let registry = test_registry();
    let items = test_items();
    let blocks = TerrainBlocks::from_registry(&registry);
    let seed = 0x0BAD_F00D_1234_5678;

    // The script: give the player a loaded inventory and a part-crafted grid,
    // put a running furnace in the world, drop some items on the floor, then
    // tick. Every one of those is phase 2 state the format did not carry.
    let setup = |sim: &mut Sim, world: &mut World| {
        let stone = items.id_of("cubara:stone").unwrap();
        let raw = items.id_of("cubara:raw_iron").unwrap();
        let log = items.id_of("cubara:oak_log").unwrap();
        let pick = items.id_of("cubara:stone_pick").unwrap();

        sim.player
            .inventory
            .set_slot(0, Some(items.new_stack(stone, 41).unwrap()));
        sim.player
            .inventory
            .set_slot(7, Some(items.new_stack(pick, 1).unwrap()));
        sim.player.inventory.select(3);
        sim.player.crafting.set_width(3);
        sim.player
            .crafting
            .set_cell(4, Some(items.new_stack(raw, 2).unwrap()));
        sim.player
            .crafting
            .set_held(Some(items.new_stack(log, 5).unwrap()));

        world.add_furnace([2, 40, -3]);
        let f = world.furnace_at_mut([2, 40, -3]).unwrap();
        f.input = Some((raw, 6));
        f.fuel = Some((log, 3));
        f.output = Some((items.id_of("cubara:stone").unwrap(), 2));
        f.burning = 17;
        f.progress = 4;

        for i in 0..3 {
            sim.entities.spawn_item(
                items.new_stack(stone, i + 1).unwrap(),
                glam::Vec3::new(i as f32, 45.0, 0.0),
                glam::Vec3::ZERO,
            );
        }
    };

    let tick_n = |sim: &mut Sim, world: &mut World, n: usize| {
        for _ in 0..n {
            sim.tick(world, &InputFrame::default(), blocks);
            sim.tick_entities(world, blocks, &items);
        }
    };

    // The uninterrupted run.
    let mut world_a = World::with_seed(seed);
    let mut sim_a = Sim::new(seed, Player::new(glam::Vec3::new(0.5, 50.0, 0.5), 0.0, 0.0));
    setup(&mut sim_a, &mut world_a);
    tick_n(&mut sim_a, &mut world_a, 20);

    // The interrupted one: same script, saved and reloaded at the halfway mark.
    let mut world_b = World::with_seed(seed);
    let mut sim_b = Sim::new(seed, Player::new(glam::Vec3::new(0.5, 50.0, 0.5), 0.0, 0.0));
    setup(&mut sim_b, &mut world_b);
    tick_n(&mut sim_b, &mut world_b, 10);

    let dir = scratch_dir("round-trip-mid-script");
    save_world(&dir, &sim_b, &world_b, &registry, &items, blocks).expect("save mid-script");
    let (mut sim_b, mut world_b) =
        load_world(&dir, &registry, &items, blocks).expect("reload mid-script");

    // Guard against a vacuous pass: two *empty* worlds would also hash alike, so
    // assert the reload actually carried phase 2's state before comparing.
    // Without block 2.8 every one of these is empty and the hash test below
    // would still have passed.
    assert_eq!(
        sim_b.player.inventory.slot(0).map(|s| s.count()),
        Some(41),
        "the inventory came back"
    );
    assert_eq!(
        sim_b.player.inventory.selected_slot(),
        3,
        "and so did the selected slot"
    );
    assert_eq!(sim_b.player.crafting.width(), 3, "the bench grid came back");
    assert!(sim_b.player.crafting.held().is_some(), "and the cursor");
    assert!(
        world_b.furnace_at([2, 40, -3]).is_some(),
        "the furnace came back"
    );
    assert_eq!(sim_b.entities.len(), 3, "and the items on the floor");
    assert_eq!(
        sim_b.entities.next_key(),
        3,
        "including the key counter, so new drops cannot collide with old ones"
    );

    tick_n(&mut sim_b, &mut world_b, 10);

    let region = hash_region_coords();
    let a = WorldHash::compute(&sim_a, &world_a, &region, blocks, 1);
    let b = WorldHash::compute(&sim_b, &world_b, &region, blocks, 1);
    assert_eq!(
        a.value(),
        b.value(),
        "a world saved and reloaded mid-script diverged from the uninterrupted run"
    );
}

/// A small region around the origin, which is where the script works.
fn hash_region_coords() -> Vec<ChunkCoord> {
    let mut out = Vec::new();
    for x in -1..=1 {
        for z in -1..=1 {
            for y in 0..=3 {
                out.push(ChunkCoord::new(x, y, z));
            }
        }
    }
    out
}
