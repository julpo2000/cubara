//! Blocks that own state over time — currently just the furnace
//! (`docs/PHASE2_ARCHITECTURE.md` §7).
//!
//! # Where these live, and a deviation from §7
//!
//! §7 sketches block entities as a per-chunk side table, "so it loads, saves and
//! unloads with the chunk it belongs to and needs no separate lifetime rules".
//! They are stored here as a **world-level `BTreeMap` keyed by world position**
//! instead, alongside [`World::edits`](crate::World), because that is the
//! mechanism this codebase already uses for exactly this lifetime.
//!
//! The reasoning §7 gives is fully preserved, item for item:
//!
//! - **Sparse** — almost no block has one, and a `BTreeMap` stores only those
//!   that do.
//! - **Ordered** — iteration order feeds the world-state hash, the same reason
//!   the arena's draw list became a `BTreeMap` in issue #81.
//! - **No separate lifetime rules** — a furnace's state has precisely the
//!   lifetime of a player edit, which is what put the furnace there in the first
//!   place. A chunk with a furnace is by definition an edited chunk, so the two
//!   already load, save and unload together.
//!
//! Putting them on [`Chunk`](cubara_voxel::Chunk) instead would mean a second,
//! parallel overlay with the same lifetime as `edits` but a different owner —
//! and chunks are *regenerated* from the seed on load (§7.4), so a chunk is
//! precisely the thing that does not survive to carry player state.

use std::collections::BTreeMap;

use cubara_voxel::{ItemId, SmeltRecipe};

/// A furnace's contents and progress.
///
/// # The property block 2.7 depends on
///
/// §7 is explicit, and it is the one thing here that is expensive to recover
/// later:
///
/// > Furnace progress is a pure function of elapsed ticks and its slot
/// > contents. Nothing about it may depend on having been ticked one tick at a
/// > time.
///
/// [`Furnace::advance`] takes an **elapsed tick count**, not a "tick once"
/// call, and is written so that `advance(n)` equals `advance(1)` done `n`
/// times. `furnace_catch_up_matches_ticking_one_at_a_time` asserts exactly
/// that, over a range that crosses fuel exhaustion and several completed
/// smelts. That test is what block 2.7 (#58) will build dormant-chunk catch-up
/// on; it is cheap to hold now and expensive to reintroduce once something has
/// assumed otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Furnace {
    /// What is waiting to be smelted.
    pub input: Option<(ItemId, u8)>,
    /// What is waiting to be burned.
    pub fuel: Option<(ItemId, u8)>,
    /// What has been smelted and not yet taken.
    pub output: Option<(ItemId, u8)>,
    /// Ticks of burn left in the fuel currently alight. Zero means nothing is
    /// burning; the next tick will light a new item if there is one *and*
    /// something to smelt.
    pub burning: u32,
    /// Ticks of progress on the current item.
    pub progress: u32,
}

/// What one call to [`Furnace::advance`] consumed and produced, so the caller
/// can report it without re-deriving it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FurnaceOutcome {
    /// How many items finished smelting.
    pub smelted: u32,
    /// Whether anything changed at all — the cheap "does this need re-drawing"
    /// answer.
    pub changed: bool,
}

impl Furnace {
    /// Run `ticks` ticks of this furnace against `recipe` (whatever the input
    /// slot currently smelts into) and `fuel_ticks` (how long one of the fuel
    /// item burns).
    ///
    /// Deliberately a loop over ticks rather than a closed-form jump. The
    /// contract that matters for block 2.7 is that the *result* depends only on
    /// the elapsed count and the starting state — not that it is computed in
    /// O(1). A closed form here would have to special-case fuel running out
    /// mid-smelt, an item finishing on the same tick fuel is consumed, and the
    /// output stack filling up; each is a chance to disagree with the
    /// one-at-a-time path, which is the exact bug the property exists to
    /// prevent. Optimising this is block 2.7's problem, with its test already
    /// in place.
    pub fn advance(
        &mut self,
        ticks: u32,
        recipe: Option<SmeltRecipe>,
        fuel_ticks: impl Fn(ItemId) -> Option<u32>,
        max_stack: impl Fn(ItemId) -> u8,
    ) -> FurnaceOutcome {
        let mut out = FurnaceOutcome::default();
        for _ in 0..ticks {
            let before = *self;
            self.step(recipe, &fuel_ticks, &max_stack, &mut out);
            if *self != before {
                out.changed = true;
            }
        }
        out
    }

    /// One tick. The whole state machine, in the order the rules apply.
    fn step(
        &mut self,
        recipe: Option<SmeltRecipe>,
        fuel_ticks: &impl Fn(ItemId) -> Option<u32>,
        max_stack: &impl Fn(ItemId) -> u8,
        out: &mut FurnaceOutcome,
    ) {
        // Nothing smeltable in the input: burn down whatever is already alight
        // but never light a new item. Fuel is only spent on work -- §7's
        // furnace does not idle away a stack of logs while empty.
        let Some(recipe) = recipe else {
            self.burning = self.burning.saturating_sub(1);
            self.progress = 0;
            return;
        };
        if !self.output_has_room(recipe, max_stack) {
            // The output is full: stall, keeping progress, rather than
            // destroying the result. Fuel already alight still burns down --
            // it is lit, and un-lighting it would be a second rule.
            self.burning = self.burning.saturating_sub(1);
            return;
        }

        if self.burning == 0 {
            // Light one fuel item, if there is one. This is the only place fuel
            // is consumed, and it happens only when there is work to do.
            let Some((fuel_id, count)) = self.fuel else {
                self.progress = 0;
                return;
            };
            let Some(burn) = fuel_ticks(fuel_id) else {
                // Not fuel at all. Leave it; the UI should not have allowed it.
                self.progress = 0;
                return;
            };
            self.fuel = (count > 1).then_some((fuel_id, count - 1));
            self.burning = burn;
        }

        self.burning -= 1;
        self.progress += 1;
        if self.progress >= recipe.ticks {
            self.progress = 0;
            self.take_one_input();
            self.push_output(recipe);
            out.smelted += 1;
        }
    }

    fn output_has_room(&self, recipe: SmeltRecipe, max_stack: &impl Fn(ItemId) -> u8) -> bool {
        match self.output {
            None => true,
            Some((id, count)) => id == recipe.output && count + recipe.count <= max_stack(id),
        }
    }

    fn take_one_input(&mut self) {
        if let Some((id, count)) = self.input {
            self.input = (count > 1).then_some((id, count - 1));
        }
    }

    fn push_output(&mut self, recipe: SmeltRecipe) {
        self.output = match self.output {
            Some((id, count)) if id == recipe.output => Some((id, count + recipe.count)),
            _ => Some((recipe.output, recipe.count)),
        };
    }
}

/// Every block that owns state, by world position.
///
/// `BTreeMap` for the same reason [`World::edits`](crate::World) is one:
/// iteration order feeds the world-state hash, and a `HashMap` would make that
/// hash depend on hash seeding.
pub type BlockEntities = BTreeMap<[i32; 3], Furnace>;

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: ItemId = ItemId(1);
    const INGOT: ItemId = ItemId(2);
    const LOG: ItemId = ItemId(3);

    fn recipe() -> SmeltRecipe {
        SmeltRecipe {
            input: RAW,
            output: INGOT,
            count: 1,
            ticks: 10,
        }
    }

    fn fuel(id: ItemId) -> Option<u32> {
        (id == LOG).then_some(25)
    }

    fn max_stack(_: ItemId) -> u8 {
        64
    }

    fn loaded() -> Furnace {
        Furnace {
            input: Some((RAW, 3)),
            fuel: Some((LOG, 2)),
            ..Furnace::default()
        }
    }

    #[test]
    fn a_furnace_with_no_fuel_does_nothing() {
        let mut f = Furnace {
            input: Some((RAW, 3)),
            ..Furnace::default()
        };
        let out = f.advance(100, Some(recipe()), fuel, max_stack);
        assert_eq!(out.smelted, 0);
        assert_eq!(f.input, Some((RAW, 3)), "input untouched");
        assert_eq!(f.output, None);
    }

    #[test]
    fn fuel_is_spent_only_when_there_is_something_to_smelt() {
        // §7's furnace does not idle away a stack of logs while empty.
        let mut f = Furnace {
            fuel: Some((LOG, 2)),
            ..Furnace::default()
        };
        f.advance(500, None, fuel, max_stack);
        assert_eq!(f.fuel, Some((LOG, 2)), "no work, no burn");
    }

    #[test]
    fn smelting_consumes_input_and_produces_output() {
        let mut f = loaded();
        let out = f.advance(10, Some(recipe()), fuel, max_stack);
        assert_eq!(out.smelted, 1);
        assert_eq!(f.input, Some((RAW, 2)));
        assert_eq!(f.output, Some((INGOT, 1)));
    }

    #[test]
    fn one_fuel_item_burns_for_its_declared_ticks() {
        // A log burns 25 ticks and a smelt takes 10, so one log smelts two
        // items and leaves 5 ticks of burn behind.
        let mut f = Furnace {
            input: Some((RAW, 10)),
            fuel: Some((LOG, 1)),
            ..Furnace::default()
        };
        let out = f.advance(1000, Some(recipe()), fuel, max_stack);
        assert_eq!(out.smelted, 2);
        assert_eq!(f.fuel, None, "the one log is gone");
        assert_eq!(f.output, Some((INGOT, 2)));
    }

    #[test]
    fn a_full_output_stalls_rather_than_destroying_the_result() {
        let mut f = Furnace {
            input: Some((RAW, 5)),
            fuel: Some((LOG, 5)),
            output: Some((INGOT, 64)),
            ..Furnace::default()
        };
        let out = f.advance(100, Some(recipe()), fuel, max_stack);
        assert_eq!(out.smelted, 0);
        assert_eq!(f.output, Some((INGOT, 64)), "nothing lost");
        assert_eq!(f.input, Some((RAW, 5)), "and nothing consumed");
    }

    #[test]
    fn furnace_catch_up_matches_ticking_one_at_a_time() {
        // **The property block 2.7 is built on** (§7): progress is a pure
        // function of elapsed ticks and slot contents, so advancing N ticks in
        // one call must equal advancing one tick N times.
        //
        // The range is chosen to cross the interesting boundaries: a smelt
        // completing (10), fuel running out mid-item (25), and the input
        // emptying entirely.
        for n in 0..120 {
            let mut bulk = loaded();
            bulk.advance(n, Some(recipe()), fuel, max_stack);

            let mut one_at_a_time = loaded();
            for _ in 0..n {
                one_at_a_time.advance(1, Some(recipe()), fuel, max_stack);
            }

            assert_eq!(bulk, one_at_a_time, "diverged after {n} ticks");
        }
    }

    #[test]
    fn an_idle_furnace_loses_progress_but_not_its_lit_fuel_instantly() {
        // Input removed mid-smelt: progress resets, and the fuel already alight
        // burns down rather than being recovered.
        let mut f = Furnace {
            input: Some((RAW, 1)),
            fuel: Some((LOG, 1)),
            ..Furnace::default()
        };
        f.advance(5, Some(recipe()), fuel, max_stack);
        assert!(f.progress > 0 && f.burning > 0);
        let lit = f.burning;

        f.advance(1, None, fuel, max_stack);
        assert_eq!(f.progress, 0, "progress abandoned");
        assert_eq!(f.burning, lit - 1, "but the lit fuel keeps burning");
    }
}
