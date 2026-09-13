//! Which nodes the camera could possibly see: everything else need not be
//! meshed, uploaded or drawn.
//!
//! **The rule this must never break: if any straight line from the camera
//! reaches a node through open air, that node is in the set.** It may include
//! nodes nobody can see -- that only costs time -- but leaving out one that can
//! be seen is a hole in the world.
//!
//! # How
//!
//! A search outward from the camera's node, from node to neighbouring node
//! across faces, entering a node through one face and leaving through another
//! only if air inside joins the two ([`FaceLinks`]). What makes it cull anything
//! at all is one more rule: **never step in the direction opposite one already
//! taken.** A path that has gone down may not come back up.
//!
//! Why that loses nothing: a straight line moves in one direction along each
//! axis, so it never steps against a direction it already took, and inside
//! every node it crosses, the air along it joins the face it came in by to the
//! face it leaves by. So each straight line of sight is one of the paths the
//! search follows.
//!
//! When two paths reach a node through the same face, the one that has taken
//! fewer directions forbids less; keeping the *intersection* of what each has
//! taken forbids no more than either, so merging can never block a line of
//! sight either.
//!
//! The camera's own direction of view plays no part, so the set only changes
//! when the camera moves to another node or a node's contents change -- not
//! every frame. The frustum is culled separately, per frame, as before.
//!
//! This is the approach known from other voxel engines as cave culling; the
//! proof above is why it is safe here.

use std::collections::{HashMap, HashSet, VecDeque};

use cubara_voxel::{BlockRegistry, Chunk, ChunkCoord, Face, FaceLinks};

use crate::node::NodeKey;

/// Sub-blocks per node along each axis. The search runs over these rather
/// than over whole nodes: a far node is 128 blocks wide, and one cave touching
/// two of its faces used to make all of it see-through. Measured on the radius-64
/// scene with caves at every level of detail: 1.74M triangles reachable at
/// grain 1, 1.25M at grain 2, 1.12M at grain 4 -- which also searched nine
/// times longer, for a tenth of the gain.
pub const GRAIN: usize = 2;

/// What joins what inside each of a node's `GRAIN`³ sub-blocks, indexed
/// `(z * GRAIN + y) * GRAIN + x`.
pub type NodeLinks = [FaceLinks; GRAIN * GRAIN * GRAIN];

/// Every sub-block of an empty node is open.
pub const OPEN: NodeLinks = [FaceLinks::ALL; GRAIN * GRAIN * GRAIN];

/// The sub-block links of a generated node; `None` (nothing generated, it is
/// empty) is [`OPEN`].
pub fn node_links(chunk: Option<&Chunk>, registry: &BlockRegistry) -> NodeLinks {
    let Some(chunk) = chunk else {
        return OPEN;
    };
    let size = Chunk::SIZE / GRAIN;
    std::array::from_fn(|i| {
        let (x, y, z) = (i % GRAIN, i / GRAIN % GRAIN, i / GRAIN / GRAIN);
        FaceLinks::of_region(chunk, registry, [x * size, y * size, z * size], size)
    })
}

const FACES: [Face; 6] = [
    Face::PosX,
    Face::NegX,
    Face::PosY,
    Face::NegY,
    Face::PosZ,
    Face::NegZ,
];

fn opposite(face: Face) -> Face {
    match face {
        Face::PosX => Face::NegX,
        Face::NegX => Face::PosX,
        Face::PosY => Face::NegY,
        Face::NegY => Face::PosY,
        Face::PosZ => Face::NegZ,
        Face::NegZ => Face::PosZ,
    }
}

fn step_of(face: Face) -> [i32; 3] {
    let n = face.normal();
    [n[0] as i32, n[1] as i32, n[2] as i32]
}

/// The nodes being searched, numbered, so per-sub-block state is a flat array.
struct Index<'a> {
    nodes: Vec<NodeKey>,
    of: HashMap<NodeKey, u32>,
    set: &'a HashSet<NodeKey>,
    /// Whether a step may go into finer nodes. From a camera inside the
    /// region it never needs to: distance from the camera only grows along a
    /// line from it. From outside, a line enters through the coarse outer
    /// nodes and travels towards finer ones, so it must.
    finer: bool,
}

impl<'a> Index<'a> {
    fn new(set: &'a HashSet<NodeKey>, finer: bool) -> Self {
        let mut nodes: Vec<NodeKey> = set.iter().copied().collect();
        nodes.sort();
        let of = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i as u32))
            .collect();
        Self {
            nodes,
            of,
            set,
            finer,
        }
    }

    /// The sub-block containing world block `p`, looked for in a node of
    /// `level` or the one coarser -- the only neighbours a line of sight can
    /// step into.
    fn locate(&self, p: [i32; 3], levels: &[u32]) -> Option<(u32, usize)> {
        let chunk = ChunkCoord::from_block(p[0], p[1], p[2]);
        let node = levels
            .iter()
            .map(|&l| NodeKey::containing(chunk, l))
            .find(|n| self.set.contains(n))?;
        let origin = node.world_origin();
        let size = (Chunk::SIZE / GRAIN) as i32 * node.extent_chunks();
        let i = |k: usize| ((p[k] - origin[k]) / size) as usize;
        Some((self.of[&node], (i(2) * GRAIN + i(1)) * GRAIN + i(0)))
    }

    /// The sub-blocks across `face` of sub-block `sub` of node `n`: its
    /// sibling, or the same-level or one-coarser sub-block beyond the face --
    /// or, when [`finer`](Self::finer) steps are allowed and neither exists,
    /// the finer sub-blocks touching it.
    fn neighbours(&self, n: u32, sub: usize, face: Face) -> Vec<(u32, usize)> {
        let node = self.nodes[n as usize];
        let c = [sub % GRAIN, sub / GRAIN % GRAIN, sub / GRAIN / GRAIN];
        let d = step_of(face);
        let inside = (0..3).all(|k| (0..GRAIN as i32).contains(&(c[k] as i32 + d[k])));
        if inside {
            let m = [0, 1, 2].map(|k| (c[k] as i32 + d[k]) as usize);
            return vec![(n, (m[2] * GRAIN + m[1]) * GRAIN + m[0])];
        }
        let origin = node.world_origin();
        let size = (Chunk::SIZE / GRAIN) as i32 * node.extent_chunks();
        // A block just outside the face, `along` of the way across it.
        let beyond = |along: [i32; 2]| {
            let mut t = 0;
            [0, 1, 2].map(|k| {
                let min = origin[k] + c[k] as i32 * size;
                match d[k] {
                    1 => min + size,
                    -1 => min - 1,
                    _ => {
                        let at = min + size * along[t] / 4;
                        t += 1;
                        at
                    }
                }
            })
        };
        if let Some(found) = self.locate(beyond([2, 2]), &[node.level, node.level + 1]) {
            return vec![found];
        }
        if !self.finer || node.level == 0 {
            return Vec::new();
        }
        let mut found = Vec::with_capacity(4);
        for along in [[1, 1], [3, 1], [1, 3], [3, 3]] {
            if let Some(v) = self.locate(beyond(along), &[node.level - 1]) {
                if !found.contains(&v) {
                    found.push(v);
                }
            }
        }
        found
    }
}

/// Every node in `nodes` a line of sight from anywhere in `camera`'s node could
/// reach.
///
/// `links(node)` is what joins what inside each of a node's sub-blocks; `None`
/// means not known yet (not generated). An unknown node is included -- it may
/// well be seen -- but not searched through until it is known, which is what
/// lets a caller generate outward along what can be seen instead of generating
/// everything.
///
/// The search starts from **every** sub-block of the camera's node, not only
/// the one the camera is in, so the result holds for any position in that
/// node and only needs working out again when the camera changes node.
pub fn visible_nodes(
    camera: ChunkCoord,
    nodes: &HashSet<NodeKey>,
    links: impl Fn(NodeKey) -> Option<NodeLinks>,
) -> HashSet<NodeKey> {
    let mut visible = HashSet::new();
    let start = (0..=8)
        .map(|level| NodeKey::containing(camera, level))
        .find(|n| nodes.contains(n));
    if nodes.is_empty() {
        return visible;
    }
    let index = Index::new(nodes, start.is_none());
    let subs = GRAIN * GRAIN * GRAIN;
    let known: Vec<Option<NodeLinks>> = index.nodes.iter().map(|&n| links(n)).collect();
    let mut seen = vec![false; index.nodes.len()];
    // For each (node, sub-block, face entered by): the directions taken to get
    // there, kept as the intersection over every path that has arrived.
    // `NONE_YET` is a value no set of six directions can be.
    const NONE_YET: u8 = 0xFF;
    let mut arrived = vec![NONE_YET; index.nodes.len() * subs * 6];
    let mut queue: VecDeque<(u32, usize, Face, u8)> = VecDeque::new();
    let bit = |f: Face| 1u8 << f as u8;

    match start {
        Some(start) => {
            let start_n = index.of[&start];
            seen[start_n as usize] = true;
            for sub in 0..subs {
                for face in FACES {
                    for (n, s) in index.neighbours(start_n, sub, face) {
                        queue.push_back((n, s, opposite(face), bit(face)));
                    }
                }
            }
        }
        // **A camera outside the region altogether.** A line of sight from
        // there enters through an outer face on the camera's side of it, moving
        // inward; which other ways it is moving is unknown, so only inward is
        // taken. Every such face is a place to start.
        None => {
            for (n, node) in index.nodes.iter().enumerate() {
                let origin = node.chunk_origin();
                let extent = node.extent_chunks();
                let o = [origin.x, origin.y, origin.z];
                let c = [camera.x, camera.y, camera.z];
                for sub in 0..subs {
                    for face in FACES {
                        if !index.neighbours(n as u32, sub, face).is_empty() {
                            continue;
                        }
                        let d = step_of(face);
                        let k = (0..3).find(|&k| d[k] != 0).expect("an axis");
                        let facing_camera = if d[k] > 0 {
                            c[k] >= o[k] + extent
                        } else {
                            c[k] < o[k]
                        };
                        if facing_camera {
                            queue.push_back((n as u32, sub, face, bit(opposite(face))));
                        }
                    }
                }
            }
        }
    }

    while let Some((n, sub, entered_by, taken)) = queue.pop_front() {
        seen[n as usize] = true;
        let slot = &mut arrived[(n as usize * subs + sub) * 6 + entered_by as usize];
        let taken = match *slot {
            // Nothing new: an earlier path already forbids no more than this.
            before if before != NONE_YET && before & !taken == 0 => continue,
            before if before != NONE_YET => before & taken,
            _ => taken,
        };
        *slot = taken;
        let Some(inside) = known[n as usize] else {
            continue;
        };
        for out in FACES {
            if taken & bit(opposite(out)) != 0 || !inside[sub].joins(entered_by, out) {
                continue;
            }
            for (m, s) in index.neighbours(n, sub, out) {
                queue.push_back((m, s, opposite(out), taken | bit(out)));
            }
        }
    }
    for (i, was) in seen.into_iter().enumerate() {
        if was {
            visible.insert(index.nodes[i]);
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::desired_nodes_3d;
    use crate::{TerrainBlocks, World};
    use cubara_voxel::{BlockId, BlockRegistry, Chunk, DropRule, Faces, Interact, Material, Shape};

    fn registry() -> BlockRegistry {
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
        .expect("fixture registry")
    }

    /// A small world region and what every node in it draws.
    struct Scene {
        nodes: HashSet<NodeKey>,
        chunks: HashMap<NodeKey, Option<Chunk>>,
        links: HashMap<NodeKey, NodeLinks>,
    }

    impl Scene {
        fn around(world: &World, camera: ChunkCoord, registry: &BlockRegistry) -> Self {
            let blocks = TerrainBlocks::from_registry(registry);
            let schedule = &[(0u32, 3), (1, 6), (2, 12)];
            let nodes: HashSet<NodeKey> =
                desired_nodes_3d(camera, 2, schedule).into_iter().collect();
            let mut chunks = HashMap::new();
            let mut links = HashMap::new();
            for &n in &nodes {
                let chunk = world.node_at(n, blocks);
                links.insert(n, node_links(chunk.as_ref(), registry));
                chunks.insert(n, chunk);
            }
            Self {
                nodes,
                chunks,
                links,
            }
        }

        /// The node drawing block `b`, and whether it draws it solid.
        fn at(&self, b: [i32; 3]) -> Option<(NodeKey, bool)> {
            let chunk = ChunkCoord::from_block(b[0], b[1], b[2]);
            let node = (0..=2)
                .map(|l| NodeKey::containing(chunk, l))
                .find(|n| self.nodes.contains(n))?;
            let (o, s) = (node.world_origin(), node.extent_chunks());
            let i = |k: usize| ((b[k] - o[k]) / s) as usize;
            let solid = self.chunks[&node]
                .as_ref()
                .is_some_and(|c| c.get(i(0), i(1), i(2)) != BlockId::AIR);
            Some((node, solid))
        }
    }

    /// Walk the blocks a ray passes through (Amanatides & Woo), returning the
    /// first node that draws one of them solid, if the ray hits anything
    /// before leaving the scene.
    fn first_hit(scene: &Scene, eye: [f32; 3], dir: [f32; 3]) -> Option<NodeKey> {
        let mut cell = [
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ];
        let mut step = [0i32; 3];
        let mut t_max = [f32::INFINITY; 3];
        let mut t_delta = [f32::INFINITY; 3];
        for k in 0..3 {
            if dir[k] > 0.0 {
                step[k] = 1;
                t_max[k] = (cell[k] as f32 + 1.0 - eye[k]) / dir[k];
                t_delta[k] = 1.0 / dir[k];
            } else if dir[k] < 0.0 {
                step[k] = -1;
                t_max[k] = (eye[k] - cell[k] as f32) / -dir[k];
                t_delta[k] = 1.0 / -dir[k];
            }
        }
        let mut entered = false;
        for _ in 0..4000 {
            let Some((node, solid)) = scene.at(cell) else {
                // Outside the scene: before entering, keep going; after
                // leaving, the ray is done.
                if entered {
                    return None;
                }
                let k = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
                    0
                } else if t_max[1] < t_max[2] {
                    1
                } else {
                    2
                };
                cell[k] += step[k];
                t_max[k] += t_delta[k];
                continue;
            };
            entered = true;
            if solid {
                return Some(node);
            }
            let k = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
                0
            } else if t_max[1] < t_max[2] {
                1
            } else {
                2
            };
            cell[k] += step[k];
            t_max[k] += t_delta[k];
        }
        None
    }

    /// **Nothing a straight line of sight reaches is left out.** About 1,200
    /// rays each from a camera above the ground, one inside a cave and one high in
    /// the air, through a real world with caves and levels of detail.
    #[test]
    fn every_node_a_line_of_sight_hits_is_visible() {
        let registry = registry();
        let blocks = TerrainBlocks::from_registry(&registry);
        let world = World::new();
        let surface = world.surface_height(8, 8);
        // A cave: the first air below the surface near the origin.
        let cave = (0..400)
            .flat_map(|i| {
                let (x, z) = (i % 20, i / 20);
                (surface - 60..surface - 4).map(move |y| [x, y, z])
            })
            .find(|p| !world.is_solid_at(p[0], p[1], p[2], blocks))
            .expect("a cave below the origin");
        // Each camera with the centre of the region it looks at. The last is
        // far outside its region, the way the bench's orbit is.
        let cases = [
            ([8.3, surface as f32 + 6.7, 8.1], None),
            (
                [
                    cave[0] as f32 + 0.37,
                    cave[1] as f32 + 0.41,
                    cave[2] as f32 + 0.53,
                ],
                None,
            ),
            ([5.2, 190.6, -3.3], None),
            ([350.4, 420.3, -280.7], Some(ChunkCoord::new(0, 1, 0))),
        ];

        let mut culled_something = false;
        for (eye, around) in cases {
            let camera = ChunkCoord::from_world_pos(eye);
            let scene = Scene::around(&world, around.unwrap_or(camera), &registry);
            let visible = visible_nodes(camera, &scene.nodes, |n| scene.links.get(&n).copied());
            culled_something |= visible.len() < scene.nodes.len();

            // Every direction on a lattice, nudged off it so no ray runs exactly
            // along an axis or through an edge. The walk does not need them
            // normalised, and this crate keeps floating-point trigonometry out
            // of simulation code (`scripts/check-architecture.sh`, Rule 1).
            let mut dirs = Vec::new();
            for x in -7i32..=7 {
                for y in -7i32..=7 {
                    for z in -7i32..=7 {
                        if x.abs().max(y.abs()).max(z.abs()) == 7 {
                            dirs.push([x as f32 + 0.137, y as f32 + 0.071, z as f32 + 0.229]);
                        }
                    }
                }
            }
            for dir in dirs {
                if let Some(hit) = first_hit(&scene, eye, dir) {
                    assert!(
                        visible.contains(&hit),
                        "eye {eye:?}: a ray along {dir:?} sees {hit:?}, which was culled"
                    );
                }
            }
        }
        assert!(
            culled_something,
            "nothing was culled anywhere, so this proves nothing"
        );
    }
}
