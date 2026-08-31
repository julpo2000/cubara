//! Shaped crafting recipes, and the matcher that answers "what does this grid
//! make?".
//!
//! Shaped, per `docs/PHASE2_ARCHITECTURE.md` decision A: where an ingredient
//! sits matters. Loaded from `assets/recipes/*.ron` through the same machinery
//! blocks and items use, so adding a recipe needs no recompile
//! (`REQUIREMENTS.md` #3).
//!
//! The one idea worth reading before the code: **matching trims first.** Both
//! the recipe's pattern and the offered grid have their empty leading and
//! trailing rows and columns removed, and only then are they compared. That is
//! what lets a 2×2 recipe match in any corner of a 3×3 bench without the player
//! guessing the alignment — and what lets a recipe declare no grid size at all,
//! so the bench gates 3×3 recipes simply by being the only grid they fit in.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::item::{ItemId, ItemRegistry};

/// The largest grid any recipe may describe. A bench is 3×3; a pattern wider
/// than that could never match anything, so it is rejected at load rather than
/// silently never firing.
pub const MAX_GRID: usize = 3;

/// What a recipe produces, resolved. Deliberately not `Deserialize` -- the
/// authored form is [`RecipeOutputDef`], which names its item; ids are assigned
/// per registry, so a file must never contain one.
#[derive(Debug, Clone, Copy)]
pub struct RecipeOutput {
    pub item: ItemId,
    pub count: u8,
}

/// One recipe, as authored. `pattern` rows are equal-length strings; a space
/// means "this cell must be empty"; every other character must appear in `key`.
///
/// Spelled `Recipe(...)` in the file: the `Def` suffix is an internal
/// distinction from the resolved [`Recipe`], and a data format should not have
/// to carry it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "Recipe")]
pub struct RecipeDef {
    pub name: String,
    pub pattern: Vec<String>,
    pub key: HashMap<char, String>,
    pub output: RecipeOutputDef,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecipeOutputDef {
    pub item: String,
    pub count: u8,
}

/// A recipe with its names resolved to ids and its pattern trimmed — the form
/// the matcher compares against.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub name: String,
    /// Row-major, `width` per row. `None` is an empty cell.
    cells: Vec<Option<ItemId>>,
    width: usize,
    height: usize,
    pub output: RecipeOutput,
}

impl Recipe {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }
}

#[derive(Debug)]
pub enum RecipeError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    /// A character in `pattern` that `key` does not explain. Caught here rather
    /// than left to never match, which would look like the recipe simply not
    /// working.
    UnkeyedCharacter {
        recipe: String,
        character: char,
        file: PathBuf,
    },
    /// A `key` entry or an output naming an item the registry does not have.
    UnknownItem {
        recipe: String,
        item: String,
        file: PathBuf,
    },
    /// A pattern with no rows, or one whose rows are all empty: it would match
    /// an empty grid and produce something from nothing.
    EmptyPattern { recipe: String, file: PathBuf },
    /// Rows of differing lengths — almost always a typo, and ambiguous enough
    /// that guessing an intent would be worse than refusing.
    RaggedPattern { recipe: String, file: PathBuf },
    /// Larger than any grid that exists, so it could never fire.
    PatternTooLarge {
        recipe: String,
        width: usize,
        height: usize,
        file: PathBuf,
    },
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

impl fmt::Display for RecipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecipeError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            RecipeError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            RecipeError::UnkeyedCharacter {
                recipe,
                character,
                file,
            } => write!(
                f,
                "{}: recipe {recipe:?} uses {character:?} in its pattern but its key does not \
                 explain it; a space means an empty cell, anything else needs a key entry",
                file.display()
            ),
            RecipeError::UnknownItem { recipe, item, file } => write!(
                f,
                "{}: recipe {recipe:?} names item {item:?}, which no assets/items/*.ron defines",
                file.display()
            ),
            RecipeError::EmptyPattern { recipe, file } => write!(
                f,
                "{}: recipe {recipe:?} has an empty pattern, so it would make something from \
                 nothing",
                file.display()
            ),
            RecipeError::RaggedPattern { recipe, file } => write!(
                f,
                "{}: recipe {recipe:?} has rows of different lengths",
                file.display()
            ),
            RecipeError::PatternTooLarge {
                recipe,
                width,
                height,
                file,
            } => write!(
                f,
                "{}: recipe {recipe:?} is {width}x{height}, larger than the {MAX_GRID}x{MAX_GRID} \
                 bench, so it could never be made",
                file.display()
            ),
            RecipeError::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate recipe {name:?}: {} and {}",
                first.display(),
                second.display()
            ),
        }
    }
}

impl std::error::Error for RecipeError {}

/// Every recipe that exists.
#[derive(Debug, Default)]
pub struct RecipeBook {
    recipes: Vec<Recipe>,
}

impl RecipeBook {
    /// Build from every `*.ron` directly inside `dir`, one [`RecipeDef`] per
    /// file, resolving item names through `items`.
    pub fn load(dir: &Path, items: &ItemRegistry) -> Result<Self, RecipeError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| RecipeError::Io {
            file: dir.to_path_buf(),
            error,
        })?;

        let mut defs = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| RecipeError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| RecipeError::Io {
                file: path.clone(),
                error,
            })?;
            let def: RecipeDef = ron::from_str(&text).map_err(|error| RecipeError::Parse {
                file: path.clone(),
                error,
            })?;
            defs.push((path, def));
        }
        Self::from_defs(defs, items)
    }

    /// Build from definitions already in memory, each paired with the file to
    /// blame in a validation error — so every rule below is testable with no
    /// filesystem involved.
    pub fn from_defs(
        defs: Vec<(PathBuf, RecipeDef)>,
        items: &ItemRegistry,
    ) -> Result<Self, RecipeError> {
        let mut seen: HashMap<String, PathBuf> = HashMap::new();
        let mut recipes = Vec::with_capacity(defs.len());

        for (file, def) in defs {
            if let Some(first) = seen.get(&def.name) {
                return Err(RecipeError::DuplicateName {
                    name: def.name,
                    first: first.clone(),
                    second: file,
                });
            }
            seen.insert(def.name.clone(), file.clone());
            recipes.push(Self::resolve(def, &file, items)?);
        }
        Ok(Self { recipes })
    }

    fn resolve(def: RecipeDef, file: &Path, items: &ItemRegistry) -> Result<Recipe, RecipeError> {
        let width = def.pattern.first().map(|r| r.chars().count()).unwrap_or(0);
        if def.pattern.is_empty() || width == 0 {
            return Err(RecipeError::EmptyPattern {
                recipe: def.name,
                file: file.to_path_buf(),
            });
        }
        if def.pattern.iter().any(|r| r.chars().count() != width) {
            return Err(RecipeError::RaggedPattern {
                recipe: def.name,
                file: file.to_path_buf(),
            });
        }
        let height = def.pattern.len();
        if width > MAX_GRID || height > MAX_GRID {
            return Err(RecipeError::PatternTooLarge {
                recipe: def.name,
                width,
                height,
                file: file.to_path_buf(),
            });
        }

        let mut cells = Vec::with_capacity(width * height);
        for row in &def.pattern {
            for ch in row.chars() {
                if ch == ' ' {
                    cells.push(None);
                    continue;
                }
                let name = def
                    .key
                    .get(&ch)
                    .ok_or_else(|| RecipeError::UnkeyedCharacter {
                        recipe: def.name.clone(),
                        character: ch,
                        file: file.to_path_buf(),
                    })?;
                let id = items.id_of(name).ok_or_else(|| RecipeError::UnknownItem {
                    recipe: def.name.clone(),
                    item: name.clone(),
                    file: file.to_path_buf(),
                })?;
                cells.push(Some(id));
            }
        }
        if cells.iter().all(|c| c.is_none()) {
            return Err(RecipeError::EmptyPattern {
                recipe: def.name,
                file: file.to_path_buf(),
            });
        }

        let output_id = items
            .id_of(&def.output.item)
            .ok_or_else(|| RecipeError::UnknownItem {
                recipe: def.name.clone(),
                item: def.output.item.clone(),
                file: file.to_path_buf(),
            })?;

        let (cells, width, height) = trim(&cells, width, height);
        Ok(Recipe {
            name: def.name,
            cells,
            width,
            height,
            output: RecipeOutput {
                item: output_id,
                count: def.output.count,
            },
        })
    }

    /// What `grid` makes, if anything.
    ///
    /// `grid` is row-major, `width` per row, `None` for an empty cell. Both it
    /// and each recipe are trimmed before comparing, which is what makes the
    /// position within the grid irrelevant.
    ///
    /// Returns the first match in load order. Two recipes with the same trimmed
    /// shape are a data bug — one would be unreachable — and this does not try
    /// to be clever about it; if it becomes a real problem, it wants a load-time
    /// check, not a tie-break rule at match time.
    pub fn find(&self, grid: &[Option<ItemId>], width: usize) -> Option<&Recipe> {
        if width == 0 || grid.is_empty() || !grid.len().is_multiple_of(width) {
            return None;
        }
        let height = grid.len() / width;
        let (cells, w, h) = trim(grid, width, height);
        if cells.is_empty() {
            return None;
        }
        self.recipes
            .iter()
            .find(|r| r.width == w && r.height == h && r.cells == cells)
    }

    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Recipe> {
        self.recipes.iter()
    }
}

/// Drop empty leading and trailing rows and columns, returning the tight
/// rectangle and its size. An all-empty input trims to nothing.
///
/// This is the whole of "position does not matter": two grids that differ only
/// by where the ingredients sit trim to the same thing.
fn trim(
    cells: &[Option<ItemId>],
    width: usize,
    height: usize,
) -> (Vec<Option<ItemId>>, usize, usize) {
    let occupied = |x: usize, y: usize| cells[y * width + x].is_some();

    let first_row = (0..height).find(|&y| (0..width).any(|x| occupied(x, y)));
    let Some(top) = first_row else {
        return (Vec::new(), 0, 0);
    };
    let bottom = (0..height)
        .rfind(|&y| (0..width).any(|x| occupied(x, y)))
        .expect("a row is occupied");
    let left = (0..width)
        .find(|&x| (0..height).any(|y| occupied(x, y)))
        .expect("a column is occupied");
    let right = (0..width)
        .rfind(|&x| (0..height).any(|y| occupied(x, y)))
        .expect("a column is occupied");

    let (w, h) = (right - left + 1, bottom - top + 1);
    let mut out = Vec::with_capacity(w * h);
    for y in top..=bottom {
        for x in left..=right {
            out.push(cells[y * width + x]);
        }
    }
    (out, w, h)
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
                },
            )
        };
        ItemRegistry::from_defs(vec![
            def("cubara:plank"),
            def("cubara:stick"),
            def("cubara:oak_log"),
            def("cubara:crafting_bench"),
        ])
        .expect("fixture registry is valid")
    }

    fn recipe(
        name: &str,
        pattern: &[&str],
        key: &[(char, &str)],
        out: (&str, u8),
    ) -> (PathBuf, RecipeDef) {
        (
            PathBuf::from(format!("{name}.ron")),
            RecipeDef {
                name: name.to_string(),
                pattern: pattern.iter().map(|r| r.to_string()).collect(),
                key: key.iter().map(|&(c, n)| (c, n.to_string())).collect(),
                output: RecipeOutputDef {
                    item: out.0.to_string(),
                    count: out.1,
                },
            },
        )
    }

    /// A 2x2 bench recipe: four planks.
    fn bench_book(items: &ItemRegistry) -> RecipeBook {
        RecipeBook::from_defs(
            vec![recipe(
                "cubara:crafting_bench",
                &["PP", "PP"],
                &[('P', "cubara:plank")],
                ("cubara:crafting_bench", 1),
            )],
            items,
        )
        .expect("fixture recipe is valid")
    }

    fn grid_3x3(cells: [Option<ItemId>; 9]) -> Vec<Option<ItemId>> {
        cells.to_vec()
    }

    #[test]
    fn a_two_by_two_recipe_matches_anywhere_in_a_three_by_three_grid() {
        // Trimming's entire purpose. If this ever regresses, crafting still
        // "works" -- in exactly one corner -- which is the kind of bug players
        // report as "the recipe is wrong".
        let items = items();
        let book = bench_book(&items);
        let p = items.id_of("cubara:plank");

        // Every placement of a 2x2 block inside 3x3: four corners and the two
        // edge-centred positions, which trimming must also handle.
        let placements = [
            [p, p, None, p, p, None, None, None, None], // top-left
            [None, p, p, None, p, p, None, None, None], // top-right
            [None, None, None, p, p, None, p, p, None], // bottom-left
            [None, None, None, None, p, p, None, p, p], // bottom-right
        ];
        for (i, cells) in placements.into_iter().enumerate() {
            let found = book.find(&grid_3x3(cells), 3);
            assert!(
                found.is_some(),
                "placement {i} did not match; trimming is not position-independent"
            );
            assert_eq!(found.unwrap().name, "cubara:crafting_bench");
        }
    }

    #[test]
    fn a_three_wide_recipe_does_not_fit_a_two_wide_grid() {
        let items = items();
        let book = RecipeBook::from_defs(
            vec![recipe(
                "cubara:wide",
                &["PPP"],
                &[('P', "cubara:plank")],
                ("cubara:stick", 1),
            )],
            &items,
        )
        .unwrap();
        let p = items.id_of("cubara:plank");
        // A full 2x2 of planks is not three in a row, at any offset.
        assert!(book.find(&[p, p, p, p], 2).is_none());
    }

    #[test]
    fn an_extra_ingredient_in_a_spare_cell_does_not_match() {
        // A superset must not match: putting a stray plank next to a valid
        // bench shape should make nothing, not silently make a bench and eat
        // the extra.
        let items = items();
        let book = bench_book(&items);
        let p = items.id_of("cubara:plank");
        let cells = [p, p, None, p, p, None, p, None, None];
        assert!(book.find(&grid_3x3(cells), 3).is_none());
    }

    #[test]
    fn a_mirrored_pattern_does_not_match() {
        // Pinning decision, not incidental behaviour: mirroring is deliberately
        // not applied, so an asymmetric recipe stays asymmetric. A recipe that
        // wants both handednesses declares both patterns.
        let items = items();
        let book = RecipeBook::from_defs(
            vec![recipe(
                "cubara:asymmetric",
                &["PS"],
                &[('P', "cubara:plank"), ('S', "cubara:stick")],
                ("cubara:stick", 1),
            )],
            &items,
        )
        .unwrap();
        let p = items.id_of("cubara:plank");
        let s = items.id_of("cubara:stick");
        assert!(
            book.find(&[p, s], 2).is_some(),
            "the authored order matches"
        );
        assert!(
            book.find(&[s, p], 2).is_none(),
            "the mirrored order must not match"
        );
    }

    #[test]
    fn an_empty_grid_makes_nothing() {
        let items = items();
        let book = bench_book(&items);
        assert!(book.find(&[None; 9], 3).is_none());
        assert!(book.find(&[], 3).is_none());
    }

    #[test]
    fn a_pattern_character_with_no_key_entry_fails_to_load() {
        let items = items();
        let err = RecipeBook::from_defs(
            vec![recipe(
                "cubara:typo",
                &["PQ"],
                &[('P', "cubara:plank")],
                ("cubara:stick", 1),
            )],
            &items,
        )
        .expect_err("an unkeyed character must not load");
        match err {
            RecipeError::UnkeyedCharacter {
                recipe, character, ..
            } => {
                assert_eq!(recipe, "cubara:typo");
                assert_eq!(character, 'Q');
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn bad_patterns_are_rejected_at_load() {
        let items = items();
        let cases: Vec<(&str, Vec<&str>)> = vec![
            ("empty", vec![]),
            ("all blank", vec!["  ", "  "]),
            ("ragged", vec!["PP", "P"]),
            ("too large", vec!["PPPP"]),
        ];
        for (what, pattern) in cases {
            let r = RecipeBook::from_defs(
                vec![recipe(
                    "cubara:bad",
                    &pattern,
                    &[('P', "cubara:plank")],
                    ("cubara:stick", 1),
                )],
                &items,
            );
            assert!(r.is_err(), "{what} pattern should not load");
        }
    }

    #[test]
    fn the_shipped_recipes_walk_the_whole_ladder_to_iron() {
        // Loads the real assets/, not a fixture: a recipe that parses in a test
        // but not in the game is exactly the failure worth catching, and so is
        // a ladder with a rung missing.
        //
        // Walks decision D end to end -- log to plank to stick to bench, then
        // the three picks and the furnace -- asserting each step *makes the
        // next one's ingredient*, rather than just that seven files parsed.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let items = ItemRegistry::load(&root.join("assets/items")).expect("assets/items");
        let book = RecipeBook::load(&root.join("assets/recipes"), &items).expect("assets/recipes");

        let id = |n: &str| items.id_of(n).unwrap_or_else(|| panic!("no item {n}"));
        let makes = |grid: &[Option<ItemId>], w: usize, want: &str, count: u8| {
            let r = book
                .find(grid, w)
                .unwrap_or_else(|| panic!("nothing matched for {want}"));
            assert_eq!(
                items.name_of(r.output.item),
                Some(want),
                "wrong output for {want}"
            );
            assert_eq!(r.output.count, count, "wrong count for {want}");
        };

        let log = Some(id("cubara:oak_log"));
        let p = Some(id("cubara:plank"));
        let st = Some(id("cubara:stick"));
        // Cobble, not stone: block 2.4a made mined stone drop `cubara:cobble`,
        // so cobble is what the ladder actually has in hand at this rung.
        let cb = Some(id("cubara:cobble"));
        let ir = Some(id("cubara:iron_ingot"));

        // 1. A log makes four planks -- a 1x1 recipe, so it works in the
        //    inventory's 2x2 grid.
        makes(&[log, None, None, None], 2, "cubara:plank", 4);
        // 2. Two planks stacked make sticks. Vertical, and 2x2 fits.
        makes(&[p, None, p, None], 2, "cubara:stick", 4);
        // 3. Four planks make the bench -- craftable *without* a bench, which
        //    is what stops the ladder deadlocking on its first rung.
        makes(&[p, p, p, p], 2, "cubara:crafting_bench", 1);
        // 4. From here on it is 3x3, i.e. the bench is required.
        makes(
            &[p, p, p, None, st, None, None, st, None],
            3,
            "cubara:wooden_pick",
            1,
        );
        makes(
            &[cb, cb, cb, None, st, None, None, st, None],
            3,
            "cubara:stone_pick",
            1,
        );
        makes(
            &[cb, cb, cb, cb, None, cb, cb, cb, cb],
            3,
            "cubara:furnace",
            1,
        );
        makes(
            &[ir, ir, ir, None, st, None, None, st, None],
            3,
            "cubara:iron_pick",
            1,
        );
    }

    #[test]
    fn the_bench_is_reachable_without_a_bench() {
        // The one property that could deadlock the whole ladder: if the bench
        // recipe needed three columns, nothing could ever be crafted, because
        // the inventory grid is 2x2 and the bench is what unlocks 3x3.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let items = ItemRegistry::load(&root.join("assets/items")).expect("assets/items");
        let book = RecipeBook::load(&root.join("assets/recipes"), &items).expect("assets/recipes");

        let bench = book
            .iter()
            .find(|r| r.name == "cubara:crafting_bench")
            .expect("the bench recipe exists");
        assert!(
            bench.width() <= 2 && bench.height() <= 2,
            "the bench must fit the inventory's 2x2 grid, or it can never be made              (got {}x{})",
            bench.width(),
            bench.height()
        );
    }

    #[test]
    fn an_unknown_item_name_fails_to_load_naming_it() {
        let items = items();
        let err = RecipeBook::from_defs(
            vec![recipe(
                "cubara:ghost",
                &["G"],
                &[('G', "cubara:does_not_exist")],
                ("cubara:stick", 1),
            )],
            &items,
        )
        .expect_err("an unknown ingredient must not load");
        assert!(
            matches!(err, RecipeError::UnknownItem { ref item, .. } if item == "cubara:does_not_exist"),
            "wrong error: {err}"
        );
    }
}
