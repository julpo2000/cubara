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

/// A process whose state after `n` ticks depends only on `n` and its starting
/// state (`PHASE2_ARCHITECTURE.md` §12.3).
///
/// **The contract is the whole point:** `advance(n)` must equal `advance(1)`
/// done `n` times. A process that cannot honour that is not
/// time-parameterizable, and dormancy would change its answer -- so it does not
/// belong behind this trait.
pub trait TimedProcess {
    /// Everything the process needs that is not its own state. Plain data, not
    /// closures, so a test can build one without a registry.
    type Ctx;

    /// Advance by `ticks`, in time bounded by the *work available* rather than
    /// by `ticks` (§12.1).
    fn advance(&mut self, ticks: u64, ctx: &Self::Ctx) -> FurnaceOutcome;
}

/// What a furnace needs to know that is not its own state.
///
/// Resolved to plain numbers by the caller: a furnace only ever asks about the
/// one item in its fuel slot and the one its recipe outputs, so there is nothing
/// here that needs a registry lookup at tick time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SmeltCtx {
    /// What the input slot currently smelts into, if anything.
    pub recipe: Option<SmeltRecipe>,
    /// How long one item from the fuel slot burns. `None` if it is not fuel.
    pub fuel_burn: Option<u32>,
    /// Stack limit of the recipe's output item.
    pub output_max: u8,
}

impl TimedProcess for Furnace {
    type Ctx = SmeltCtx;

    /// Catch up by `ticks`, jumping from event to event (§12.1).
    ///
    /// Every iteration either completes one item or consumes one fuel unit, so
    /// the loop runs at most `input + fuel` times -- **bounded by the stack
    /// sizes involved, never by `ticks`.** A chunk dormant for a million ticks
    /// costs the same as one dormant for a thousand.
    ///
    /// Checked against [`Furnace::advance_one_by_one`], which is the
    /// specification (§12.2).
    fn advance(&mut self, ticks: u64, ctx: &SmeltCtx) -> FurnaceOutcome {
        let mut out = FurnaceOutcome::default();
        let before = *self;
        let mut left = ticks;

        while left > 0 {
            // Stall cases end the whole catch-up in one step: nothing further
            // can happen no matter how long we wait, except lit fuel burning
            // down. Each mirrors the matching early return in `step`.
            let Some(recipe) = ctx.recipe.filter(|_| self.input.is_some()) else {
                self.burning = self
                    .burning
                    .saturating_sub(left.min(u32::MAX as u64) as u32);
                self.progress = 0;
                break;
            };
            if !self.output_has_room_for(recipe, ctx.output_max) {
                // Progress is kept, unlike the no-recipe case -- `step` keeps it
                // too.
                self.burning = self
                    .burning
                    .saturating_sub(left.min(u32::MAX as u64) as u32);
                break;
            }
            if self.burning == 0 {
                // Lighting is free (same tick in `step`), but needs fuel.
                let Some((fuel_id, count)) = self.fuel else {
                    self.progress = 0;
                    break;
                };
                let Some(burn) = ctx.fuel_burn else {
                    self.progress = 0;
                    break;
                };
                self.fuel = (count > 1).then_some((fuel_id, count - 1));
                self.burning = burn;
                if burn == 0 {
                    // A zero-burn fuel would spin forever; treat it as not fuel.
                    self.progress = 0;
                    break;
                }
            }

            // Run until the nearer of: this item finishing, or the fire going
            // out. Never past `left`.
            let to_finish = (recipe.ticks - self.progress) as u64;
            let run = to_finish.min(self.burning as u64).min(left);
            self.progress += run as u32;
            self.burning -= run as u32;
            left -= run;

            if self.progress >= recipe.ticks {
                self.progress = 0;
                self.take_one_input();
                self.push_output(recipe);
                out.smelted += 1;
            }
        }

        out.changed = *self != before;
        out
    }
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
    pub fn advance_one_by_one(
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
        //
        // **`input.is_some()` is part of this check, and was missing until
        // block 2.7a.** Without it, a caller passing a recipe while the input
        // slot is empty would burn fuel and push output *from nothing*: the
        // `take_one_input` below is a no-op on an empty slot while
        // `push_output` still fires. Nothing reached it -- `Game` resolves the
        // recipe *from* the input, so it passes `None` when the slot is empty --
        // but the guard was the caller's, not this function's, and block 2.7a's
        // context carries the recipe as data where that is easy to get wrong.
        // Found by `bounded_catch_up_agrees_with_the_one_tick_reference`, which
        // is exactly the kind of latent bug keeping the reference path is for.
        let Some(recipe) = recipe.filter(|_| self.input.is_some()) else {
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
            Some((id, _)) => self.output_has_room_for(recipe, max_stack(id)),
        }
    }

    /// The same rule against an already-resolved stack limit -- what the bounded
    /// path uses, so the two cannot disagree about what "full" means.
    fn output_has_room_for(&self, recipe: SmeltRecipe, max_stack: u8) -> bool {
        match self.output {
            None => true,
            Some((id, count)) => id == recipe.output && count + recipe.count <= max_stack,
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
        let out = f.advance_one_by_one(100, Some(recipe()), fuel, max_stack);
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
        f.advance_one_by_one(500, None, fuel, max_stack);
        assert_eq!(f.fuel, Some((LOG, 2)), "no work, no burn");
    }

    #[test]
    fn smelting_consumes_input_and_produces_output() {
        let mut f = loaded();
        let out = f.advance_one_by_one(10, Some(recipe()), fuel, max_stack);
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
        let out = f.advance_one_by_one(1000, Some(recipe()), fuel, max_stack);
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
        let out = f.advance_one_by_one(100, Some(recipe()), fuel, max_stack);
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
            bulk.advance_one_by_one(n, Some(recipe()), fuel, max_stack);

            let mut one_at_a_time = loaded();
            for _ in 0..n {
                one_at_a_time.advance_one_by_one(1, Some(recipe()), fuel, max_stack);
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
        f.advance_one_by_one(5, Some(recipe()), fuel, max_stack);
        assert!(f.progress > 0 && f.burning > 0);
        let lit = f.burning;

        f.advance_one_by_one(1, None, fuel, max_stack);
        assert_eq!(f.progress, 0, "progress abandoned");
        assert_eq!(f.burning, lit - 1, "but the lit fuel keeps burning");
    }

    fn ctx() -> SmeltCtx {
        SmeltCtx {
            recipe: Some(recipe()),
            fuel_burn: Some(25),
            output_max: 64,
        }
    }

    #[test]
    fn bounded_catch_up_agrees_with_the_one_tick_reference() {
        // **The safety argument for the whole block** (§12.2). The bounded path
        // is fast and subtle; `advance_one_by_one` is slow and obviously
        // correct. Running both over many starting states and many elapsed
        // counts, and asserting they agree, is what makes the fast one
        // trustworthy.
        //
        // The states are chosen to sit on and around every boundary the bounded
        // path jumps between: mid-item, mid-fuel, empty fuel, empty input, and
        // a nearly-full output.
        let states = [
            loaded(),
            Furnace {
                input: Some((RAW, 1)),
                fuel: Some((LOG, 1)),
                ..Furnace::default()
            },
            Furnace {
                input: Some((RAW, 5)),
                fuel: Some((LOG, 3)),
                burning: 7,
                progress: 9,
                ..Furnace::default()
            },
            Furnace {
                input: Some((RAW, 2)),
                fuel: None,
                burning: 4,
                progress: 3,
                ..Furnace::default()
            },
            Furnace {
                input: None,
                fuel: Some((LOG, 2)),
                burning: 10,
                ..Furnace::default()
            },
            Furnace {
                input: Some((RAW, 4)),
                fuel: Some((LOG, 4)),
                output: Some((INGOT, 63)),
                ..Furnace::default()
            },
            Furnace {
                input: Some((RAW, 4)),
                fuel: Some((LOG, 4)),
                output: Some((INGOT, 64)),
                ..Furnace::default()
            },
        ];
        let c = ctx();
        for (i, start) in states.iter().enumerate() {
            for n in [0u64, 1, 2, 9, 10, 11, 24, 25, 26, 49, 50, 137, 500, 5_000] {
                let mut fast = *start;
                fast.advance(n, &c);

                let mut slow = *start;
                slow.advance_one_by_one(n as u32, c.recipe, fuel, max_stack);

                assert_eq!(
                    fast, slow,
                    "state {i} diverged after {n} ticks\n  fast: {fast:?}\n  slow: {slow:?}"
                );
            }
        }
    }

    #[test]
    fn catch_up_cost_does_not_grow_with_elapsed_time() {
        // §12.1's actual promise. The bounded path's iteration count is capped
        // by the work available, so a furnace with one item and one fuel must
        // reach the same end state whether it slept 1,000 ticks or 100 million
        // -- and the huge one must not take meaningfully longer.
        let c = ctx();
        let start = Furnace {
            input: Some((RAW, 1)),
            fuel: Some((LOG, 1)),
            ..Furnace::default()
        };

        let mut short = start;
        short.advance(1_000, &c);
        let mut astronomical = start;
        astronomical.advance(100_000_000, &c);

        assert_eq!(
            short, astronomical,
            "a furnace out of work ends in the same place however long it waits"
        );
        // If this were still per-tick, the call above would run a hundred
        // million iterations; the test finishing at all is the evidence.
    }

    #[test]
    fn a_zero_burn_fuel_cannot_spin_forever() {
        // A data file declaring `burn_ticks: Some(0)` would make the
        // light-fuel/consume-fuel cycle make no progress. The one-tick path
        // cannot loop (it does one tick and returns); the bounded path could,
        // so it stops instead.
        let c = SmeltCtx {
            fuel_burn: Some(0),
            ..ctx()
        };
        let mut f = loaded();
        f.advance(1_000_000, &c);
        assert_eq!(f.progress, 0, "it stalled rather than hanging");
    }
}
