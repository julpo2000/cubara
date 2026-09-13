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

use cubara_voxel::{ChunkCoord, Face, FaceLinks};

use crate::node::NodeKey;

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

/// The node of `nodes` across `face` of `node`: the one beside it at the same
/// level, or the coarser one containing that space.
///
/// **Never a finer one**, and not because it cannot exist. Detail is chosen by
/// distance from the camera, and along a straight line from the camera that
/// distance only grows -- so a line of sight never passes from a coarser node
/// into a finer one. Stepping into finer nodes would only let the search wander
/// back towards the camera, which can only add nodes nobody sees (measured: the
/// line-of-sight test passes without it).
fn neighbour(node: NodeKey, face: Face, nodes: &HashSet<NodeKey>) -> Option<NodeKey> {
    let d = step_of(face);
    let same = NodeKey::new(
        node.level,
        [node.pos[0] + d[0], node.pos[1] + d[1], node.pos[2] + d[2]],
    );
    if nodes.contains(&same) {
        return Some(same);
    }
    let coarser = NodeKey::containing(same.chunk_origin(), node.level + 1);
    nodes.contains(&coarser).then_some(coarser)
}

/// Every node in `nodes` a line of sight from `camera` could reach.
///
/// `links(node)` is what joins what inside a node; `None` means not known yet
/// (not generated). An unknown node is included -- it may well be seen -- but
/// not searched through until it is known, which is what lets a caller generate
/// outward along what can be seen instead of generating everything.
pub fn visible_nodes(
    camera: ChunkCoord,
    nodes: &HashSet<NodeKey>,
    links: impl Fn(NodeKey) -> Option<FaceLinks>,
) -> HashSet<NodeKey> {
    let mut visible = HashSet::new();
    let Some(start) = (0..=8)
        .map(|level| NodeKey::containing(camera, level))
        .find(|n| nodes.contains(n))
    else {
        return visible;
    };
    visible.insert(start);

    // For each (node, face it was entered by): the directions taken to get
    // there, kept as the intersection over every path that has arrived.
    let mut arrived: HashMap<(NodeKey, Face), u8> = HashMap::new();
    let mut queue: VecDeque<(NodeKey, Face, u8)> = VecDeque::new();
    let bit = |f: Face| 1u8 << f as u8;

    let mut enqueue = |queue: &mut VecDeque<(NodeKey, Face, u8)>,
                       visible: &mut HashSet<NodeKey>,
                       node: NodeKey,
                       entered_by: Face,
                       taken: u8| {
        visible.insert(node);
        let merged = match arrived.get(&(node, entered_by)) {
            // Nothing new: an earlier path already forbids no more than this.
            Some(&before) if before & !taken == 0 => return,
            Some(&before) => before & taken,
            None => taken,
        };
        arrived.insert((node, entered_by), merged);
        queue.push_back((node, entered_by, merged));
    };

    // From inside the camera's node, any face may be the way out.
    for face in FACES {
        if let Some(n) = neighbour(start, face, nodes) {
            enqueue(&mut queue, &mut visible, n, opposite(face), bit(face));
        }
    }

    while let Some((node, entered_by, taken)) = queue.pop_front() {
        let Some(inside) = links(node) else {
            continue;
        };
        for out in FACES {
            if taken & bit(opposite(out)) != 0 || !inside.joins(entered_by, out) {
                continue;
            }
            if let Some(n) = neighbour(node, out, nodes) {
                enqueue(&mut queue, &mut visible, n, opposite(out), taken | bit(out));
            }
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
        links: HashMap<NodeKey, FaceLinks>,
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
                links.insert(
                    n,
                    chunk
                        .as_ref()
                        .map_or(FaceLinks::ALL, |c| FaceLinks::of_chunk(c, registry)),
                );
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
        for _ in 0..2000 {
            let (node, solid) = scene.at(cell)?;
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

    /// **Nothing a straight line of sight reaches is left out.** Thousands of
    /// rays from a camera above the ground, one inside a cave and one high in
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
        let eyes = [
            [8.3, surface as f32 + 6.7, 8.1],
            [
                cave[0] as f32 + 0.37,
                cave[1] as f32 + 0.41,
                cave[2] as f32 + 0.53,
            ],
            [5.2, 190.6, -3.3],
        ];

        let mut culled_something = false;
        for eye in eyes {
            let camera = ChunkCoord::from_world_pos(eye);
            let scene = Scene::around(&world, camera, &registry);
            let visible = visible_nodes(camera, &scene.nodes, |n| scene.links.get(&n).copied());
            culled_something |= visible.len() < scene.nodes.len();

            // Directions on a spiral over the sphere, none axis-aligned.
            let rays = 3000;
            for i in 0..rays {
                let t = (i as f32 + 0.5) / rays as f32;
                let polar = (1.0 - 2.0 * t).acos();
                let azimuth = i as f32 * 2.399_963_2 + 0.137;
                let dir = [
                    polar.sin() * azimuth.cos(),
                    polar.cos(),
                    polar.sin() * azimuth.sin(),
                ];
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
