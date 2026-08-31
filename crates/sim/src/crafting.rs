//! The crafting grid, the cursor, and taking the result.
//!
//! No rendering and no notion of a pixel: the UI turns a click position into a
//! [`SlotRef`] and everything below this line is a state machine over slot
//! indices. That is deliberate — the owner chose click-to-pick-up over
//! drag-and-drop precisely so this could be a state machine
//! (`docs/PHASE2_ARCHITECTURE.md` §3.1), which is why every rule here is a unit
//! test with no mouse involved.

use cubara_voxel::{ItemRegistry, ItemStack, ItemState, RecipeBook, MAX_GRID};

use crate::inventory::Inventory;

/// Where a click landed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotRef {
    Inventory(usize),
    Grid(usize),
    Result,
}

/// The crafting grid and what the cursor is holding.
///
/// Always nine cells, with `width` deciding how many are reachable: 2 in the
/// inventory, 3 at a bench. One storage layout rather than two, so switching
/// between them is a number rather than a different type — and so closing a
/// bench has one obvious rule for the cells that are no longer reachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crafting {
    cells: [Option<ItemStack>; MAX_GRID * MAX_GRID],
    width: usize,
    held: Option<ItemStack>,
}

impl Default for Crafting {
    fn default() -> Self {
        Self::new(2)
    }
}

impl Crafting {
    /// A grid `width` cells on a side. Clamped to [`MAX_GRID`]: a wider grid
    /// could hold ingredients no recipe can describe.
    pub fn new(width: usize) -> Self {
        Self {
            cells: [None; MAX_GRID * MAX_GRID],
            width: width.clamp(1, MAX_GRID),
            held: None,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Widen or narrow the usable area — opening a bench, or closing one.
    ///
    /// Cells outside the new width keep their contents rather than being
    /// cleared: [`close`](Self::close) is what empties the grid, and silently
    /// deleting items on a width change would be a second, invisible way to
    /// lose them.
    pub fn set_width(&mut self, width: usize) {
        self.width = width.clamp(1, MAX_GRID);
    }

    pub fn held(&self) -> Option<ItemStack> {
        self.held
    }

    pub fn cell(&self, index: usize) -> Option<ItemStack> {
        self.cells.get(index).copied().flatten()
    }

    /// Grid cells in row-major order, `width` per row — what
    /// [`RecipeBook::find`] is fed.
    fn pattern(&self) -> Vec<Option<cubara_voxel::ItemId>> {
        let mut out = Vec::with_capacity(self.width * self.width);
        for y in 0..self.width {
            for x in 0..self.width {
                out.push(self.cells[y * MAX_GRID + x].map(|s| s.item()));
            }
        }
        out
    }

    /// What the grid currently makes, as a fresh stack. `None` if nothing
    /// matches.
    ///
    /// Fresh via [`ItemRegistry::new_stack`], so a crafted tool comes out at
    /// full durability rather than inheriting anything from its ingredients.
    pub fn result(&self, book: &RecipeBook, items: &ItemRegistry) -> Option<ItemStack> {
        let recipe = book.find(&self.pattern(), self.width)?;
        items
            .new_stack(recipe.output.item, recipe.output.count)
            .ok()
    }

    /// Handle a click. `right` is the right mouse button.
    ///
    /// The rules, all of them, so no call site has to invent one:
    ///
    /// | cursor | slot | left-click | right-click |
    /// |---|---|---|---|
    /// | empty | occupied | lift the whole stack | take half, rounded up |
    /// | full | empty | put it all down | place exactly one |
    /// | full | same item | merge up to `max_stack`, remainder stays held | place one |
    /// | full | other item | swap | swap |
    ///
    /// The result slot is special and is handled by
    /// [`take_result`](Self::take_result).
    pub fn click(
        &mut self,
        slot: SlotRef,
        right: bool,
        inventory: &mut Inventory,
        items: &ItemRegistry,
        book: &RecipeBook,
    ) {
        match slot {
            SlotRef::Result => {
                if !right {
                    self.take_result(book, items);
                }
                // Right-clicking the result does nothing: "half a craft" is not
                // a thing, and silently doing a whole one would be a surprise.
            }
            SlotRef::Grid(i) if i < MAX_GRID * MAX_GRID => {
                let current = self.cells[i];
                let (new_slot, new_held) = Self::exchange(current, self.held, right, items);
                self.cells[i] = new_slot;
                self.held = new_held;
            }
            SlotRef::Inventory(i) => {
                let current = inventory.slot(i);
                let (new_slot, new_held) = Self::exchange(current, self.held, right, items);
                inventory.set_slot(i, new_slot);
                self.held = new_held;
            }
            SlotRef::Grid(_) => {}
        }
    }

    /// The one place the click rules live, shared by every kind of slot -- an
    /// inventory slot and a grid cell behave identically, and writing that
    /// twice is how they would drift apart.
    fn exchange(
        slot: Option<ItemStack>,
        held: Option<ItemStack>,
        right: bool,
        items: &ItemRegistry,
    ) -> (Option<ItemStack>, Option<ItemStack>) {
        let rebuild = |stack: ItemStack, count: u8| -> Option<ItemStack> {
            if count == 0 {
                return None;
            }
            ItemStack::new(
                stack.item(),
                count,
                stack.state(),
                items.max_stack(stack.item()),
            )
            .ok()
        };

        match (slot, held) {
            (None, None) => (None, None),

            // Cursor empty: take everything, or half rounded up.
            (Some(s), None) => {
                if right {
                    let take = s.count().div_ceil(2);
                    (rebuild(s, s.count() - take), rebuild(s, take))
                } else {
                    (None, Some(s))
                }
            }

            // Cursor full, slot empty: put it all down, or exactly one.
            (None, Some(h)) => {
                if right {
                    (rebuild(h, 1), rebuild(h, h.count() - 1))
                } else {
                    (Some(h), None)
                }
            }

            (Some(s), Some(h)) => {
                if s.mergeable_with(h) {
                    let max = items.max_stack(s.item());
                    let room = max.saturating_sub(s.count());
                    let moved = if right {
                        room.min(1)
                    } else {
                        room.min(h.count())
                    };
                    (rebuild(s, s.count() + moved), rebuild(h, h.count() - moved))
                } else {
                    // Different items -- swap, both ways round, including a
                    // worn tool against an identical-looking one (they are not
                    // mergeable, so they swap rather than stacking).
                    (Some(h), Some(s))
                }
            }
        }
    }

    /// Take what the grid makes, consuming **one of each ingredient**.
    ///
    /// One per cell, not the whole stack in it: a grid of 64 planks makes one
    /// bench per click and stays usable, which is what the player expects and
    /// what makes repeated crafting possible at all.
    ///
    /// All-or-nothing against the cursor. If the cursor holds something the
    /// result cannot merge into, the click does nothing -- rather than needing
    /// a rule for where the overflow goes, which is a gameplay decision and not
    /// mine to make.
    pub fn take_result(&mut self, book: &RecipeBook, items: &ItemRegistry) -> bool {
        let Some(result) = self.result(book, items) else {
            return false;
        };

        let new_held = match self.held {
            None => result,
            Some(h) => {
                if !h.mergeable_with(result) {
                    return false;
                }
                let max = items.max_stack(h.item());
                let total = h.count() as u16 + result.count() as u16;
                if total > max as u16 {
                    return false;
                }
                match ItemStack::new(h.item(), total as u8, ItemState::None, max) {
                    Ok(s) => s,
                    Err(_) => return false,
                }
            }
        };

        // Only now that it is certain to succeed: consume one per ingredient.
        for y in 0..self.width {
            for x in 0..self.width {
                let i = y * MAX_GRID + x;
                let Some(stack) = self.cells[i] else { continue };
                self.cells[i] = match stack.count() {
                    0 | 1 => None,
                    n => ItemStack::new(
                        stack.item(),
                        n - 1,
                        stack.state(),
                        items.max_stack(stack.item()),
                    )
                    .ok(),
                };
            }
        }
        self.held = Some(new_held);
        true
    }

    /// Return the grid's contents and the cursor to `inventory`.
    ///
    /// Returns `true` when everything fit. Anything that did not is **left
    /// where it was**, and the caller should keep the screen open: refusing to
    /// close is more honest than eating the items, and dropping them on the
    /// floor needs entities that do not exist until ECS (2.5).
    pub fn close(&mut self, inventory: &mut Inventory, items: &ItemRegistry) -> bool {
        let mut all_fit = true;
        for i in 0..MAX_GRID * MAX_GRID {
            let Some(stack) = self.cells[i] else { continue };
            match inventory.add(stack, items) {
                None => self.cells[i] = None,
                Some(left) => {
                    self.cells[i] = Some(left);
                    all_fit = false;
                }
            }
        }
        if let Some(h) = self.held {
            match inventory.add(h, items) {
                None => self.held = None,
                Some(left) => {
                    self.held = Some(left);
                    all_fit = false;
                }
            }
        }
        all_fit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::{ItemDef, ItemId, RecipeDef, RecipeOutputDef};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn items() -> ItemRegistry {
        let def = |name: &str, max: u8, dur: Option<u16>| {
            (
                PathBuf::from(format!("{name}.ron")),
                ItemDef {
                    name: name.to_string(),
                    max_stack: max,
                    durability: dur,
                    tier: 0,
                    speed: None,
                },
            )
        };
        ItemRegistry::from_defs(vec![
            def("cubara:plank", 64, None),
            def("cubara:stick", 64, None),
            def("cubara:crafting_bench", 64, None),
            def("cubara:wooden_pick", 1, Some(50)),
        ])
        .expect("fixture registry is valid")
    }

    fn book(items: &ItemRegistry) -> RecipeBook {
        let mut key = HashMap::new();
        key.insert('P', "cubara:plank".to_string());
        RecipeBook::from_defs(
            vec![(
                PathBuf::from("bench.ron"),
                RecipeDef {
                    name: "cubara:crafting_bench".to_string(),
                    pattern: vec!["PP".to_string(), "PP".to_string()],
                    key,
                    output: RecipeOutputDef {
                        item: "cubara:crafting_bench".to_string(),
                        count: 1,
                    },
                },
            )],
            items,
        )
        .expect("fixture recipe is valid")
    }

    fn id(r: &ItemRegistry, n: &str) -> ItemId {
        r.id_of(n).expect("fixture item")
    }

    /// A 2x2 grid with `n` planks in every cell.
    fn grid_of_planks(r: &ItemRegistry, n: u8) -> Crafting {
        let mut c = Crafting::new(2);
        let plank = id(r, "cubara:plank");
        for i in [0, 1, MAX_GRID, MAX_GRID + 1] {
            c.cells[i] = Some(r.new_stack(plank, n).unwrap());
        }
        c
    }

    #[test]
    fn left_click_lifts_a_whole_stack_and_puts_it_down() {
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = Crafting::new(2);
        inv.add(r.new_stack(id(&r, "cubara:plank"), 10).unwrap(), &r);

        c.click(SlotRef::Inventory(0), false, &mut inv, &r, &b);
        assert_eq!(c.held().map(|s| s.count()), Some(10), "the whole stack");
        assert_eq!(inv.slot(0), None, "and the slot is empty");

        c.click(SlotRef::Grid(0), false, &mut inv, &r, &b);
        assert_eq!(c.held(), None);
        assert_eq!(c.cell(0).map(|s| s.count()), Some(10));
    }

    #[test]
    fn right_click_takes_half_rounded_up_and_places_one() {
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = Crafting::new(2);
        inv.add(r.new_stack(id(&r, "cubara:plank"), 7).unwrap(), &r);

        c.click(SlotRef::Inventory(0), true, &mut inv, &r, &b);
        assert_eq!(
            c.held().map(|s| s.count()),
            Some(4),
            "half of 7, rounded up"
        );
        assert_eq!(inv.slot(0).map(|s| s.count()), Some(3));

        c.click(SlotRef::Grid(0), true, &mut inv, &r, &b);
        assert_eq!(c.cell(0).map(|s| s.count()), Some(1), "exactly one placed");
        assert_eq!(c.held().map(|s| s.count()), Some(3));
    }

    #[test]
    fn clicking_the_same_item_merges_and_keeps_the_remainder_held() {
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = Crafting::new(2);
        let plank = id(&r, "cubara:plank");
        c.cells[0] = Some(r.new_stack(plank, 60).unwrap());
        c.held = Some(r.new_stack(plank, 10).unwrap());

        c.click(SlotRef::Grid(0), false, &mut inv, &r, &b);
        assert_eq!(
            c.cell(0).map(|s| s.count()),
            Some(64),
            "filled to max_stack"
        );
        assert_eq!(c.held().map(|s| s.count()), Some(6), "the rest stays held");
    }

    #[test]
    fn clicking_a_different_item_swaps() {
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = Crafting::new(2);
        c.cells[0] = Some(r.new_stack(id(&r, "cubara:plank"), 3).unwrap());
        c.held = Some(r.new_stack(id(&r, "cubara:stick"), 5).unwrap());

        c.click(SlotRef::Grid(0), false, &mut inv, &r, &b);
        assert_eq!(c.cell(0).map(|s| s.item()), Some(id(&r, "cubara:stick")));
        assert_eq!(c.held().map(|s| s.item()), Some(id(&r, "cubara:plank")));
    }

    #[test]
    fn taking_the_result_consumes_one_per_cell_not_the_whole_stack() {
        // The rule that makes repeated crafting possible: a grid of 64 planks
        // must make one bench per click and stay usable, not empty itself.
        let r = items();
        let b = book(&r);
        let mut c = grid_of_planks(&r, 64);

        assert!(c.take_result(&b, &r));
        assert_eq!(
            c.held().map(|s| s.item()),
            Some(id(&r, "cubara:crafting_bench"))
        );
        for i in [0, 1, MAX_GRID, MAX_GRID + 1] {
            assert_eq!(c.cell(i).map(|s| s.count()), Some(63), "cell {i}");
        }
    }

    #[test]
    fn the_last_ingredient_empties_the_cell() {
        let r = items();
        let b = book(&r);
        let mut c = grid_of_planks(&r, 1);

        assert!(c.take_result(&b, &r));
        for i in [0, 1, MAX_GRID, MAX_GRID + 1] {
            assert_eq!(c.cell(i), None, "cell {i} is empty");
        }
        // And nothing can be made now.
        assert!(c.result(&b, &r).is_none());
    }

    #[test]
    fn taking_the_result_merges_into_a_compatible_cursor() {
        let r = items();
        let b = book(&r);
        let mut c = grid_of_planks(&r, 2);
        c.held = Some(r.new_stack(id(&r, "cubara:crafting_bench"), 3).unwrap());

        assert!(c.take_result(&b, &r));
        assert_eq!(c.held().map(|s| s.count()), Some(4), "3 + 1");
    }

    #[test]
    fn taking_the_result_does_nothing_with_an_incompatible_cursor() {
        // All-or-nothing: rather than needing a rule for where the overflow
        // goes, which is a gameplay decision. Crucially the ingredients must
        // *not* be consumed by a click that produced nothing.
        let r = items();
        let b = book(&r);
        let mut c = grid_of_planks(&r, 2);
        c.held = Some(r.new_stack(id(&r, "cubara:stick"), 1).unwrap());

        assert!(!c.take_result(&b, &r), "the craft does not happen");
        for i in [0, 1, MAX_GRID, MAX_GRID + 1] {
            assert_eq!(
                c.cell(i).map(|s| s.count()),
                Some(2),
                "cell {i} must be untouched"
            );
        }
        assert_eq!(c.held().map(|s| s.item()), Some(id(&r, "cubara:stick")));
    }

    #[test]
    fn clicking_into_the_result_slot_does_nothing() {
        // Swapping into the result would put an item into a slot that
        // regenerates from the grid -- silent data loss.
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = Crafting::new(2);
        c.held = Some(r.new_stack(id(&r, "cubara:stick"), 4).unwrap());

        c.click(SlotRef::Result, false, &mut inv, &r, &b);
        assert_eq!(
            c.held().map(|s| s.count()),
            Some(4),
            "the cursor is unchanged"
        );
    }

    #[test]
    fn right_clicking_the_result_does_nothing() {
        let r = items();
        let b = book(&r);
        let mut inv = Inventory::new();
        let mut c = grid_of_planks(&r, 2);

        c.click(SlotRef::Result, true, &mut inv, &r, &b);
        assert_eq!(c.held(), None, "half a craft is not a thing");
        assert_eq!(c.cell(0).map(|s| s.count()), Some(2), "nothing consumed");
    }

    #[test]
    fn a_two_by_two_recipe_does_not_fire_in_the_wrong_cells_of_a_bench() {
        // `width` is what makes a bench a bench: the same nine cells, with more
        // of them reachable. A 2x2 recipe still matches inside a 3x3 because
        // the matcher trims (#147) -- this pins that the *storage* layout does
        // not quietly change which cells count.
        let r = items();
        let b = book(&r);
        let plank = id(&r, "cubara:plank");
        let mut c = Crafting::new(3);
        // Bottom-right 2x2 of a 3x3.
        for i in [
            MAX_GRID + 1,
            MAX_GRID + 2,
            2 * MAX_GRID + 1,
            2 * MAX_GRID + 2,
        ] {
            c.cells[i] = Some(r.new_stack(plank, 1).unwrap());
        }
        assert!(
            c.result(&b, &r).is_some(),
            "a 2x2 recipe must match in a corner of the bench"
        );
    }

    #[test]
    fn the_same_click_sequence_reaches_the_same_state() {
        // The property the world-state hash rests on. Every rule in `exchange`
        // is written to depend only on the two stacks and the button -- nothing
        // about ordering, recency or hashing -- so an identical script must
        // land identically. If this regresses, a replay desyncs days later,
        // which is the hardest failure in this project to trace.
        let r = items();
        let b = book(&r);
        let plank = id(&r, "cubara:plank");

        let run = || {
            let mut inv = Inventory::new();
            inv.add(r.new_stack(plank, 40).unwrap(), &r);
            let mut c = Crafting::new(2);
            for (slot, right) in [
                (SlotRef::Inventory(0), false),
                (SlotRef::Grid(0), true),
                (SlotRef::Grid(1), true),
                (SlotRef::Grid(MAX_GRID), true),
                (SlotRef::Grid(MAX_GRID + 1), true),
                (SlotRef::Inventory(0), false),
                (SlotRef::Result, false),
                (SlotRef::Inventory(5), false),
            ] {
                c.click(slot, right, &mut inv, &r, &b);
            }
            (c, inv)
        };

        let (c1, i1) = run();
        let (c2, i2) = run();
        assert_eq!(c1, c2, "the grid and cursor must match");
        assert_eq!(i1, i2, "and so must the inventory");
    }

    #[test]
    fn closing_returns_everything_that_fits() {
        let r = items();
        let mut inv = Inventory::new();
        let mut c = grid_of_planks(&r, 5);
        c.held = Some(r.new_stack(id(&r, "cubara:stick"), 2).unwrap());

        assert!(c.close(&mut inv, &r), "an empty inventory takes it all");
        assert_eq!(c.cell(0), None);
        assert_eq!(c.held(), None);
        assert_eq!(
            inv.slot(0).map(|s| s.count()),
            Some(20),
            "four cells of 5 planks merged into one stack"
        );
    }

    #[test]
    fn closing_leaves_behind_what_does_not_fit() {
        // Refusing to close is more honest than eating the items, and dropping
        // them needs entities that do not exist yet (2.5).
        let r = items();
        let mut inv = Inventory::new();
        let stick = id(&r, "cubara:stick");
        for _ in 0..crate::inventory::SLOT_COUNT {
            inv.add(r.new_stack(stick, 64).unwrap(), &r);
        }
        let mut c = grid_of_planks(&r, 5);

        assert!(!c.close(&mut inv, &r), "a full inventory cannot take it");
        assert_eq!(
            c.cell(0).map(|s| s.count()),
            Some(5),
            "the planks are still in the grid, not gone"
        );
    }
}
