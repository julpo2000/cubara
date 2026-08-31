//! The block registry: which blocks exist, and the runtime ids assigned to
//! them.
//!
//! Definitions are authored as *materials* — a texture set plus the shapes it
//! comes in (`docs/PHASE1_ARCHITECTURE.md` §3.2) — in RON files under
//! `assets/blocks/`. The registry expands each material's shapes into a flat
//! id space and assigns runtime [`BlockId`]s by sorting the expanded names
//! lexicographically, so the same definitions always produce the same ids on
//! every machine and every run (§3.4): **names are identity, numbers are
//! per-world**. `BlockId(0)` is always air, pinned regardless of sort order —
//! air isn't authored as a [`Material`] at all.
//!
//! This module knows nothing about the GPU (`ARCHITECTURE.md` Rule 4): it
//! resolves texture *names*, not array layers. `cubara-render` maps names to
//! layers when it builds the texture array (block 1.4) — that seam is what
//! keeps block definitions GPU-free.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::block::BlockId;
use crate::mesh::Face;

/// The physical shape a material comes in. Phase 1 has only `Full`; a
/// material with more (`Stair`, `Slab`, …) would expand to one id per shape
/// (§3.2). The one-element match in [`Shape::expand`] is written this way now
/// because it costs nothing and is the actual current data — not a slot
/// reserved for a shape that doesn't exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Shape {
    Full,
}

impl Shape {
    /// The expanded block name for this material+shape pair. `Full` is a
    /// material's base form, so it takes the material's own name; a future
    /// shape would append a suffix (e.g. a stairs variant).
    fn expand(self, material_name: &str) -> String {
        match self {
            Shape::Full => material_name.to_string(),
        }
    }
}

/// Which texture each face of a block samples, by name — resolved to a
/// texture-array layer by `cubara-render`, not here (§3.2).
#[derive(Debug, Clone, Deserialize)]
pub enum Faces {
    /// Every face uses the same texture.
    All(String),
    /// Top, the four side faces, and bottom use different textures (e.g.
    /// grass: green top, dirt bottom, blended side).
    Sided {
        top: String,
        side: String,
        bottom: String,
    },
}

impl Faces {
    /// Every texture name this definition references. May repeat (e.g.
    /// `All`'s one name); callers that care about uniqueness dedupe.
    fn texture_names(&self) -> Vec<&str> {
        match self {
            Faces::All(name) => vec![name.as_str()],
            Faces::Sided { top, side, bottom } => {
                vec![top.as_str(), side.as_str(), bottom.as_str()]
            }
        }
    }

    /// The texture name for a specific face direction -- `PosY` is top,
    /// `NegY` is bottom, and the four horizontal directions are all "side".
    /// `All` materials ignore `face` entirely.
    fn texture_for(&self, face: Face) -> &str {
        match self {
            Faces::All(name) => name.as_str(),
            Faces::Sided { top, side, bottom } => match face {
                Face::PosY => top.as_str(),
                Face::NegY => bottom.as_str(),
                Face::PosX | Face::NegX | Face::PosZ | Face::NegZ => side.as_str(),
            },
        }
    }
}

/// One material definition, as authored in `assets/blocks/*.ron` — see the
/// module docs for the RON shape.
#[derive(Debug, Clone, Deserialize)]
pub struct Material {
    pub name: String,
    pub solid: bool,
    pub faces: Faces,
    pub shapes: Vec<Shape>,
    /// What breaking this yields (`PHASE2_ARCHITECTURE.md` §4). Absent means
    /// [`DropRule::SameName`] -- see that type for why the three states are
    /// named rather than `Option`.
    #[serde(default)]
    pub drops: DropRule,
    /// The tool tier needed to get anything out of this block: **0 hand,
    /// 1 wood, 2 stone, 3 iron** (§4). A lower tier still *breaks* the block;
    /// it just yields nothing.
    #[serde(default)]
    pub requires_tier: u8,
    /// How much work breaking this takes (`PHASE2_ARCHITECTURE.md` §4.3), against
    /// the held tool's speed: `ticks = ceil(hardness / speed)`.
    ///
    /// **Absent means unbreakable**, not instant -- `Some(0)` is instant. A
    /// block that forgot to declare hardness therefore becomes conspicuously
    /// unminable rather than silently free, which is the failure that gets
    /// noticed and fixed.
    #[serde(default)]
    pub hardness: Option<u32>,
}

/// What a block yields when broken.
///
/// **Three named states rather than `Option<ItemDrop>`**, which is a
/// deliberate deviation from §4's sketch (`drops: Some(..)` / `drops: None`).
/// `Option` has only two, and this needs three: *drop my own name* (the
/// pre-2.4 policy, and what every block file that says nothing still means),
/// *drop this specific item*, and *drop nothing at all*.
///
/// Writing that with `Option` would make an **omitted** field and an explicit
/// `None` the same value -- so a new block file that simply forgot to mention
/// `drops` would silently yield nothing. That exact failure already happened
/// once in this project (§4.2: nine item files shipped, three blocks left
/// without, and breaking them silently did nothing), and it is worth a small
/// syntax deviation to make it unrepresentable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub enum DropRule {
    /// Yield the item with the same name as this block. The default, so the
    /// block files written before 2.4 keep their existing behaviour without
    /// being edited.
    #[default]
    SameName,
    /// Yield nothing, whatever tool is held. Leaves, per §5.
    Nothing,
    /// Yield exactly this.
    Item(ItemDrop),
}

/// A specific drop: which item, and how many.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ItemDrop {
    pub item: String,
    pub count: u8,
}

/// A validation failure while building a [`BlockRegistry`]. Every variant
/// names the offending file and block, so a bad definition is diagnosable
/// from the error message alone.
#[derive(Debug)]
pub enum RegistryError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    /// Two materials (or two shapes of the same material) expand to the same
    /// block name.
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// A material's `shapes` list is empty, so it expands to no block at all.
    EmptyShapes { name: String, file: PathBuf },
    /// A material's `faces` names a texture with no matching file in the
    /// directory checked by [`BlockRegistry::validate_textures`].
    UnknownTexture {
        name: String,
        texture: String,
        file: PathBuf,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            RegistryError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            RegistryError::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "block \"{name}\" is defined twice: {} and {}",
                first.display(),
                second.display()
            ),
            RegistryError::EmptyShapes { name, file } => write!(
                f,
                "{}: block \"{name}\" has an empty `shapes` list -- it would expand to no block \
                 at all",
                file.display()
            ),
            RegistryError::UnknownTexture {
                name,
                texture,
                file,
            } => write!(
                f,
                "{}: block \"{name}\" references texture \"{texture}\", which no file in the \
                 textures directory provides",
                file.display()
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

/// A block's runtime entry: everything the registry knows about one id.
#[derive(Debug)]
struct Entry {
    name: String,
    solid: bool,
    /// `None` only for air, which isn't authored as a [`Material`].
    faces: Option<Faces>,
    /// The file this entry was defined in, for [`RegistryError`] messages.
    /// Meaningless (empty) for air.
    file: PathBuf,
    drops: DropRule,
    requires_tier: u8,
    hardness: Option<u32>,
}

/// Runtime block identity: which blocks exist, and the [`BlockId`]s assigned
/// to them for this process. Ids are **not** stable across processes with a
/// different set of loaded materials — see the module docs.
#[derive(Debug)]
pub struct BlockRegistry {
    /// Indexed by `BlockId.0 as usize`; `entries[0]` is always air.
    entries: Vec<Entry>,
    by_name: HashMap<String, BlockId>,
}

impl BlockRegistry {
    /// Build a registry from every `*.ron` file directly inside `dir`, one
    /// [`Material`] per file. File read order never affects the result — ids
    /// come from the sorted expanded names, not arrival order (see
    /// `from_materials`).
    pub fn load(dir: &Path) -> Result<Self, RegistryError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| RegistryError::Io {
            file: dir.to_path_buf(),
            error,
        })?;

        let mut materials = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| RegistryError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| RegistryError::Io {
                file: path.clone(),
                error,
            })?;
            let material: Material =
                ron::from_str(&text).map_err(|error| RegistryError::Parse {
                    file: path.clone(),
                    error,
                })?;
            materials.push((path, material));
        }
        Self::from_materials(materials)
    }

    /// Build a registry from materials already in memory, each paired with
    /// the file it should be blamed on in a validation error. `load` reduces
    /// to this after reading the directory; tests use it directly, so
    /// validation is testable with no filesystem involved.
    pub fn from_materials(materials: Vec<(PathBuf, Material)>) -> Result<Self, RegistryError> {
        struct Expanded {
            name: String,
            solid: bool,
            faces: Faces,
            file: PathBuf,
            drops: DropRule,
            requires_tier: u8,
            hardness: Option<u32>,
        }

        let mut expanded: Vec<Expanded> = Vec::new();
        let mut first_seen: HashMap<String, PathBuf> = HashMap::new();

        for (file, material) in &materials {
            if material.shapes.is_empty() {
                return Err(RegistryError::EmptyShapes {
                    name: material.name.clone(),
                    file: file.clone(),
                });
            }
            for &shape in &material.shapes {
                let name = shape.expand(&material.name);
                if let Some(first) = first_seen.get(&name) {
                    return Err(RegistryError::DuplicateName {
                        name,
                        first: first.clone(),
                        second: file.clone(),
                    });
                }
                first_seen.insert(name.clone(), file.clone());
                expanded.push(Expanded {
                    name,
                    solid: material.solid,
                    faces: material.faces.clone(),
                    file: file.clone(),
                    drops: material.drops.clone(),
                    requires_tier: material.requires_tier,
                    hardness: material.hardness,
                });
            }
        }

        // The whole point: sorted by name, not by arrival order, so file read
        // order (directory iteration order is platform-defined) never moves an
        // id.
        expanded.sort_by(|a, b| a.name.cmp(&b.name));

        let mut entries = vec![Entry {
            name: "cubara:air".to_string(),
            solid: false,
            faces: None,
            file: PathBuf::new(),
            // Air is never broken, so this is unreachable rather than
            // meaningful -- `Nothing` is the answer that cannot leak an item
            // if it ever is reached.
            drops: DropRule::Nothing,
            requires_tier: 0,
            hardness: None,
        }];
        let mut by_name = HashMap::new();
        by_name.insert("cubara:air".to_string(), BlockId::AIR);

        for e in expanded {
            let id = BlockId(entries.len() as u16);
            by_name.insert(e.name.clone(), id);
            entries.push(Entry {
                name: e.name,
                solid: e.solid,
                faces: Some(e.faces),
                file: e.file,
                drops: e.drops,
                requires_tier: e.requires_tier,
                hardness: e.hardness,
            });
        }

        Ok(Self { entries, by_name })
    }

    /// What breaking `id` yields, before the tier check.
    ///
    /// Unknown ids report [`DropRule::Nothing`]: the safe answer, since it can
    /// only ever cost a drop rather than invent one.
    pub fn drops(&self, id: BlockId) -> DropRule {
        self.entries
            .get(id.0 as usize)
            .map(|e| e.drops.clone())
            .unwrap_or(DropRule::Nothing)
    }

    /// The tool tier `id` needs before it yields anything (§4).
    ///
    /// Unknown ids report `u8::MAX` -- nothing satisfies it. Same stance as
    /// [`drops`](Self::drops): fail towards yielding nothing.
    pub fn requires_tier(&self, id: BlockId) -> u8 {
        self.entries
            .get(id.0 as usize)
            .map(|e| e.requires_tier)
            .unwrap_or(u8::MAX)
    }

    /// How much work breaking `id` takes, or `None` if it cannot be broken
    /// (§4.3). Air and unknown ids are unbreakable.
    pub fn hardness(&self, id: BlockId) -> Option<u32> {
        self.entries.get(id.0 as usize).and_then(|e| e.hardness)
    }

    /// Whether `id` is solid. Unknown ids (shouldn't happen -- a `Chunk` only
    /// ever holds ids this same registry assigned) report not-solid rather
    /// than panicking, since a mismatched registry is a loud bug elsewhere,
    /// not something meshing should crash over.
    pub fn is_solid(&self, id: BlockId) -> bool {
        self.entries
            .get(id.0 as usize)
            .map(|e| e.solid)
            .unwrap_or(false)
    }

    /// The runtime id for a block name, if this registry has it.
    pub fn id_of(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    /// The block name for a runtime id, if this registry has it -- the
    /// inverse of [`id_of`](Self::id_of). `name_of(BlockId::AIR)` is always
    /// `"cubara:air"`. What save/load (block 1.9) uses to build a world's id
    /// table (`level.ron`'s `blocks` field, §7.2): the names in force when
    /// the world was saved, so a later load can remap them to whatever ids
    /// the registry assigns them next time, even if that's changed.
    pub fn name_of(&self, id: BlockId) -> Option<&str> {
        self.entries.get(id.0 as usize).map(|e| e.name.as_str())
    }

    /// Every `BlockId` this registry assigned, air included, in ascending order.
    pub fn ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        (0..self.entries.len() as u16).map(BlockId)
    }

    /// Every distinct texture name any material references, in a stable
    /// (sorted) order. `cubara-render` builds its texture array from this
    /// list, with a texture's array layer being its position in it.
    pub fn texture_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .entries
            .iter()
            .filter_map(|e| e.faces.as_ref())
            .flat_map(Faces::texture_names)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The texture name `id` shows on `face` -- an `All` material gives the
    /// same name for every face; a `Sided` material gives its top/side/bottom
    /// texture depending on which of the six directions `face` is (§3.2).
    /// `None` for air (nothing to texture) or an id this registry doesn't
    /// have.
    pub fn texture_for_face(&self, id: BlockId, face: Face) -> Option<&str> {
        let faces = self.entries.get(id.0 as usize)?.faces.as_ref()?;
        Some(faces.texture_for(face))
    }

    /// Check every material's referenced texture names against real files in
    /// `textures_dir` (matched by file stem, extension-agnostic) — a
    /// filesystem existence check only, no image decoding (`ARCHITECTURE.md`
    /// Rule 4: this stays GPU-free). **Not** run by [`load`](Self::load)
    /// itself: texture assets are block 1.4's scope, so nothing calls this
    /// yet against the real `assets/textures/` directory. This is the
    /// validation machinery block 1.3 owns, ready for 1.4 to call for real.
    pub fn validate_textures(&self, textures_dir: &Path) -> Result<(), RegistryError> {
        let available: std::collections::HashSet<String> = std::fs::read_dir(textures_dir)
            .map_err(|error| RegistryError::Io {
                file: textures_dir.to_path_buf(),
                error,
            })?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .collect();

        for entry in &self.entries {
            let Some(faces) = &entry.faces else {
                continue; // air: no faces, nothing to check
            };
            for texture in faces.texture_names() {
                if !available.contains(texture) {
                    return Err(RegistryError::UnknownTexture {
                        name: entry.name.clone(),
                        texture: texture.to_string(),
                        file: entry.file.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stone() -> (PathBuf, Material) {
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
            },
        )
    }

    fn grass() -> (PathBuf, Material) {
        (
            PathBuf::from("grass.ron"),
            Material {
                name: "cubara:grass".to_string(),
                solid: true,
                faces: Faces::Sided {
                    top: "grass_top".to_string(),
                    side: "grass_side".to_string(),
                    bottom: "soil".to_string(),
                },
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        )
    }

    fn soil() -> (PathBuf, Material) {
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
            },
        )
    }

    #[test]
    fn air_is_always_id_zero() {
        let registry = BlockRegistry::from_materials(vec![stone()]).unwrap();
        assert_eq!(registry.id_of("cubara:air"), Some(BlockId::AIR));
        assert!(!registry.is_solid(BlockId::AIR));
    }

    #[test]
    fn name_of_is_the_inverse_of_id_of() {
        let registry = BlockRegistry::from_materials(vec![stone(), grass(), soil()]).unwrap();
        for name in ["cubara:air", "cubara:stone", "cubara:grass", "cubara:soil"] {
            let id = registry.id_of(name).unwrap();
            assert_eq!(registry.name_of(id), Some(name));
        }
        assert_eq!(
            registry.name_of(BlockId(999)),
            None,
            "an id this registry never assigned"
        );
    }

    #[test]
    fn id_assignment_is_stable_across_shuffled_read_order() {
        let forward = BlockRegistry::from_materials(vec![stone(), grass(), soil()]).unwrap();
        let shuffled = BlockRegistry::from_materials(vec![soil(), stone(), grass()]).unwrap();
        let reversed = BlockRegistry::from_materials(vec![grass(), soil(), stone()]).unwrap();

        for name in ["cubara:air", "cubara:stone", "cubara:grass", "cubara:soil"] {
            let a = forward.id_of(name);
            assert_eq!(
                a,
                shuffled.id_of(name),
                "{name} moved under a shuffled order"
            );
            assert_eq!(
                a,
                reversed.id_of(name),
                "{name} moved under a reversed order"
            );
        }
    }

    #[test]
    fn ids_sort_lexicographically_by_expanded_name() {
        // "cubara:grass" < "cubara:soil" < "cubara:stone" lexicographically;
        // air is always 0 regardless.
        let registry = BlockRegistry::from_materials(vec![stone(), grass(), soil()]).unwrap();
        let grass_id = registry.id_of("cubara:grass").unwrap().0;
        let soil_id = registry.id_of("cubara:soil").unwrap().0;
        let stone_id = registry.id_of("cubara:stone").unwrap().0;
        assert_eq!(registry.id_of("cubara:air"), Some(BlockId::AIR));
        assert!(grass_id < soil_id);
        assert!(soil_id < stone_id);
    }

    #[test]
    fn solid_flag_round_trips_through_the_registry() {
        let registry = BlockRegistry::from_materials(vec![stone()]).unwrap();
        let id = registry.id_of("cubara:stone").unwrap();
        assert!(registry.is_solid(id));
    }

    #[test]
    fn a_fourth_material_is_a_pure_data_change() {
        // Loading four files instead of three needs no recompile -- this test
        // proves it by loading a real fixture directory from disk rather than
        // in-memory `Material` values.
        let dir = std::env::temp_dir().join(format!(
            "cubara-registry-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, ron) in [
            (
                "stone.ron",
                r#"Material(name: "cubara:stone", solid: true, faces: All("stone"), shapes: [Full])"#,
            ),
            (
                "soil.ron",
                r#"Material(name: "cubara:soil", solid: true, faces: All("soil"), shapes: [Full])"#,
            ),
            (
                "grass.ron",
                r#"Material(name: "cubara:grass", solid: true, faces: Sided(top: "grass_top", side: "grass_side", bottom: "soil"), shapes: [Full])"#,
            ),
            (
                "glass.ron",
                r#"Material(name: "cubara:glass", solid: false, faces: All("glass"), shapes: [Full])"#,
            ),
        ] {
            std::fs::write(dir.join(name), ron).unwrap();
        }

        let registry = BlockRegistry::load(&dir).expect("fixture directory loads");
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            registry.id_of("cubara:glass").is_some(),
            "the fourth material is present"
        );
        assert!(!registry.is_solid(registry.id_of("cubara:glass").unwrap()));
        for name in ["cubara:air", "cubara:stone", "cubara:soil", "cubara:grass"] {
            assert!(registry.id_of(name).is_some(), "{name} still present");
        }
    }

    #[test]
    fn duplicate_name_is_a_named_error() {
        let a = (
            PathBuf::from("a.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        );
        let b = (
            PathBuf::from("b.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        );
        let err = BlockRegistry::from_materials(vec![a, b]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cubara:stone"), "{msg}");
        assert!(msg.contains("a.ron"), "{msg}");
        assert!(msg.contains("b.ron"), "{msg}");
    }

    #[test]
    fn a_material_without_hardness_is_unbreakable() {
        // §4.3 chose "absent means unbreakable" over "absent means instant" so
        // that a block file which forgets to declare hardness is conspicuously
        // unminable rather than silently free. No shipped block relies on it --
        // which is exactly why it is pinned here rather than in a game test.
        let r = BlockRegistry::from_materials(vec![(
            PathBuf::from("bedrock.ron"),
            Material {
                name: "cubara:bedrock".into(),
                solid: true,
                faces: Faces::All("stone".into()),
                shapes: vec![Shape::Full],
                drops: DropRule::Nothing,
                requires_tier: 0,
                hardness: None,
            },
        )])
        .expect("valid");
        let id = r.id_of("cubara:bedrock").expect("registered");
        assert_eq!(r.hardness(id), None);
        assert_eq!(r.hardness(BlockId::AIR), None, "air too");
    }

    #[test]
    fn empty_shapes_is_a_named_error() {
        let material = (
            PathBuf::from("empty.ron"),
            Material {
                name: "cubara:nothing".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        );
        let err = BlockRegistry::from_materials(vec![material]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cubara:nothing"), "{msg}");
        assert!(msg.contains("empty.ron"), "{msg}");
    }

    #[test]
    fn unknown_texture_is_a_named_error() {
        let dir = std::env::temp_dir().join(format!(
            "cubara-textures-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stone.png"), b"not a real png, existence only").unwrap();

        let registry = BlockRegistry::from_materials(vec![grass()]).unwrap();
        let err = registry.validate_textures(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();

        let msg = err.to_string();
        assert!(msg.contains("cubara:grass"), "{msg}");
        assert!(
            msg.contains("grass_top") || msg.contains("grass_side"),
            "{msg}"
        );
        assert!(msg.contains("grass.ron"), "{msg}");
    }

    #[test]
    fn textures_all_present_validates_cleanly() {
        let dir = std::env::temp_dir().join(format!(
            "cubara-textures-ok-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stone.png"), b"placeholder").unwrap();

        let registry = BlockRegistry::from_materials(vec![stone()]).unwrap();
        let result = registry.validate_textures(&dir);
        std::fs::remove_dir_all(&dir).ok();

        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn the_real_assets_blocks_directory_loads_and_defines_the_three_phase_1_materials() {
        // Guards against a hand-edited assets/blocks/*.ron breaking silently --
        // this is the directory the app actually loads at startup.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks");
        let registry = BlockRegistry::load(&dir).expect("assets/blocks must parse and validate");

        for name in ["cubara:air", "cubara:stone", "cubara:soil", "cubara:grass"] {
            assert!(
                registry.id_of(name).is_some(),
                "{name} missing from the registry"
            );
        }
        assert!(!registry.is_solid(BlockId::AIR));
        for name in ["cubara:stone", "cubara:soil", "cubara:grass"] {
            let id = registry.id_of(name).unwrap();
            assert!(registry.is_solid(id), "{name} should be solid");
        }
    }

    #[test]
    fn ids_covers_every_entry_in_ascending_order() {
        let registry = BlockRegistry::from_materials(vec![stone(), grass(), soil()]).unwrap();
        let ids: Vec<u16> = registry.ids().map(|id| id.0).collect();
        assert_eq!(
            ids,
            vec![0, 1, 2, 3],
            "air plus the three materials, in order"
        );
    }

    #[test]
    fn texture_names_are_sorted_and_deduplicated() {
        // grass's bottom ("soil") repeats soil's own texture name -- must
        // appear once, not twice.
        let registry = BlockRegistry::from_materials(vec![stone(), grass(), soil()]).unwrap();
        assert_eq!(
            registry.texture_names(),
            vec!["grass_side", "grass_top", "soil", "stone"]
        );
    }

    #[test]
    fn texture_for_face_resolves_all_and_sided_materials() {
        let registry = BlockRegistry::from_materials(vec![stone(), grass()]).unwrap();
        assert_eq!(registry.texture_for_face(BlockId::AIR, Face::PosY), None);

        let stone_id = registry.id_of("cubara:stone").unwrap();
        for face in [Face::PosY, Face::NegY, Face::PosX, Face::NegZ] {
            assert_eq!(
                registry.texture_for_face(stone_id, face),
                Some("stone"),
                "an All material uses the same texture on every face"
            );
        }

        let grass_id = registry.id_of("cubara:grass").unwrap();
        assert_eq!(
            registry.texture_for_face(grass_id, Face::PosY),
            Some("grass_top")
        );
        assert_eq!(
            registry.texture_for_face(grass_id, Face::NegY),
            Some("soil")
        );
        for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert_eq!(
                registry.texture_for_face(grass_id, face),
                Some("grass_side"),
                "all four horizontal directions use the side texture"
            );
        }
    }
}
