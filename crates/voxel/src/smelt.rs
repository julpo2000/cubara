//! Smelting recipes — one input, one output, a duration.
//!
//! A separate book from [`crate::recipe`]'s shaped grid recipes, and a separate
//! `assets/smelting/` directory, because the *matcher* is a different shape
//! rather than a special case of the same one: there is no pattern, no width,
//! and no position. Folding a one-input recipe into the grid matcher would mean
//! a 1×1 pattern that happens to mean something else, which is the kind of
//! near-miss that reads as a bug later.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::item::{ItemId, ItemRegistry};
use crate::recipe::RecipeOutputDef;

/// One smelting recipe, as authored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "SmeltRecipe")]
pub struct SmeltRecipeDef {
    pub name: String,
    /// What goes in the input slot, by name.
    pub input: String,
    pub output: RecipeOutputDef,
    /// How many ticks one item takes to smelt.
    pub ticks: u32,
}

/// A smelting recipe, resolved to ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmeltRecipe {
    pub input: ItemId,
    pub output: ItemId,
    pub count: u8,
    pub ticks: u32,
}

#[derive(Debug)]
pub enum SmeltError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    /// An input or output naming an item that does not exist. Caught at load
    /// rather than at smelt time, where it would be an inert furnace and no
    /// message at all.
    UnknownItem {
        recipe: String,
        item: String,
        file: PathBuf,
    },
    /// Two recipes consuming the same input: which one wins would depend on
    /// file order, and file order is platform-defined.
    DuplicateInput {
        input: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// `ticks: 0` would smelt an entire stack in one tick, which is a typo
    /// every time.
    ZeroTicks { recipe: String, file: PathBuf },
}

impl fmt::Display for SmeltError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmeltError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            SmeltError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            SmeltError::UnknownItem { recipe, item, file } => write!(
                f,
                "{}: smelting recipe {recipe:?} names unknown item {item:?}",
                file.display()
            ),
            SmeltError::DuplicateInput {
                input,
                first,
                second,
            } => write!(
                f,
                "two smelting recipes both consume {input:?}: {} and {}",
                first.display(),
                second.display()
            ),
            SmeltError::ZeroTicks { recipe, file } => write!(
                f,
                "{}: smelting recipe {recipe:?} has ticks 0, which would smelt \
                 instantly",
                file.display()
            ),
        }
    }
}

impl std::error::Error for SmeltError {}

/// Every smelting recipe, indexed by what it consumes.
///
/// Keyed by input because that is the only question a furnace ever asks: *given
/// what is in the input slot, is there anything to make?*
#[derive(Debug, Default)]
pub struct SmeltBook {
    by_input: HashMap<ItemId, SmeltRecipe>,
}

impl SmeltBook {
    pub fn load(dir: &Path, items: &ItemRegistry) -> Result<Self, SmeltError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| SmeltError::Io {
            file: dir.to_path_buf(),
            error,
        })?;
        let mut defs = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| SmeltError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| SmeltError::Io {
                file: path.clone(),
                error,
            })?;
            let def: SmeltRecipeDef = ron::from_str(&text).map_err(|error| SmeltError::Parse {
                file: path.clone(),
                error,
            })?;
            defs.push((path, def));
        }
        Self::from_defs(defs, items)
    }

    pub fn from_defs(
        defs: Vec<(PathBuf, SmeltRecipeDef)>,
        items: &ItemRegistry,
    ) -> Result<Self, SmeltError> {
        let mut by_input: HashMap<ItemId, SmeltRecipe> = HashMap::new();
        let mut files: HashMap<ItemId, PathBuf> = HashMap::new();

        for (file, def) in defs {
            if def.ticks == 0 {
                return Err(SmeltError::ZeroTicks {
                    recipe: def.name,
                    file,
                });
            }
            let resolve = |name: &str| -> Result<ItemId, SmeltError> {
                items.id_of(name).ok_or_else(|| SmeltError::UnknownItem {
                    recipe: def.name.clone(),
                    item: name.to_string(),
                    file: file.clone(),
                })
            };
            let input = resolve(&def.input)?;
            let output = resolve(&def.output.item)?;
            if let Some(first) = files.get(&input) {
                return Err(SmeltError::DuplicateInput {
                    input: def.input,
                    first: first.clone(),
                    second: file,
                });
            }
            files.insert(input, file);
            by_input.insert(
                input,
                SmeltRecipe {
                    input,
                    output,
                    count: def.output.count,
                    ticks: def.ticks,
                },
            );
        }
        Ok(Self { by_input })
    }

    /// The recipe consuming `input`, if any.
    pub fn for_input(&self, input: ItemId) -> Option<SmeltRecipe> {
        self.by_input.get(&input).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.by_input.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemDef;

    fn items() -> ItemRegistry {
        let def = |name: &str| {
            (
                PathBuf::from(format!("{name}.ron")),
                ItemDef {
                    name: name.to_string(),
                    max_stack: 64,
                    durability: None,
                    tier: 0,
                    speed: None,
                    burn_ticks: None,
                },
            )
        };
        ItemRegistry::from_defs(vec![
            def("cubara:raw_iron"),
            def("cubara:iron_ingot"),
            def("cubara:oak_log"),
        ])
        .expect("valid items")
    }

    fn def(name: &str, input: &str, output: &str, ticks: u32) -> (PathBuf, SmeltRecipeDef) {
        (
            PathBuf::from(format!("{name}.ron")),
            SmeltRecipeDef {
                name: name.to_string(),
                input: input.to_string(),
                output: RecipeOutputDef {
                    item: output.to_string(),
                    count: 1,
                },
                ticks,
            },
        )
    }

    #[test]
    fn a_recipe_is_found_by_its_input() {
        let items = items();
        let book = SmeltBook::from_defs(
            vec![def(
                "cubara:iron_ingot",
                "cubara:raw_iron",
                "cubara:iron_ingot",
                200,
            )],
            &items,
        )
        .expect("valid");
        let r = book
            .for_input(items.id_of("cubara:raw_iron").unwrap())
            .expect("found");
        assert_eq!(r.output, items.id_of("cubara:iron_ingot").unwrap());
        assert_eq!(r.ticks, 200);
        assert!(book
            .for_input(items.id_of("cubara:oak_log").unwrap())
            .is_none());
    }

    #[test]
    fn an_unknown_item_is_a_named_error() {
        let items = items();
        assert!(matches!(
            SmeltBook::from_defs(
                vec![def("cubara:gold", "cubara:gold_ore", "cubara:gold", 200)],
                &items
            ),
            Err(SmeltError::UnknownItem { .. })
        ));
    }

    #[test]
    fn zero_ticks_is_a_named_error() {
        let items = items();
        assert!(matches!(
            SmeltBook::from_defs(
                vec![def(
                    "cubara:iron_ingot",
                    "cubara:raw_iron",
                    "cubara:iron_ingot",
                    0
                )],
                &items
            ),
            Err(SmeltError::ZeroTicks { .. })
        ));
    }

    #[test]
    fn two_recipes_for_the_same_input_are_rejected() {
        // Which one wins would otherwise depend on directory iteration order.
        let items = items();
        assert!(matches!(
            SmeltBook::from_defs(
                vec![
                    def("a", "cubara:raw_iron", "cubara:iron_ingot", 200),
                    def("b", "cubara:raw_iron", "cubara:oak_log", 200),
                ],
                &items
            ),
            Err(SmeltError::DuplicateInput { .. })
        ));
    }

    #[test]
    fn the_shipped_smelting_recipes_close_the_ladder() {
        // Loads the real assets/: raw iron -- what 2.4a's drop table gives you
        // for mining iron ore -- must smelt into the ingot the iron pick recipe
        // asks for. That is the last rung of REQUIREMENTS #5.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let items = ItemRegistry::load(&root.join("assets/items")).expect("assets/items");
        let book = SmeltBook::load(&root.join("assets/smelting"), &items).expect("assets/smelting");
        let raw = items.id_of("cubara:raw_iron").expect("raw iron exists");
        let r = book.for_input(raw).expect("raw iron smelts");
        assert_eq!(items.name_of(r.output), Some("cubara:iron_ingot"));
    }
}
