//! Which faces of a chunk can see each other through it.
//!
//! The per-node half of visibility culling (`cubara_world::visibility` is the
//! other): if no air path inside a chunk joins face A to face B, then nothing
//! entering through A can leave through B, so a line of sight that needs to
//! cannot exist. A chunk of solid rock joins nothing; a chunk of sky joins
//! everything.
//!
//! Air here means "not solid" per the registry -- the same test the mesher uses
//! to decide where faces go, so what this says can be seen through is exactly
//! what is drawn as open.

use crate::block::BlockId;
use crate::mesh::Face;
use crate::registry::BlockRegistry;
use crate::voxel::Chunk;

/// For each unordered pair of the six faces, whether air inside the chunk joins
/// them. Fifteen pairs, one bit each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceLinks(u16);

const ALL_FACES: [Face; 6] = [
    Face::PosX,
    Face::NegX,
    Face::PosY,
    Face::NegY,
    Face::PosZ,
    Face::NegZ,
];

/// The bit for the pair `(a, b)`, order-free.
fn pair_bit(a: Face, b: Face) -> u16 {
    let (i, j) = {
        let (i, j) = (a as u16, b as u16);
        (i.min(j), i.max(j))
    };
    // Index of (i, j), i < j, in the upper triangle of a 6x6 matrix.
    1 << (i * (11 - i) / 2 + j - i - 1)
}

impl FaceLinks {
    /// Nothing joins anything: solid.
    pub const NONE: Self = Self(0);
    /// Everything joins everything: open.
    pub const ALL: Self = Self((1 << 15) - 1);

    /// Whether air inside joins face `a` to face `b`. A face is never "joined"
    /// to itself: leaving by the face you came in is going backwards.
    pub fn joins(self, a: Face, b: Face) -> bool {
        a != b && self.0 & pair_bit(a, b) != 0
    }

    /// Worked out from `chunk`'s blocks at the chunk's own resolution.
    ///
    /// Flood-fills each separate pocket of air once and records every face
    /// that pocket touches; any two of those are joined. Linear in the number
    /// of cells, so it costs about as much as meshing the chunk once did.
    pub fn of_chunk(chunk: &Chunk, registry: &BlockRegistry) -> Self {
        let n = Chunk::SIZE;
        let open = |id: BlockId| !registry.is_solid(id);
        let index = |x: usize, y: usize, z: usize| (z * n + y) * n + x;

        let mut seen = vec![false; n * n * n];
        let mut stack: Vec<(usize, usize, usize)> = Vec::new();
        let mut links = 0u16;

        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    if seen[index(x, y, z)] || !open(chunk.get(x, y, z)) {
                        continue;
                    }
                    seen[index(x, y, z)] = true;
                    stack.push((x, y, z));
                    let mut touched = 0u8;
                    while let Some((x, y, z)) = stack.pop() {
                        let edge = n - 1;
                        touched |= (x == edge) as u8
                            | ((x == 0) as u8) << 1
                            | ((y == edge) as u8) << 2
                            | ((y == 0) as u8) << 3
                            | ((z == edge) as u8) << 4
                            | ((z == 0) as u8) << 5;
                        let mut visit = |x: usize, y: usize, z: usize| {
                            let i = index(x, y, z);
                            if !seen[i] && open(chunk.get(x, y, z)) {
                                seen[i] = true;
                                stack.push((x, y, z));
                            }
                        };
                        if x > 0 {
                            visit(x - 1, y, z);
                        }
                        if x < edge {
                            visit(x + 1, y, z);
                        }
                        if y > 0 {
                            visit(x, y - 1, z);
                        }
                        if y < edge {
                            visit(x, y + 1, z);
                        }
                        if z > 0 {
                            visit(x, y, z - 1);
                        }
                        if z < edge {
                            visit(x, y, z + 1);
                        }
                    }
                    for a in ALL_FACES {
                        for b in ALL_FACES {
                            if a != b
                                && touched & (1 << a as u8) != 0
                                && touched & (1 << b as u8) != 0
                            {
                                links |= pair_bit(a, b);
                            }
                        }
                    }
                }
            }
        }
        Self(links)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{DropRule, Faces, Interact, Material, Shape};

    fn registry() -> BlockRegistry {
        BlockRegistry::from_materials(vec![(
            std::path::PathBuf::from("test-fixture.ron"),
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
        )])
        .expect("fixture registry is valid")
    }

    #[test]
    fn every_pair_has_its_own_bit() {
        let mut bits = std::collections::HashSet::new();
        for a in ALL_FACES {
            for b in ALL_FACES {
                if a != b {
                    assert_eq!(
                        pair_bit(a, b),
                        pair_bit(b, a),
                        "order matters for {a:?} {b:?}"
                    );
                    bits.insert(pair_bit(a, b));
                }
            }
        }
        assert_eq!(bits.len(), 15);
        assert_eq!(bits.iter().fold(0, |acc, b| acc | b), FaceLinks::ALL.0);
    }

    #[test]
    fn solid_rock_joins_nothing_and_open_air_joins_everything() {
        let r = registry();
        let rock = Chunk::from_fn(|_, _, _| BlockId::STONE);
        let sky = Chunk::from_fn(|_, _, _| BlockId::AIR);
        assert_eq!(FaceLinks::of_chunk(&rock, &r), FaceLinks::NONE);
        assert_eq!(FaceLinks::of_chunk(&sky, &r), FaceLinks::ALL);
    }

    /// A straight tunnel along x joins its two ends and nothing else.
    #[test]
    fn a_tunnel_joins_only_the_faces_it_opens_onto() {
        let r = registry();
        let tunnel = Chunk::from_fn(|_, y, z| {
            if (7..9).contains(&y) && (7..9).contains(&z) {
                BlockId::AIR
            } else {
                BlockId::STONE
            }
        });
        let links = FaceLinks::of_chunk(&tunnel, &r);
        assert!(links.joins(Face::PosX, Face::NegX));
        assert!(!links.joins(Face::PosX, Face::PosY), "rock between them");
        assert!(!links.joins(Face::PosY, Face::NegY));
        assert!(
            !links.joins(Face::NegX, Face::NegX),
            "going back the way it came"
        );
    }

    /// Two pockets that do not meet: each joins its own faces, not the other's.
    #[test]
    fn separate_pockets_do_not_join_each_other() {
        let r = registry();
        let chunk = Chunk::from_fn(|x, y, _| {
            let low_left = x < 4 && y < 4; // touches -x and -y (and both z)
            let high_right = x > 11 && y > 11; // touches +x and +y
            if low_left || high_right {
                BlockId::AIR
            } else {
                BlockId::STONE
            }
        });
        let links = FaceLinks::of_chunk(&chunk, &r);
        assert!(links.joins(Face::NegX, Face::NegY));
        assert!(links.joins(Face::PosX, Face::PosY));
        assert!(!links.joins(Face::NegX, Face::PosX), "joined through rock");
        assert!(!links.joins(Face::NegY, Face::PosY), "joined through rock");
        // Both pockets run the full depth, so each joins the z faces.
        assert!(links.joins(Face::PosZ, Face::NegZ));
    }

    /// A pocket wholly inside touches no face, so it joins nothing.
    #[test]
    fn a_sealed_pocket_joins_nothing() {
        let r = registry();
        let chunk = Chunk::from_fn(|x, y, z| {
            if (5..10).contains(&x) && (5..10).contains(&y) && (5..10).contains(&z) {
                BlockId::AIR
            } else {
                BlockId::STONE
            }
        });
        assert_eq!(FaceLinks::of_chunk(&chunk, &r), FaceLinks::NONE);
    }
}
