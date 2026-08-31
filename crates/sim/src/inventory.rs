//! What the player is carrying.
//!
//! Simulation state, not UI state: inventory contents feed the world-state hash
//! (`crate::hash`), so every rule here has to be a *specification* rather than
//! whatever the implementation happened to do. See
//! `docs/PHASE2_ARCHITECTURE.md` §2.

use cubara_voxel::{ItemRegistry, ItemStack, ItemState};

/// Total slots the player carries. Structural, not content: the UI layout and
/// (from block 2.8) the save format both depend on it, where a *recipe* is
/// content and lives in RON. If this ever needs to be per-world, that is a
/// save-format change and belongs with 2.8.
pub const SLOT_COUNT: usize = 36;

/// How many of those slots are the hotbar. They are **slots `0..HOTBAR_WIDTH`
/// of the same array**, not a second container -- a slot is a slot, and which
/// ones the UI draws along the bottom is a rendering concern. Two containers
/// would mean two code paths for "put this item somewhere", which is the
/// divergence `ARCHITECTURE.md` Rule 5 exists to prevent.
pub const HOTBAR_WIDTH: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inventory {
    slots: [Option<ItemStack>; SLOT_COUNT],
    selected: u8,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}

impl Inventory {
    pub fn new() -> Self {
        Self {
            slots: [None; SLOT_COUNT],
            selected: 0,
        }
    }

    /// Try to take `stack` in, returning whatever would not fit.
    ///
    /// **The order is the specification, not an implementation detail.**
    /// Inventory contents feed the world-state hash, so a rule that depended on
    /// iteration order, recency or hashing would make two identical
    /// playthroughs diverge -- which is Rule 1. In full:
    ///
    /// 1. Merge into the **lowest-indexed** existing stack of the same item
    ///    that has room and carries [`ItemState::None`].
    /// 2. Otherwise place in the **lowest-indexed** empty slot.
    /// 3. Otherwise return what is left, so the caller can decide. The items
    ///    are **not** silently dropped.
    ///
    /// A stack carrying state never merges (step 1's `ItemState::None`
    /// condition). That falls out of `ItemStack`'s invariant -- state implies a
    /// count of one -- rather than needing a case of its own: two half-worn
    /// tools are not interchangeable, so they occupy two slots.
    pub fn add(&mut self, stack: ItemStack, registry: &ItemRegistry) -> Option<ItemStack> {
        let item = stack.item();
        let state = stack.state();
        let max = registry.max_stack(item);
        // `stack` is a valid `ItemStack`, so its count is already <= max --
        // which is what guarantees the leftover below is representable as one
        // stack rather than needing several.
        let mut remaining = stack.count();

        if state == ItemState::None {
            for slot in self.slots.iter_mut() {
                let Some(existing) = slot else { continue };
                if existing.item() != item || existing.state() != ItemState::None {
                    continue;
                }
                let room = max.saturating_sub(existing.count());
                if room == 0 {
                    continue;
                }
                let moved = room.min(remaining);
                *existing = ItemStack::new(item, existing.count() + moved, ItemState::None, max)
                    .expect("a merge that respects `room` stays within max_stack");
                remaining -= moved;
                if remaining == 0 {
                    return None;
                }
            }
        }

        for slot in self.slots.iter_mut() {
            if slot.is_some() {
                continue;
            }
            let placed = if state == ItemState::None {
                remaining.min(max)
            } else {
                1
            };
            *slot = Some(
                ItemStack::new(item, placed, state, max)
                    .expect("placing at most max_stack into an empty slot is valid"),
            );
            remaining -= placed;
            if remaining == 0 {
                return None;
            }
        }

        ItemStack::new(item, remaining, state, max).ok()
    }

    pub fn slot(&self, index: usize) -> Option<ItemStack> {
        self.slots.get(index).copied().flatten()
    }

    /// Remove and return whatever is in `index`.
    pub fn take(&mut self, index: usize) -> Option<ItemStack> {
        self.slots.get_mut(index).and_then(|s| s.take())
    }

    /// Remove exactly one item from `index`, returning a stack of that one.
    ///
    /// Lives here rather than at the call site because [`ItemStack`]'s fields
    /// are private, and they are private on purpose: the count/state invariant
    /// cannot be enforced by a constructor that anyone can bypass by editing a
    /// field. Decrementing is the one legitimate mutation, so it is spelled out
    /// once, here.
    ///
    /// The slot empties when the last one goes -- an `Some(stack)` with a count
    /// of zero is not representable, which is the invariant doing its job.
    pub fn take_one(&mut self, index: usize, registry: &ItemRegistry) -> Option<ItemStack> {
        let slot = self.slots.get_mut(index)?;
        let stack = (*slot)?;
        let max = registry.max_stack(stack.item());
        let one = ItemStack::new(stack.item(), 1, stack.state(), max).ok()?;
        *slot = match stack.count() {
            0 | 1 => None,
            n => ItemStack::new(stack.item(), n - 1, stack.state(), max).ok(),
        };
        Some(one)
    }

    /// Overwrite a slot outright.
    ///
    /// The click rules in [`crate::crafting`] compute a slot's new contents and
    /// the cursor's together, so they need to *set* rather than add: `add`'s
    /// lowest-indexed-first rule is right for a pickup and wrong for "put this
    /// exact stack in this exact slot".
    pub fn set_slot(&mut self, index: usize, stack: Option<ItemStack>) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = stack;
        }
    }

    /// Every slot in index order -- the order the world-state hash reads them
    /// in, and the only order anything should.
    pub fn slots(&self) -> impl Iterator<Item = Option<ItemStack>> + '_ {
        self.slots.iter().copied()
    }

    /// Which hotbar slot is held. Always a valid hotbar index.
    pub fn selected_slot(&self) -> u8 {
        self.selected
    }

    /// The stack in the held hotbar slot, if any.
    pub fn selected_stack(&self) -> Option<ItemStack> {
        self.slot(self.selected as usize)
    }

    /// Select a hotbar slot, clamped rather than rejected: a stray index is a
    /// UI bug, and clamping keeps the sim in a valid state either way.
    pub fn select(&mut self, index: u8) {
        self.selected = index.min(HOTBAR_WIDTH as u8 - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::{ItemDef, ItemId, ItemRegistry};
    use std::path::PathBuf;

    fn registry() -> ItemRegistry {
        let def = |name: &str, max_stack: u8, durability: Option<u16>| {
            (
                PathBuf::from(format!("{name}.ron")),
                ItemDef {
                    name: name.to_string(),
                    max_stack,
                    durability,
                    tier: 0,
                    speed: None,
                },
            )
        };
        ItemRegistry::from_defs(vec![
            def("cubara:oak_log", 64, None),
            def("cubara:plank", 64, None),
            def("cubara:wooden_pick", 1, Some(50)),
        ])
        .expect("fixture registry is valid")
    }

    fn id(r: &ItemRegistry, name: &str) -> ItemId {
        r.id_of(name).expect("fixture item exists")
    }

    /// The occupied slots as `(index, item, count)`, so a test can assert the
    /// exact layout rather than just a total.
    fn layout(inv: &Inventory) -> Vec<(usize, ItemId, u8)> {
        inv.slots()
            .enumerate()
            .filter_map(|(i, s)| s.map(|s| (i, s.item(), s.count())))
            .collect()
    }

    #[test]
    fn all_three_insertion_steps_in_order() {
        // Exercises merge, then empty-slot placement, then the full-inventory
        // leftover -- asserting the exact layout, because "the right total"
        // would pass even if the rule picked slots arbitrarily, and the rule
        // is what the world-state hash depends on.
        let r = registry();
        let mut inv = Inventory::new();
        let log = id(&r, "cubara:oak_log");
        let plank = id(&r, "cubara:plank");

        assert_eq!(inv.add(r.new_stack(log, 10).unwrap(), &r), None);
        assert_eq!(layout(&inv), vec![(0, log, 10)]);

        // A different item cannot merge, so it takes the next empty slot.
        assert_eq!(inv.add(r.new_stack(plank, 5).unwrap(), &r), None);
        assert_eq!(layout(&inv), vec![(0, log, 10), (1, plank, 5)]);

        // More logs merge into slot 0 -- the lowest-indexed match -- not into
        // a fresh slot after the planks.
        assert_eq!(inv.add(r.new_stack(log, 4).unwrap(), &r), None);
        assert_eq!(layout(&inv), vec![(0, log, 14), (1, plank, 5)]);
    }

    #[test]
    fn overflow_starts_a_new_stack_at_the_lowest_free_index() {
        let r = registry();
        let mut inv = Inventory::new();
        let log = id(&r, "cubara:oak_log");

        inv.add(r.new_stack(log, 60).unwrap(), &r);
        // 60 + 10 exceeds max_stack 64: slot 0 tops up to 64, the remaining 6
        // start slot 1.
        assert_eq!(inv.add(r.new_stack(log, 10).unwrap(), &r), None);
        assert_eq!(layout(&inv), vec![(0, log, 64), (1, log, 6)]);
    }

    #[test]
    fn two_tools_of_the_same_kind_occupy_two_slots() {
        // The invariant reaching the inventory: tools carry their own wear, so
        // they never merge -- even at identical wear, because they are about
        // to diverge.
        let r = registry();
        let mut inv = Inventory::new();
        let pick = id(&r, "cubara:wooden_pick");

        inv.add(r.new_stack(pick, 1).unwrap(), &r);
        inv.add(r.new_stack(pick, 1).unwrap(), &r);
        assert_eq!(layout(&inv), vec![(0, pick, 1), (1, pick, 1)]);
    }

    #[test]
    fn a_full_inventory_returns_the_remainder_rather_than_losing_it() {
        let r = registry();
        let mut inv = Inventory::new();
        let log = id(&r, "cubara:oak_log");
        let plank = id(&r, "cubara:plank");

        // Fill every slot with a full stack of planks, so no log can merge and
        // no slot is free.
        for _ in 0..SLOT_COUNT {
            assert_eq!(inv.add(r.new_stack(plank, 64).unwrap(), &r), None);
        }
        let leftover = inv
            .add(r.new_stack(log, 7).unwrap(), &r)
            .expect("a full inventory must hand the items back");
        assert_eq!(leftover.item(), log);
        assert_eq!(leftover.count(), 7, "all seven come back, not some of them");
    }

    #[test]
    fn a_partly_full_inventory_returns_only_what_did_not_fit() {
        let r = registry();
        let mut inv = Inventory::new();
        let log = id(&r, "cubara:oak_log");
        let plank = id(&r, "cubara:plank");

        for _ in 0..SLOT_COUNT - 1 {
            inv.add(r.new_stack(plank, 64).unwrap(), &r);
        }
        // One slot left. 64 logs fit exactly; a further 3 do not.
        assert_eq!(inv.add(r.new_stack(log, 64).unwrap(), &r), None);
        let leftover = inv.add(r.new_stack(log, 3).unwrap(), &r).unwrap();
        assert_eq!(leftover.count(), 3);
    }

    #[test]
    fn the_selected_slot_stays_inside_the_hotbar() {
        let mut inv = Inventory::new();
        inv.select(3);
        assert_eq!(inv.selected_slot(), 3);
        // A stray index is a UI bug; the sim clamps rather than panicking or
        // pointing at a main-inventory slot.
        inv.select(200);
        assert_eq!(inv.selected_slot(), HOTBAR_WIDTH as u8 - 1);
    }

    #[test]
    fn the_same_add_sequence_produces_the_same_layout() {
        // The property the world-state hash rests on. `add`'s rule is written
        // as "lowest-indexed" precisely so this holds; a rule that consulted
        // iteration order or recency would pass every other test in this file
        // and fail here -- and in a replay, days later, as a desync.
        let r = registry();
        let log = id(&r, "cubara:oak_log");
        let plank = id(&r, "cubara:plank");
        let pick = id(&r, "cubara:wooden_pick");

        let run = || {
            let mut inv = Inventory::new();
            for (item, n) in [
                (log, 40),
                (plank, 7),
                (log, 40),
                (pick, 1),
                (plank, 64),
                (pick, 1),
                (log, 3),
            ] {
                inv.add(r.new_stack(item, n).unwrap(), &r);
            }
            inv
        };

        assert_eq!(
            layout(&run()),
            layout(&run()),
            "the same sequence of adds must produce the same slots every time"
        );
    }

    #[test]
    fn take_empties_the_slot() {
        let r = registry();
        let mut inv = Inventory::new();
        let log = id(&r, "cubara:oak_log");
        inv.add(r.new_stack(log, 5).unwrap(), &r);

        let taken = inv.take(0).expect("slot 0 holds the logs");
        assert_eq!(taken.count(), 5);
        assert_eq!(inv.slot(0), None);
        assert_eq!(inv.take(0), None, "taking twice yields nothing");
    }
}
