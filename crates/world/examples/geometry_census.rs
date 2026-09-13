//! Where the triangles go: a census of the benchmark scene's geometry.
//!
//! ```bash
//! cargo run --release -p cubara-world --example geometry_census -- [radius] [ymin] [ymax]
//! ```
//!
//! Every quad the meshers emit for the bench region is sorted into one of:
//!
//! - **buried** -- the cell on the outside of the face is solid. Nobody can ever
//!   see this face; it exists because each node is meshed without looking at its
//!   neighbours, so where two solid nodes meet, both emit a wall.
//! - **underground** -- facing air, but below the terrain surface at that
//!   column: cave walls, visible only through a cave mouth.
//! - **surface** -- facing air at or above the surface: what a player above
//!   ground actually looks at.
//!
//! It measures, it does not change anything, and it is what decides which
//! optimisation is worth building first.

use std::path::Path;

use cubara_voxel::{BlockRegistry, ChunkCoord, Face, MeshContext, OreRegistry, StructureRegistry};
use cubara_world::mesh::mesh_node;
use cubara_world::node::{desired_nodes, desired_nodes_3d, schedule_for_radius};
use cubara_world::{TerrainBlocks, World};

#[derive(Default, Debug, Clone, Copy)]
struct Tally {
    buried: u64,
    underground: u64,
    surface: u64,
}

impl Tally {
    fn total(&self) -> u64 {
        self.buried + self.underground + self.surface
    }
}

fn main() {
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let radius = args.first().copied().unwrap_or(64);
    let (ymin, ymax) = (
        args.get(1).copied().unwrap_or(-2),
        args.get(2).copied().unwrap_or(2),
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry = BlockRegistry::load(&root.join("assets/blocks")).expect("assets/blocks");
    let structures =
        StructureRegistry::load(&root.join("assets/structures")).expect("assets/structures");
    let ores = OreRegistry::load(&root.join("assets/ores")).expect("assets/ores");
    let blocks = TerrainBlocks::from_registry(&registry)
        .with_oak(&structures, &registry)
        .with_ores(&ores, &registry);
    let world = World::new();
    let layer_of = |_: &str| 0;
    let ctx = MeshContext {
        registry: &registry,
        layer_of: &layer_of,
    };

    let schedule = schedule_for_radius(radius);
    // `CENSUS_SQUASH=k` selects in 3D around chunk (0, ymin, 0) the way the
    // game does, instead of the band ymin..=ymax.
    let nodes = match std::env::var("CENSUS_SQUASH").ok().and_then(|k| k.parse().ok()) {
        Some(k) => desired_nodes_3d(ChunkCoord::new(0, ymin, 0), k, &schedule),
        None => desired_nodes(ChunkCoord::new(0, 0, 0), ymin..=ymax, &schedule),
    };
    let mut per_level = [Tally::default(); 8];

    let covered = std::env::var_os("CENSUS_OPEN").is_none();
    // Meshing alone, timed before the census so the census's own world
    // lookups do not count against it.
    let started = std::time::Instant::now();
    for node in &nodes {
        if covered {
            let _ = mesh_node(&world, &registry, &layer_of, *node, blocks);
        } else if let Some(chunk) = world.node_at(*node, blocks) {
            let _ = chunk.build_mesh(&ctx);
        }
    }
    let elapsed = started.elapsed();
    for node in &nodes {
        // The real meshing path, or with `CENSUS_OPEN=1` every node meshed on
        // its own the way it was before border covering -- for a before/after.
        let mesh = if covered {
            match mesh_node(&world, &registry, &layer_of, *node, blocks) {
                Some(g) => g.mesh,
                None => continue,
            }
        } else {
            let Some(chunk) = world.node_at(*node, blocks) else {
                continue;
            };
            chunk.build_mesh(&ctx)
        };
        // Blocks per lattice cell of this node's mesh.
        let cell = node.extent_chunks();
        let origin = node.world_origin();
        let tally = &mut per_level[node.level as usize];

        for quad in mesh.vertices.chunks(4) {
            // The quad's centre, in world blocks.
            let mut c = [0f64; 3];
            for v in quad {
                c[0] += v.x() as f64 / 4.0;
                c[1] += v.y() as f64 / 4.0;
                c[2] += v.z() as f64 / 4.0;
            }
            let world_c = [
                origin[0] as f64 + c[0] * cell as f64,
                origin[1] as f64 + c[1] * cell as f64,
                origin[2] as f64 + c[2] * cell as f64,
            ];
            let n = quad[0].face().normal();
            // Half a cell outside the face: the centre of the neighbouring
            // cell at this node's own resolution. At level 0 that is exactly
            // the neighbouring block; above, it samples the middle of the
            // coarse cell, which is what the downsample keys on.
            let half = cell as f64 * 0.5;
            let out = [
                (world_c[0] + n[0] as f64 * half).floor() as i32,
                (world_c[1] + n[1] as f64 * half).floor() as i32,
                (world_c[2] + n[2] as f64 * half).floor() as i32,
            ];
            let tris = 2u64;
            if registry.is_solid(world.block_at(out[0], out[1], out[2], blocks)) {
                tally.buried += tris;
            } else if (world_c[1].floor() as i32) < world.surface_height(out[0], out[2]) - 1 {
                tally.underground += tris;
            } else {
                tally.surface += tris;
            }
        }
    }

    let mut all = Tally::default();
    println!(
        "radius {radius}, chunk-layers {ymin}..={ymax}, {} nodes",
        nodes.len()
    );
    println!("level | triangles | buried | underground | surface");
    for (level, t) in per_level.iter().enumerate() {
        if t.total() == 0 {
            continue;
        }
        all.buried += t.buried;
        all.underground += t.underground;
        all.surface += t.surface;
        println!(
            "  {level}   | {:>9} | {:>5.1}% | {:>10.1}% | {:>6.1}%",
            t.total(),
            pct(t.buried, t.total()),
            pct(t.underground, t.total()),
            pct(t.surface, t.total())
        );
    }
    println!(
        "  all | {:>9} | {:>5.1}% | {:>10.1}% | {:>6.1}%",
        all.total(),
        pct(all.buried, all.total()),
        pct(all.underground, all.total()),
        pct(all.surface, all.total())
    );
    println!(
        "meshed{} in {:.2} s (single thread, meshing only)",
        if covered {
            " with border covering"
        } else {
            " without border covering"
        },
        elapsed.as_secs_f64()
    );
    let _ = Face::PosX;
}

fn pct(part: u64, whole: u64) -> f64 {
    100.0 * part as f64 / whole.max(1) as f64
}
