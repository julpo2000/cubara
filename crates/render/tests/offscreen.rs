//! The window's renderer with no window ([`Renderer::offscreen`]): what
//! `--bench flight` measures through, so it has to draw what it is handed and
//! stop drawing what it is told to drop, the way the window does.

use cubara_render::{CameraPose, Hud, MeshedNode, NodeId, Renderer};
use cubara_voxel::ChunkCoord;
use cubara_world::mesh::mesh_node;
use cubara_world::node::NodeKey;
use cubara_world::{TerrainBlocks, World};

fn draw(renderer: &mut Renderer, camera: CameraPose) -> Vec<u8> {
    renderer.render(
        camera,
        None,
        None,
        &[],
        Hud {
            hotbar: None,
            panel: None,
            health: None,
            crosshair: false,
            menu: None,
        },
        0.0,
    );
    renderer
        .read_frame()
        .expect("an offscreen renderer reads its frame back")
        .pixels
}

fn id(node: NodeKey) -> NodeId {
    NodeId {
        level: node.level,
        pos: node.pos,
    }
}

#[test]
fn an_offscreen_renderer_draws_what_it_is_handed_and_nothing_it_dropped() {
    let world = World::new();
    let surface = world.surface_height(8, 8) as f32;
    let camera = CameraPose {
        eye: glam::Vec3::new(8.0, surface + 24.0, 8.0),
        look_dir: glam::Vec3::new(0.05, -1.0, 0.03),
    };
    let Some((mut renderer, assets)) = Renderer::offscreen(64, 48, camera) else {
        eprintln!("SKIP an_offscreen_renderer_draws_what_it_is_handed: no GPU adapter");
        return;
    };
    assert_eq!(renderer.size(), (64, 48));
    let layer_of = |name: &str| assets.layers.layer_of(name);
    let blocks = TerrainBlocks::from_registry(&assets.registry);
    let node = NodeKey::containing(ChunkCoord::from_world_pos([8.0, surface, 8.0]), 1);
    let geometry = mesh_node(&world, &assets.registry, &layer_of, node, blocks)
        .expect("the ground under spawn has something to draw");
    let meshed = MeshedNode {
        id: id(node),
        origin: geometry.origin,
        scale: geometry.scale,
        mesh: geometry.mesh,
        aabb: geometry.aabb,
    };

    let sky = draw(&mut renderer, camera);
    renderer.apply_node_updates([], [meshed]);
    let ground = draw(&mut renderer, camera);
    assert_eq!(
        renderer.uploads_pending(),
        0,
        "one node waited past a frame"
    );
    let pixels = sky.len() / 4;
    let differ = sky
        .as_chunks::<4>()
        .0
        .iter()
        .zip(ground.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differ > pixels / 2,
        "the node handed over is not what the frame shows ({differ} of {pixels} pixels differ)"
    );

    renderer.apply_node_updates([id(node)], []);
    assert_eq!(
        draw(&mut renderer, camera),
        sky,
        "a dropped node is still drawn"
    );
}
