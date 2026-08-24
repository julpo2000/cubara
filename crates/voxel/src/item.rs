//! Item identity, stacks, and the item registry.
//!
//! The deliberate mirror of [`crate::block`] and [`crate::registry`]: an
//! [`ItemId`] is a *runtime* index, definitions are authored as RON under
//! `assets/items/`, and ids come from sorting the names rather than from file
//! read order — **names are identity, numbers are per-world**
//! (`docs/PHASE2_ARCHITECTURE.md` §1.2, the same decision block 1.3 / #54 made
//! for blocks, for the same reason: a save that stores numeric ids breaks the
//! moment a data file is added or reordered, and mods make that certain).
//!
//! The one thing that is *not* a mirror is [`ItemStack`]. A block is fully
//! described by its id; an item is not, because tools wear out
//! (`PHASE2_ARCHITECTURE.md` decision C). So a stack carries [`ItemState`],
//! and the invariant below is what keeps that from costing anything on the
//! common path.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A runtime item-type index. `NONE` (0) is the absence of an item, pinned
/// regardless of sort order — exactly as `BlockId::AIR` is.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ItemId(pub u16);

impl ItemId {
    /// Always "no item". Not authored in `assets/items/`.
    pub const NONE: ItemId = ItemId(0);
}

/// Per-item state: what one *individual* item carries that its id does not say.
///
/// `None` for everything stackable. A tool carries its own remaining wear, so
/// two otherwise identical tools are not interchangeable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ItemState {
    None,
    Durability { remaining: u16 },
}

/// Why an [`ItemStack`] could not be built.
#[derive(Debug, PartialEq, Eq)]
pub enum StackError {
    /// The invariant: state implies a stack of one.
    StatefulStackTooLarge { count: u8 },
    /// A stack of nothing is not a stack; an empty slot is `Option::None`.
    ZeroCount,
    /// More items than this kind allows in one slot.
    OverMaxStack { count: u8, max_stack: u8 },
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StackError::StatefulStackTooLarge { count } => write!(
                f,
                "an item carrying its own state cannot stack: count {count}, must be 1"
            ),
            StackError::ZeroCount => {
                write!(f, "a stack of zero is not a stack; use Option::None")
            }
            StackError::OverMaxStack { count, max_stack } => {
                write!(
                    f,
                    "count {count} exceeds this item's max_stack of {max_stack}"
                )
            }
        }
    }
}

impl std::error::Error for StackError {}

/// One slot's worth of items.
///
/// **The invariant, enforced here and pinned by
/// `a_stateful_stack_of_more_than_one_is_rejected`:** a stack whose `state` is
/// not [`ItemState::None`] always has `count == 1`. Two half-worn tools are not
/// interchangeable, so they cannot share a slot.
///
/// That invariant is also what keeps per-item state cheap. Because state
/// implies a count of one, everything stackable carries `ItemState::None` and
/// behaves exactly as a plain `(id, count)` pair would — only tools pay for the
/// generality. Fields are private so the invariant cannot be sidestepped by
/// building the struct literally.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ItemStack {
    item: ItemId,
    count: u8,
    state: ItemState,
}

impl ItemStack {
    /// Build a stack, rejecting the combinations the invariant forbids.
    /// `max_stack` comes from the registry; [`ItemRegistry::new_stack`] is the
    /// usual way in, and passes it for you.
    pub fn new(
        item: ItemId,
        count: u8,
        state: ItemState,
        max_stack: u8,
    ) -> Result<Self, StackError> {
        if count == 0 {
            return Err(StackError::ZeroCount);
        }
        if state != ItemState::None && count != 1 {
            return Err(StackError::StatefulStackTooLarge { count });
        }
        if count > max_stack {
            return Err(StackError::OverMaxStack { count, max_stack });
        }
        Ok(Self { item, count, state })
    }

    pub fn item(self) -> ItemId {
        self.item
    }

    pub fn count(self) -> u8 {
        self.count
    }

    pub fn state(self) -> ItemState {
        self.state
    }

    /// Whether two stacks may merge: same item, and both stateless. A stack
    /// carrying state never merges — which falls out of the invariant rather
    /// than being a special case, since it could only ever hold one anyway.
    pub fn mergeable_with(self, other: ItemStack) -> bool {
        self.item == other.item && self.state == ItemState::None && other.state == ItemState::None
    }
}

/// One item definition, as authored in `assets/items/*.ron`.
///
/// ```ron
/// (
///     name: "cubara:oak_log",
///     max_stack: 64,
///     durability: None,
/// )
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct ItemDef {
    pub name: String,
    pub max_stack: u8,
    /// `Some(n)`: this kind is created with `ItemState::Durability { remaining: n }`
    /// and must declare `max_stack: 1`. `None`: created stateless.
    pub durability: Option<u16>,
}

#[derive(Debug)]
pub enum ItemRegistryError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    /// Two files declare the same item name.
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// An item declares durability *and* a stack size above one, which the
    /// [`ItemStack`] invariant makes unbuildable — so it is caught at load
    /// time, naming the file, rather than at the first attempt to make one.
    StackableTool {
        name: String,
        max_stack: u8,
        file: PathBuf,
    },
    /// `max_stack: 0` would make every stack of this item unbuildable.
    ZeroMaxStack { name: String, file: PathBuf },
}

impl fmt::Display for ItemRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemRegistryError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            ItemRegistryError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            ItemRegistryError::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate item {name:?}: {} and {}",
                first.display(),
                second.display()
            ),
            ItemRegistryError::StackableTool {
                name,
                max_stack,
                file,
            } => write!(
                f,
                "{}: item {name:?} declares durability but max_stack {max_stack}; \
                 an item carrying its own state must declare max_stack: 1",
                file.display()
            ),
            ItemRegistryError::ZeroMaxStack { name, file } => write!(
                f,
                "{}: item {name:?} declares max_stack: 0, so no stack of it could exist",
                file.display()
            ),
        }
    }
}

impl std::error::Error for ItemRegistryError {}

#[derive(Debug)]
struct Entry {
    name: String,
    max_stack: u8,
    durability: Option<u16>,
}

/// Which items exist, and the runtime ids assigned to them.
#[derive(Debug)]
pub struct ItemRegistry {
    entries: Vec<Entry>,
    by_name: HashMap<String, ItemId>,
}

impl ItemRegistry {
    /// Build from every `*.ron` directly inside `dir`, one [`ItemDef`] per
    /// file. File read order never affects the result.
    pub fn load(dir: &Path) -> Result<Self, ItemRegistryError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| ItemRegistryError::Io {
            file: dir.to_path_buf(),
            error,
        })?;

        let mut defs = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| ItemRegistryError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| ItemRegistryError::Io {
                file: path.clone(),
                error,
            })?;
            let def: ItemDef = ron::from_str(&text).map_err(|error| ItemRegistryError::Parse {
                file: path.clone(),
                error,
            })?;
            defs.push((path, def));
        }
        Self::from_defs(defs)
    }

    /// Build from definitions already in memory, each paired with the file to
    /// blame in a validation error. [`load`](Self::load) reduces to this after
    /// reading the directory; tests use it directly, so validation is testable
    /// with no filesystem involved.
    pub fn from_defs(defs: Vec<(PathBuf, ItemDef)>) -> Result<Self, ItemRegistryError> {
        let mut seen: HashMap<String, PathBuf> = HashMap::new();
        let mut sorted: Vec<(PathBuf, ItemDef)> = Vec::with_capacity(defs.len());

        for (file, def) in defs {
            if def.max_stack == 0 {
                return Err(ItemRegistryError::ZeroMaxStack {
                    name: def.name,
                    file,
                });
            }
            if def.durability.is_some() && def.max_stack != 1 {
                return Err(ItemRegistryError::StackableTool {
                    name: def.name,
                    max_stack: def.max_stack,
                    file,
                });
            }
            if let Some(first) = seen.get(&def.name) {
                return Err(ItemRegistryError::DuplicateName {
                    name: def.name,
                    first: first.clone(),
                    second: file,
                });
            }
            seen.insert(def.name.clone(), file.clone());
            sorted.push((file, def));
        }

        // The whole point: ids come from the sorted names, not arrival order,
        // since directory iteration order is platform-defined.
        sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));

        let mut entries = vec![Entry {
            name: "cubara:none".to_string(),
            max_stack: 1,
            durability: None,
        }];
        let mut by_name = HashMap::new();
        by_name.insert("cubara:none".to_string(), ItemId::NONE);

        for (_, def) in sorted {
            let id = ItemId(entries.len() as u16);
            by_name.insert(def.name.clone(), id);
            entries.push(Entry {
                name: def.name,
                max_stack: def.max_stack,
                durability: def.durability,
            });
        }

        Ok(Self { entries, by_name })
    }

    pub fn id_of(&self, name: &str) -> Option<ItemId> {
        self.by_name.get(name).copied()
    }

    pub fn name_of(&self, id: ItemId) -> Option<&str> {
        self.entries.get(id.0 as usize).map(|e| e.name.as_str())
    }

    /// How many of `id` fit in one slot. Unknown ids report 1 rather than
    /// panicking — a mismatched registry is a loud bug elsewhere, not
    /// something an inventory operation should crash over. Same stance
    /// `BlockRegistry::is_solid` takes.
    pub fn max_stack(&self, id: ItemId) -> u8 {
        self.entries
            .get(id.0 as usize)
            .map(|e| e.max_stack)
            .unwrap_or(1)
    }

    /// The wear a fresh one of `id` starts with, if it is a tool.
    pub fn durability(&self, id: ItemId) -> Option<u16> {
        self.entries.get(id.0 as usize).and_then(|e| e.durability)
    }

    pub fn ids(&self) -> impl Iterator<Item = ItemId> + '_ {
        (0..self.entries.len()).map(|i| ItemId(i as u16))
    }

    /// A fresh stack of `count` of `id`, with the state this item kind implies:
    /// full durability for a tool, [`ItemState::None`] otherwise. The usual way
    /// to build a stack, since it reads `max_stack` and `durability` for you.
    pub fn new_stack(&self, id: ItemId, count: u8) -> Result<ItemStack, StackError> {
        let state = match self.durability(id) {
            Some(remaining) => ItemState::Durability { remaining },
            None => ItemState::None,
        };
        ItemStack::new(id, count, state, self.max_stack(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, max_stack: u8, durability: Option<u16>) -> (PathBuf, ItemDef) {
        (
            PathBuf::from(format!("{name}.ron")),
            ItemDef {
                name: name.to_string(),
                max_stack,
                durability,
            },
        )
    }

    fn registry() -> ItemRegistry {
        ItemRegistry::from_defs(vec![
            def("cubara:plank", 64, None),
            def("cubara:oak_log", 64, None),
            def("cubara:wooden_pick", 1, Some(60)),
        ])
        .expect("fixture registry is valid")
    }

    #[test]
    fn a_stackable_stack_holds_many() {
        let r = registry();
        let log = r.id_of("cubara:oak_log").unwrap();
        let stack = r.new_stack(log, 32).expect("32 logs is a valid stack");
        assert_eq!(stack.count(), 32);
        assert_eq!(stack.state(), ItemState::None);
    }

    #[test]
    fn a_stateful_stack_of_more_than_one_is_rejected() {
        // The invariant. Two tools with different remaining wear are not
        // interchangeable, so they cannot share a slot -- and the type must
        // not let that be expressed at all, rather than relying on every
        // caller to remember.
        let r = registry();
        let pick = r.id_of("cubara:wooden_pick").unwrap();
        assert_eq!(
            ItemStack::new(pick, 2, ItemState::Durability { remaining: 60 }, 64),
            Err(StackError::StatefulStackTooLarge { count: 2 })
        );
        // And one of them is fine.
        assert!(r.new_stack(pick, 1).is_ok());
    }

    #[test]
    fn a_fresh_tool_carries_full_durability() {
        let r = registry();
        let pick = r.id_of("cubara:wooden_pick").unwrap();
        let stack = r.new_stack(pick, 1).unwrap();
        assert_eq!(stack.state(), ItemState::Durability { remaining: 60 });
        assert_eq!(stack.count(), 1);
    }

    #[test]
    fn zero_and_overfull_stacks_are_rejected() {
        let r = registry();
        let log = r.id_of("cubara:oak_log").unwrap();
        assert_eq!(r.new_stack(log, 0), Err(StackError::ZeroCount));
        assert_eq!(
            r.new_stack(log, 65),
            Err(StackError::OverMaxStack {
                count: 65,
                max_stack: 64
            })
        );
    }

    #[test]
    fn tools_never_merge_but_stackables_do() {
        let r = registry();
        let log = r.id_of("cubara:oak_log").unwrap();
        let pick = r.id_of("cubara:wooden_pick").unwrap();
        let a = r.new_stack(log, 1).unwrap();
        let b = r.new_stack(log, 1).unwrap();
        assert!(a.mergeable_with(b));

        let t1 = r.new_stack(pick, 1).unwrap();
        let t2 = r.new_stack(pick, 1).unwrap();
        assert!(
            !t1.mergeable_with(t2),
            "two tools must not merge even at identical wear -- they will diverge"
        );
    }

    #[test]
    fn ids_come_from_sorted_names_not_file_order() {
        // Directory iteration order is platform-defined, so the same files
        // must produce the same ids however they arrive.
        let forward = ItemRegistry::from_defs(vec![
            def("cubara:apple", 64, None),
            def("cubara:beam", 64, None),
            def("cubara:cog", 64, None),
        ])
        .unwrap();
        let reversed = ItemRegistry::from_defs(vec![
            def("cubara:cog", 64, None),
            def("cubara:beam", 64, None),
            def("cubara:apple", 64, None),
        ])
        .unwrap();
        for name in ["cubara:apple", "cubara:beam", "cubara:cog"] {
            assert_eq!(
                forward.id_of(name),
                reversed.id_of(name),
                "{name} moved with file order"
            );
        }
    }

    #[test]
    fn names_are_identity_and_numbers_are_per_world() {
        // The other half of the same rule: adding a data file *does* shift the
        // numbers, which is exactly why a save must store names. If this ever
        // stops being true, someone has made ids global and the save format's
        // id table has quietly become dead weight.
        let small = ItemRegistry::from_defs(vec![
            def("cubara:beam", 64, None),
            def("cubara:cog", 64, None),
        ])
        .unwrap();
        let with_new_file = ItemRegistry::from_defs(vec![
            def("cubara:apple", 64, None),
            def("cubara:beam", 64, None),
            def("cubara:cog", 64, None),
        ])
        .unwrap();
        assert_ne!(
            small.id_of("cubara:beam"),
            with_new_file.id_of("cubara:beam"),
            "adding an earlier-sorting item must shift later ids -- that is why \
             names, not numbers, are identity"
        );
    }

    #[test]
    fn a_tool_that_claims_to_stack_is_rejected_at_load() {
        // Caught when the data is read, naming the file, rather than at the
        // first attempt to build one of them somewhere far away.
        let err = ItemRegistry::from_defs(vec![def("cubara:bad_pick", 64, Some(60))])
            .expect_err("a stackable tool must not load");
        match err {
            ItemRegistryError::StackableTool {
                name, max_stack, ..
            } => {
                assert_eq!(name, "cubara:bad_pick");
                assert_eq!(max_stack, 64);
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn duplicate_names_and_zero_stacks_are_rejected() {
        assert!(matches!(
            ItemRegistry::from_defs(vec![
                def("cubara:plank", 64, None),
                def("cubara:plank", 64, None),
            ]),
            Err(ItemRegistryError::DuplicateName { .. })
        ));
        assert!(matches!(
            ItemRegistry::from_defs(vec![def("cubara:void", 0, None)]),
            Err(ItemRegistryError::ZeroMaxStack { .. })
        ));
    }

    #[test]
    fn the_shipped_item_files_load_and_cover_the_ladder() {
        // Loads the real `assets/items/` the game ships, not a fixture -- a
        // data file that parses in a test but not in the app is the failure
        // this catches. The named items are the ladder to iron
        // (`docs/PHASE2_ARCHITECTURE.md` decision D); if one disappears, the
        // recipes that 2.2 will reference lose their target.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/items");
        let r = ItemRegistry::load(&dir).expect("assets/items must load");

        for name in [
            "cubara:oak_log",
            "cubara:plank",
            "cubara:stick",
            "cubara:cobble",
            "cubara:raw_iron",
            "cubara:iron_ingot",
            "cubara:wooden_pick",
            "cubara:stone_pick",
            "cubara:iron_pick",
        ] {
            assert!(r.id_of(name).is_some(), "{name} missing from assets/items");
        }

        // Every shipped tool is single-stack and starts with real wear -- the
        // registry already rejects the alternative, so this asserts the *data*
        // says what decision C intends, not that the check exists.
        for tool in [
            "cubara:wooden_pick",
            "cubara:stone_pick",
            "cubara:iron_pick",
        ] {
            let id = r.id_of(tool).unwrap();
            assert_eq!(r.max_stack(id), 1, "{tool} must not stack");
            assert!(
                r.durability(id).is_some_and(|d| d > 0),
                "{tool} must declare durability"
            );
        }
    }

    #[test]
    fn none_is_always_zero() {
        let r = registry();
        assert_eq!(r.id_of("cubara:none"), Some(ItemId::NONE));
        assert_eq!(ItemId::NONE.0, 0);
    }
}
