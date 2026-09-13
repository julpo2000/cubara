//! How much finer-grained visibility would cull: an experiment, not the game.
//!
//! ```bash
//! cargo run --release -p cubara-world --example visibility_grain -- [eye_y]
//! ```
//!
//! Splits every node into `grain`³ sub-blocks, works out what air joins inside
//! each, and runs the same direction-limited search over sub-blocks instead of
//! whole nodes. Reports, per grain, the triangles drawn if a node is drawn
//! whenever any of its sub-blocks is visible, and if only visible sub-blocks
//! are drawn (the best a per-sub-block split of each mesh could do).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Instant;

use cubara_voxel::{
    BlockId, BlockRegistry, Chunk, ChunkCoord, Face, OreRegistry, StructureRegistry,
};
use cubara_world::mesh::build_node;
use cubara_world::node::{desired_nodes_3d, schedule_for_radius, NodeKey};
use cubara_world::{TerrainBlocks, World};

const FACES: [Face; 6] = [
    Face::PosX,
    Face::NegX,
    Face::PosY,
    Face::NegY,
    Face::PosZ,
    Face::NegZ,
];

fn opposite(f: Face) -> Face {
    match f {
        Face::PosX => Face::NegX,
        Face::NegX => Face::PosX,
        Face::PosY => Face::NegY,
        Face::NegY => Face::PosY,
        Face::PosZ => Face::NegZ,
        Face::NegZ => Face::PosZ,
    }
}

/// 6x6 joined matrix as bits: bit (a*6+b).
fn sub_links(chunk: Option<&Chunk>, registry: &BlockRegistry, grain: usize) -> Vec<u64> {
    let s = Chunk::SIZE / grain;
    let mut out = vec![0u64; grain * grain * grain];
    let Some(chunk) = chunk else {
        return vec![u64::MAX; grain * grain * grain];
    };
    for gz in 0..grain {
        for gy in 0..grain {
            for gx in 0..grain {
                let base = [gx * s, gy * s, gz * s];
                let idx = |x: usize, y: usize, z: usize| (z * s + y) * s + x;
                let open = |x: usize, y: usize, z: usize| {
                    !registry.is_solid(chunk.get(base[0] + x, base[1] + y, base[2] + z))
                };
                let mut seen = vec![false; s * s * s];
                let mut links = 0u64;
                for z in 0..s {
                    for y in 0..s {
                        for x in 0..s {
                            if seen[idx(x, y, z)] || !open(x, y, z) {
                                continue;
                            }
                            seen[idx(x, y, z)] = true;
                            let mut stack = vec![(x, y, z)];
                            let mut touched = 0u8;
                            while let Some((x, y, z)) = stack.pop() {
                                let e = s - 1;
                                touched |= (x == e) as u8
                                    | ((x == 0) as u8) << 1
                                    | ((y == e) as u8) << 2
                                    | ((y == 0) as u8) << 3
                                    | ((z == e) as u8) << 4
                                    | ((z == 0) as u8) << 5;
                                let mut n = Vec::with_capacity(6);
                                if x > 0 {
                                    n.push((x - 1, y, z));
                                }
                                if x < e {
                                    n.push((x + 1, y, z));
                                }
                                if y > 0 {
                                    n.push((x, y - 1, z));
                                }
                                if y < e {
                                    n.push((x, y + 1, z));
                                }
                                if z > 0 {
                                    n.push((x, y, z - 1));
                                }
                                if z < e {
                                    n.push((x, y, z + 1));
                                }
                                for (a, b, c) in n {
                                    if !seen[idx(a, b, c)] && open(a, b, c) {
                                        seen[idx(a, b, c)] = true;
                                        stack.push((a, b, c));
                                    }
                                }
                            }
                            for a in 0..6 {
                                for b in 0..6 {
                                    if a != b && touched & (1 << a) != 0 && touched & (1 << b) != 0
                                    {
                                        links |= 1 << (a * 6 + b);
                                    }
                                }
                            }
                        }
                    }
                }
                out[(gz * grain + gy) * grain + gx] = links;
            }
        }
    }
    out
}

fn main() {
    let eye_y: i32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(40);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry = BlockRegistry::load(&root.join("assets/blocks")).expect("blocks");
    let structures = StructureRegistry::load(&root.join("assets/structures")).expect("structures");
    let ores = OreRegistry::load(&root.join("assets/ores")).expect("ores");
    let blocks = TerrainBlocks::from_registry(&registry)
        .with_oak(&structures, &registry)
        .with_ores(&ores, &registry);
    let world = World::new();
    let eye = [8, eye_y, 8];
    let camera = ChunkCoord::from_block(eye[0], eye[1], eye[2]);
    let nodes: Vec<NodeKey> = desired_nodes_3d(camera, 2, &schedule_for_radius(64));
    let set: HashSet<NodeKey> = nodes.iter().copied().collect();
    let layer_of = |_: &str| 0;

    let t = Instant::now();
    let mut chunks: HashMap<NodeKey, Option<Chunk>> = HashMap::new();
    // Quad centres in world blocks, per node.
    let mut quads: HashMap<NodeKey, Vec<[i32; 3]>> = HashMap::new();
    for &n in &nodes {
        let built = build_node(&world, &registry, &layer_of, n, blocks);
        if let Some(g) = built.geometry {
            let (o, cell) = (n.world_origin(), n.extent_chunks());
            let centres = g
                .mesh
                .vertices
                .chunks(4)
                .map(|q| {
                    let avg = |k: usize| {
                        q.iter()
                            .map(|v| [v.x(), v.y(), v.z()][k] as i32)
                            .sum::<i32>()
                            / 4
                    };
                    let nrm = q[0].face().normal();
                    // Nudge into the solid side so a face on a sub-block
                    // boundary belongs to the block drawing it.
                    [0, 1, 2].map(|k| o[k] + avg(k) * cell - (nrm[k] > 0.0) as i32 * (cell.min(1)))
                })
                .collect();
            quads.insert(n, centres);
        }
        chunks.insert(n, world.node_at(n, blocks));
    }
    println!(
        "{} nodes around eye y={eye_y}, generated+meshed in {:.1} s",
        nodes.len(),
        t.elapsed().as_secs_f64()
    );
    let total: usize = quads.values().map(Vec::len).sum::<usize>() * 2;
    println!("all nodes: {total} triangles");

    for grain in [1usize, 2, 4] {
        let t = Instant::now();
        let links: HashMap<NodeKey, Vec<u64>> = nodes
            .iter()
            .map(|&n| (n, sub_links(chunks[&n].as_ref(), &registry, grain)))
            .collect();
        let link_time = t.elapsed().as_secs_f64();
        let sub_blocks = Chunk::SIZE as i32 / grain as i32;
        let locate = |p: [i32; 3]| -> Option<(NodeKey, usize)> {
            let c = ChunkCoord::from_block(p[0], p[1], p[2]);
            let n = (0..=3)
                .map(|l| NodeKey::containing(c, l))
                .find(|n| set.contains(n))?;
            let (o, cell) = (n.world_origin(), n.extent_chunks());
            let size = sub_blocks * cell;
            let i = |k: usize| ((p[k] - o[k]) / size) as usize;
            Some((n, (i(2) * grain + i(1)) * grain + i(0)))
        };
        let t = Instant::now();
        let start = locate(eye).expect("eye inside");
        let mut visible: HashSet<(NodeKey, usize)> = HashSet::from([start]);
        let mut arrived: HashMap<(NodeKey, usize, u8), u8> = HashMap::new();
        let mut queue = VecDeque::new();
        let neighbour = |(n, i): (NodeKey, usize), f: Face| -> Option<(NodeKey, usize)> {
            let (o, cell) = (n.world_origin(), n.extent_chunks());
            let size = sub_blocks * cell;
            let g = grain as i32;
            let ii = i as i32;
            let min = [
                o[0] + (ii % g) * size,
                o[1] + (ii / g % g) * size,
                o[2] + (ii / g / g) * size,
            ];
            let nrm = f.normal();
            let p = [0, 1, 2].map(|k| match nrm[k] as i32 {
                1 => min[k] + size,
                -1 => min[k] - 1,
                _ => min[k] + size / 2,
            });
            locate(p)
        };
        for f in FACES {
            if let Some(v) = neighbour(start, f) {
                queue.push_back((v, opposite(f), 1u8 << f as u8));
            }
        }
        while let Some((v, entered, taken)) = queue.pop_front() {
            visible.insert(v);
            let key = (v.0, v.1, entered as u8);
            match arrived.get(&key) {
                Some(&b) if b & !taken == 0 => continue,
                Some(&b) => {
                    arrived.insert(key, b & taken);
                }
                None => {
                    arrived.insert(key, taken);
                }
            }
            let taken = arrived[&key];
            let l = links[&v.0][v.1];
            for out in FACES {
                if taken & (1 << opposite(out) as u8) != 0 {
                    continue;
                }
                if l & (1u64 << (entered as usize * 6 + out as usize)) == 0 {
                    continue;
                }
                if let Some(nv) = neighbour(v, out) {
                    queue.push_back((nv, opposite(out), taken | 1 << out as u8));
                }
            }
        }
        let bfs = t.elapsed().as_secs_f64();
        let visible_nodes: HashSet<NodeKey> = visible.iter().map(|v| v.0).collect();
        let whole: usize = quads
            .iter()
            .filter(|(n, _)| visible_nodes.contains(n))
            .map(|(_, q)| q.len() * 2)
            .sum();
        let split: usize = quads
            .values()
            .flatten()
            .filter(|&&p| locate(p).is_some_and(|v| visible.contains(&v)))
            .count()
            * 2;
        println!(
            "grain {grain}: links {link_time:.2} s, search {:.1} ms, {} of {} nodes | whole nodes {whole} tris | visible sub-blocks only {split} tris",
            bfs * 1000.0,
            visible_nodes.len(),
            nodes.len()
        );
    }
    // The oracle: which grain-4 sub-blocks straight rays actually hit.
    let grain = 4usize;
    let sub_blocks = Chunk::SIZE as i32 / grain as i32;
    let locate = |p: [i32; 3]| -> Option<(NodeKey, usize, bool)> {
        let c = ChunkCoord::from_block(p[0], p[1], p[2]);
        let n = (0..=3)
            .map(|l| NodeKey::containing(c, l))
            .find(|n| set.contains(n))?;
        let (o, cell) = (n.world_origin(), n.extent_chunks());
        let size = sub_blocks * cell;
        let i = |k: usize| ((p[k] - o[k]) / size) as usize;
        let ci = |k: usize| ((p[k] - o[k]) / cell) as usize;
        let solid = chunks[&n]
            .as_ref()
            .is_some_and(|ch| ch.get(ci(0), ci(1), ci(2)) != BlockId::AIR);
        Some((n, (i(2) * grain + i(1)) * grain + i(0), solid))
    };
    let t = Instant::now();
    let mut hit: HashSet<(NodeKey, usize)> = HashSet::new();
    let e = [
        eye[0] as f32 + 0.37,
        eye[1] as f32 + 0.41,
        eye[2] as f32 + 0.53,
    ];
    let side = 40i32;
    let mut rays = 0;
    for a in -side..=side {
        for b in -side..=side {
            for (axis, sign) in [(0usize, 1i32), (0, -1), (1, 1), (1, -1), (2, 1), (2, -1)] {
                let mut d = [0f32; 3];
                d[axis] = sign as f32 * side as f32;
                d[(axis + 1) % 3] = a as f32 + 0.113;
                d[(axis + 2) % 3] = b as f32 + 0.271;
                rays += 1;
                let mut cell = [
                    e[0].floor() as i32,
                    e[1].floor() as i32,
                    e[2].floor() as i32,
                ];
                let mut step = [0i32; 3];
                let mut tmax = [f32::INFINITY; 3];
                let mut tdel = [f32::INFINITY; 3];
                for k in 0..3 {
                    if d[k] > 0.0 {
                        step[k] = 1;
                        tmax[k] = (cell[k] as f32 + 1.0 - e[k]) / d[k];
                        tdel[k] = 1.0 / d[k];
                    } else if d[k] < 0.0 {
                        step[k] = -1;
                        tmax[k] = (e[k] - cell[k] as f32) / -d[k];
                        tdel[k] = 1.0 / -d[k];
                    }
                }
                for _ in 0..4000 {
                    let Some((n, i, solid)) = locate(cell) else {
                        break;
                    };
                    if solid {
                        hit.insert((n, i));
                        break;
                    }
                    let k = if tmax[0] < tmax[1] && tmax[0] < tmax[2] {
                        0
                    } else if tmax[1] < tmax[2] {
                        1
                    } else {
                        2
                    };
                    cell[k] += step[k];
                    tmax[k] += tdel[k];
                }
            }
        }
    }
    let hit_nodes: HashSet<NodeKey> = hit.iter().map(|v| v.0).collect();
    let whole: usize = quads
        .iter()
        .filter(|(n, _)| hit_nodes.contains(n))
        .map(|(_, q)| q.len() * 2)
        .sum();
    let split: usize = quads
        .values()
        .flatten()
        .filter(|&&p| locate(p).is_some_and(|(n, i, _)| hit.contains(&(n, i))))
        .count()
        * 2;
    println!(
        "oracle ({rays} rays, {:.1} s): {} nodes hit | whole hit nodes {whole} tris | hit sub-blocks only {split} tris",
        t.elapsed().as_secs_f64(),
        hit_nodes.len()
    );
    let _ = BlockId::AIR;
}
