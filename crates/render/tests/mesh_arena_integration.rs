//! Integration between `cubara_world::mesh` (node meshing) and
//! `cubara_render::arena` (GPU upload) -- the seam issue #110 put between two
//! crates. Lives here, not as a unit test in either crate, because it needs
//! both: `cubara-world` must never depend on `wgpu`
//! (`ARCHITECTURE.md` §1/§4), so `cubara-render`'s own test suite (which has
//! `cubara-world` as a dev-dependency, see `Cargo.toml`) is the only place
//! that can exercise the real pipeline end to end.

use cubara_render::ChunkArena;
use cubara_voxel::{BlockRegistry, ChunkCoord, DropRule, Faces, Interact, Material, Shape};
use cubara_world::mesh::{sort_batch, BuiltNode};
use cubara_world::node::desired_nodes;
use cubara_world::{TerrainBlocks, World};

/// A registry with the three real material *names* -- `mesh_node` resolves
/// `TerrainBlocks::from_registry` by name (block 1.4c), so a fixture missing
/// any of them panics. All three are plain `All` materials with an empty
/// texture-layer map (`|_: &str| 0`), since this test is about arena layout
/// mechanics, not texturing.
fn test_registry() -> BlockRegistry {
    let material = |name: &str| {
        (
            std::path::PathBuf::from("test-fixture.ron"),
            Material {
                name: name.to_string(),
                solid: true,
                faces: Faces::All(name.to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
                interact: Interact::None,
            },
        )
    };
    BlockRegistry::from_materials(vec![
        material("cubara:grass"),
        material("cubara:soil"),
        material("cubara:stone"),
    ])
    .expect("fixture registry is valid")
}

/// A headless device, or `None` on a CI runner with no GPU adapter -- the same
/// convention `cubara_render::headless::render` uses, so this test skips loudly
/// instead of failing where there is nothing to test against.
fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("cubara-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()
}

#[test]
fn sorted_batch_gives_the_same_arena_layout_regardless_of_arrival_order() {
    // Issue #83: ChunkArena::insert is first-fit, so whichever order a batch
    // of finished mesh jobs is *applied* in decides which node gets which
    // slab offset. Worker completion order is not the same every run. A unit
    // test over the allocator alone would not catch this -- the allocator is
    // already deterministic given its input order, the bug is the pipeline
    // feeding it that order -- so this drives the real pipeline pieces
    // (BuiltNode, mesh_node, ChunkArena::insert) through several different
    // arrival orders of the *same* batch and asserts sort_batch makes all of
    // them land on the identical layout.
    let Some((device, queue)) = test_device() else {
        eprintln!(
            "SKIP sorted_batch_gives_the_same_arena_layout_regardless_of_arrival_order: \
             no GPU adapter"
        );
        return;
    };

    let world = World::new();
    let registry = test_registry();
    let layer_of = |_: &str| 0;
    let schedule = [(0u32, 3i32)];
    let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=2, &schedule);

    // Several stand-ins for different worker-scheduling outcomes: request
    // order, fully reversed, and a couple of arbitrary shuffles.
    let mut reversed = nodes.clone();
    reversed.reverse();
    let mut shuffled_a = nodes.clone();
    shuffled_a.sort_by_key(|n| (n.pos[0] * 7 + n.pos[2] * 13 + n.pos[1] * 31).rem_euclid(97));
    let mut shuffled_b = nodes.clone();
    shuffled_b.sort_by_key(|n| -(n.pos[0] * 5 + n.pos[2] * 11 + n.pos[1] * 17));
    let orderings = [nodes.clone(), reversed, shuffled_a, shuffled_b];

    let mut layouts = Vec::new();
    for order in &orderings {
        let batch: Vec<BuiltNode> = order
            .iter()
            .map(|&node| {
                cubara_world::mesh::build_node(
                    &world,
                    &registry,
                    &layer_of,
                    node,
                    TerrainBlocks::from_registry(&registry),
                )
            })
            .collect();

        let mut arena = ChunkArena::new(&device, false);
        for built in sort_batch(batch) {
            if let Some(geometry) = built.geometry {
                let id = cubara_render::NodeId {
                    level: built.node.level,
                    pos: built.node.pos,
                };
                arena.insert(
                    &queue,
                    id,
                    geometry.origin,
                    geometry.scale,
                    &geometry.mesh,
                    geometry.aabb,
                );
            }
        }
        layouts.push(arena.slot_offsets());
    }

    let reference = &layouts[0];
    assert!(
        reference.len() > 1,
        "the test region must produce more than one node of geometry to be meaningful"
    );
    for (i, layout) in layouts.iter().enumerate().skip(1) {
        assert_eq!(
            layout, reference,
            "arrival order {i} produced a different arena layout than sorted order 0 -- \
             sort_batch did not make the layout arrival-order-independent"
        );
    }
}

/// `ChunkArena::prepare`'s doc comment says it emits "one draw per run of
/// face directions that can face the camera... at most three runs" -- pin
/// that against the real pipeline rather than trusting the comment. A corner
/// view (diagonal on all three axes) is deliberate: a camera looking straight
/// at one face only ever exercises a single run per node, which could not
/// tell a regression that collapsed `prepare` back to one draw per node
/// (dropping the face-direction split entirely) from correct behaviour --
/// both would pass. The corner forces at least one node to need more than
/// one draw, which is what `draw_count > visible_nodes` below actually pins.
#[test]
fn prepare_emits_between_one_and_three_draws_per_visible_node() {
    let Some((device, queue)) = test_device() else {
        eprintln!(
            "SKIP prepare_emits_between_one_and_three_draws_per_visible_node: no GPU adapter"
        );
        return;
    };

    let world = World::new();
    let registry = test_registry();
    let layer_of = |_: &str| 0;
    let schedule = [(0u32, 3i32)];
    let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=1, &schedule);

    let mut arena = ChunkArena::new(&device, false);
    for node in nodes {
        let built = cubara_world::mesh::build_node(
            &world,
            &registry,
            &layer_of,
            node,
            TerrainBlocks::from_registry(&registry),
        );
        if let Some(geometry) = built.geometry {
            let id = cubara_render::NodeId {
                level: built.node.level,
                pos: built.node.pos,
            };
            arena.insert(
                &queue,
                id,
                geometry.origin,
                geometry.scale,
                &geometry.mesh,
                geometry.aabb,
            );
        }
    }

    let (min, max) = arena.bounds().expect("test region produced no geometry");
    let center = glam::vec3(
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    );
    let extent = (max[0] - min[0])
        .max(max[1] - min[1])
        .max(max[2] - min[2])
        .max(1.0);
    let eye = center + glam::vec3(extent, extent, extent);
    let vp = cubara_render::CameraUniform::look_view_proj(1.0, eye, center - eye);
    let frustum = cubara_render::Frustum::from_view_proj(vp);

    let draw_count = arena.prepare(&queue, &frustum);
    let visible_nodes = arena.visible_nodes();

    assert!(
        visible_nodes > 0,
        "the corner camera must see some of the test region"
    );
    assert!(
        draw_count >= visible_nodes,
        "prepare must issue at least one draw per visible node, got {draw_count} draws for \
         {visible_nodes} nodes"
    );
    assert!(
        draw_count <= visible_nodes * 3,
        "prepare must issue at most three draws per visible node (one per opposite-face pair \
         still facing the camera), got {draw_count} draws for {visible_nodes} nodes"
    );
    assert!(
        draw_count > visible_nodes,
        "a corner view must see more than one face direction on at least one node; \
         draw_count == visible_nodes ({visible_nodes}) means prepare stopped splitting \
         by face direction"
    );
}
