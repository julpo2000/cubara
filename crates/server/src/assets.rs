//! Loading the definitions the world is made of.
//!
//! # Why these moved off the client
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §3.4: the server decides what a block *means*
//! — what it drops, what tier it needs, how long it smelts. A client needs the
//! same definitions to draw and to predict, and is given them; it does not get
//! to disagree about them. Loaders living on `Game` made the client the one that
//! read the rules off disk and handed them to the authority, which is backwards.
//!
//! It is also what a dedicated server needs to be able to *start*: a headless
//! host has no window, so nothing was going to call `Game::set_assets`.
//!
//! # Blocks without textures
//!
//! [`load_block_registry`] is deliberately **not** `cubara_render::load_registry`.
//! That one additionally validates that every material's faces have textures on
//! disk — a real check, and a render concern (Rule 3). A server does not draw,
//! so it does not care whether `stone_top.png` exists, and requiring it would
//! make the texture folder a dependency of running a world.
//!
//! Both call the same [`BlockRegistry::load`], so they cannot disagree about
//! what a block *is*; they disagree only about whether it can be seen.

use cubara_voxel::{
    BlockRegistry, ItemRegistry, OreRegistry, RecipeBook, SmeltBook, StructureRegistry,
};
use std::path::{Path, PathBuf};

/// The repository root, found relative to this crate rather than to the
/// process's working directory.
///
/// `CARGO_MANIFEST_DIR` is `crates/server`, so `../..` reaches the root no
/// matter where the binary is invoked from — the same trick `load_registry`
/// uses from `crates/render`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where the world lives on disk.
///
/// One world, in a fixed place next to the executable's project root. Named
/// worlds and a world-picker are a gameplay/UI decision nobody has made
/// (#179), and inventing one would be inventing a menu; this is the smallest
/// thing that makes "still there after you close it" true.
///
/// The dedicated server takes a `--world <dir>` instead of being stuck with
/// this, because two servers on one host must not share a save.
pub fn world_dir() -> PathBuf {
    repo_root().join("saves/world")
}

/// Load `assets/blocks/*.ron` — what blocks exist, what they drop, what breaks
/// them. **No texture validation**; see the module docs.
pub fn load_block_registry() -> BlockRegistry {
    BlockRegistry::load(&repo_root().join("assets/blocks")).expect("assets/blocks must load")
}

/// Load `assets/items/*.ron`.
pub fn load_item_registry() -> ItemRegistry {
    ItemRegistry::load(&repo_root().join("assets/items")).expect("assets/items must load")
}

/// Load `assets/structures/*.ron` — the shapes worldgen grows.
pub fn load_structure_registry() -> StructureRegistry {
    StructureRegistry::load(&repo_root().join("assets/structures"))
        .expect("assets/structures must load")
}

/// Load `assets/ores/*.ron` — which ores exist, and how common they are.
pub fn load_ore_registry() -> OreRegistry {
    OreRegistry::load(&repo_root().join("assets/ores")).expect("assets/ores must load")
}

/// Load `assets/smelting/*.ron`, resolving item names through `items`.
pub fn load_smelt_book(items: &ItemRegistry) -> SmeltBook {
    SmeltBook::load(&repo_root().join("assets/smelting"), items).expect("assets/smelting must load")
}

/// Load `assets/recipes/*.ron`, resolving ingredient names through `items`.
pub fn load_recipe_book(items: &ItemRegistry) -> RecipeBook {
    RecipeBook::load(&repo_root().join("assets/recipes"), items).expect("assets/recipes must load")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the crate, as a test: a server can read every definition it
    /// needs with no GPU, no window and no texture folder.
    ///
    /// It is a compile-time claim as much as a runtime one — this test lives in
    /// a crate whose `Cargo.toml` cannot name `wgpu`, and
    /// `scripts/check-architecture.sh` fails if it ever does.
    #[test]
    fn a_server_can_load_every_definition_without_a_gpu() {
        let blocks = load_block_registry();
        assert!(blocks.id_of("cubara:stone").is_some(), "blocks loaded");

        let items = load_item_registry();
        assert!(items.id_of("cubara:cobble").is_some(), "items loaded");

        load_recipe_book(&items);
        load_smelt_book(&items);
        load_structure_registry();
        load_ore_registry();
    }

    /// `cubara-render`'s loader and this one must not disagree about what a
    /// block is — they differ only in whether they demand textures.
    ///
    /// Asserted through the block the whole world is made of rather than by
    /// comparing registries: `cubara-server` must not depend on `cubara-render`
    /// even in tests, or the dependency it exists to avoid would be back.
    #[test]
    fn the_server_reads_the_same_block_definitions_the_client_does() {
        let blocks = load_block_registry();
        let stone = blocks.id_of("cubara:stone").expect("stone exists");
        assert!(
            blocks.requires_tier(stone) > 0,
            "stone still needs a tool: the server read the real rules, not defaults"
        );
    }
}
