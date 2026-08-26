//! What a structure looks like — loaded from `assets/structures/*.ron`.
//!
//! **Shape only.** *Where* a structure goes is
//! `docs/PHASE1_ARCHITECTURE.md` §8.4's structure pass, which is code in
//! `cubara_world::worldgen` and stays there: placement has to be a pure
//! function of `(seed, coord)` with a declared radius, and that is an
//! algorithm, not a number. This file is the part that can be retuned without
//! a recompile (`REQUIREMENTS.md` #3).
//!
//! Loaded here rather than in `cubara-world` because this is where the RON
//! machinery already lives, next to blocks, items and recipes — `cubara-world`
//! has no serde dependency and should not grow one to parse a second copy of
//! the same thing.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A canopy: which block, and how far it reaches from the trunk.
#[derive(Debug, Clone, Deserialize)]
pub struct CanopyDef {
    pub block: String,
    pub radius: i32,
}

/// One structure, as authored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "Structure")]
pub struct StructureDef {
    pub name: String,
    pub trunk: TrunkDef,
    pub canopy: CanopyDef,
    /// The block a structure will grow on, by name. Nothing grows on anything
    /// else, which is what keeps trees off stone and out of caves.
    pub grows_on: String,
    /// **One in N chunk columns** carries one of these.
    ///
    /// An integer, not a float, deliberately: placement must be bit-identical
    /// across platforms (§8.5), and an integer comparison is the cheapest way
    /// to guarantee that. Lower is denser.
    pub density: u32,
}

/// A trunk: which block, and how tall it may grow.
#[derive(Debug, Clone, Deserialize)]
pub struct TrunkDef {
    pub block: String,
    /// Inclusive `(min, max)`, resolved from the same hash that chose the
    /// tree's position — so a tree's size is as reproducible as its location.
    pub height: (i32, i32),
}

#[derive(Debug)]
pub enum StructureError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// `density: 0` would mean "every column", which is not a forest, it is a
    /// solid block of wood.
    ZeroDensity { name: String, file: PathBuf },
    /// A height range that cannot produce a tree.
    BadHeight {
        name: String,
        height: (i32, i32),
        file: PathBuf,
    },
}

impl fmt::Display for StructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StructureError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            StructureError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            StructureError::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate structure {name:?}: {} and {}",
                first.display(),
                second.display()
            ),
            StructureError::ZeroDensity { name, file } => write!(
                f,
                "{}: structure {name:?} has density 0, which would put one in every \
                 chunk column",
                file.display()
            ),
            StructureError::BadHeight { name, height, file } => write!(
                f,
                "{}: structure {name:?} has height {height:?}; both must be positive \
                 and min must not exceed max",
                file.display()
            ),
        }
    }
}

impl std::error::Error for StructureError {}

/// Every structure that exists, by name.
#[derive(Debug, Default)]
pub struct StructureRegistry {
    by_name: HashMap<String, StructureDef>,
}

impl StructureRegistry {
    pub fn load(dir: &Path) -> Result<Self, StructureError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| StructureError::Io {
            file: dir.to_path_buf(),
            error,
        })?;

        let mut defs = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| StructureError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| StructureError::Io {
                file: path.clone(),
                error,
            })?;
            let def: StructureDef =
                ron::from_str(&text).map_err(|error| StructureError::Parse {
                    file: path.clone(),
                    error,
                })?;
            defs.push((path, def));
        }
        Self::from_defs(defs)
    }

    pub fn from_defs(defs: Vec<(PathBuf, StructureDef)>) -> Result<Self, StructureError> {
        let mut by_name: HashMap<String, StructureDef> = HashMap::new();
        let mut files: HashMap<String, PathBuf> = HashMap::new();

        for (file, def) in defs {
            if def.density == 0 {
                return Err(StructureError::ZeroDensity {
                    name: def.name,
                    file,
                });
            }
            let (lo, hi) = def.trunk.height;
            if lo <= 0 || hi < lo {
                return Err(StructureError::BadHeight {
                    name: def.name,
                    height: (lo, hi),
                    file,
                });
            }
            if let Some(first) = files.get(&def.name) {
                return Err(StructureError::DuplicateName {
                    name: def.name,
                    first: first.clone(),
                    second: file,
                });
            }
            files.insert(def.name.clone(), file);
            by_name.insert(def.name.clone(), def);
        }
        Ok(Self { by_name })
    }

    pub fn get(&self, name: &str) -> Option<&StructureDef> {
        self.by_name.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, density: u32, height: (i32, i32)) -> (PathBuf, StructureDef) {
        (
            PathBuf::from(format!("{name}.ron")),
            StructureDef {
                name: name.to_string(),
                trunk: TrunkDef {
                    block: "cubara:oak_log".to_string(),
                    height,
                },
                canopy: CanopyDef {
                    block: "cubara:oak_leaves".to_string(),
                    radius: 2,
                },
                grows_on: "cubara:grass".to_string(),
                density,
            },
        )
    }

    #[test]
    fn the_shipped_oak_loads_and_is_shaped_like_a_tree() {
        // Loads the real assets/structures/, not a fixture: a data file that
        // parses in a test but not in the game is the failure worth catching.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/structures");
        let r = StructureRegistry::load(&dir).expect("assets/structures must load");
        let oak = r.get("cubara:oak").expect("cubara:oak is defined");

        assert_eq!(oak.trunk.block, "cubara:oak_log");
        assert_eq!(oak.canopy.block, "cubara:oak_leaves");
        assert_eq!(oak.grows_on, "cubara:grass");
        assert!(oak.canopy.radius >= 1, "a canopy of nothing is not a tree");
        assert!(
            oak.trunk.height.0 > oak.canopy.radius,
            "the trunk must be taller than the canopy is wide, or the leaves \
             sit on the ground"
        );
    }

    #[test]
    fn a_density_of_zero_is_rejected() {
        // Would put a tree in every chunk column -- not a forest, a solid
        // block of wood, and the kind of typo that is obvious in play and
        // invisible in review.
        let err = StructureRegistry::from_defs(vec![def("cubara:oak", 0, (4, 6))])
            .expect_err("density 0 must not load");
        assert!(matches!(err, StructureError::ZeroDensity { .. }), "{err}");
    }

    #[test]
    fn impossible_heights_are_rejected() {
        for height in [(0, 4), (-1, 3), (6, 4)] {
            let r = StructureRegistry::from_defs(vec![def("cubara:oak", 12, height)]);
            assert!(r.is_err(), "height {height:?} should not load");
        }
    }
}
