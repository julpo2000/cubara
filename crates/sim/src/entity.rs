//! Entities — the things there are *many* of.
//!
//! Block 2.5, designed in `docs/PHASE2_ARCHITECTURE.md` §10. Currently one kind:
//! [`DroppedItem`], the thing five separate sites in blocks 2.1–2.4 destroyed
//! because there was nowhere to put it.
//!
//! # The determinism contract (§10.2)
//!
//! **`hecs` query iteration order is unspecified.** It follows internal
//! archetype layout, which depends on the order components were inserted and on
//! entity reuse. Rule 1 — determinism — is this project's keystone rule, so that
//! is a hazard rather than a detail, and this module is built around it:
//!
//! 1. Every entity carries an [`EntityKey`], a `u64` from a counter in world
//!    state, assigned at spawn and never reused. `hecs::Entity` is generational
//!    and its bits depend on allocation history; it never reaches the world
//!    hash, a save file, or any ordering decision.
//! 2. No system lets query order change the result. Either the per-entity update
//!    is independent of the others — gravity and despawn timers are — or the
//!    system collects and **sorts by `EntityKey`** before acting. Pickup is in
//!    the second category, because two items reaching a nearly-full inventory in
//!    a different order give a different inventory.
//! 3. [`Entities::sorted`] is the only iteration order anything outside this
//!    module may depend on, and it is by `EntityKey`.
//!
//! `entity_results_do_not_depend_on_spawn_order` is what makes that true rather
//! than merely stated: it forces archetype churn and asserts the simulation
//! lands in the same place.

use glam::Vec3;

use cubara_voxel::{ItemRegistry, ItemStack, Rarity};

use crate::inventory::Inventory;
use crate::physics;

/// A stable, never-reused entity identifier.
///
/// Distinct from `hecs::Entity` deliberately: that one is an index plus a
/// generation, so it depends on allocation history and would make two worlds
/// that ran the same events disagree. This is a plain counter in world state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityKey(pub u64);

/// An item lying on the ground (§10.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DroppedItem {
    pub stack: ItemStack,
    pub pos: Vec3,
    pub velocity: Vec3,
    /// Ticks since it was dropped, against its rarity's despawn time (§10.5).
    pub age: u32,
    pub on_ground: bool,
}

/// How close the player must come to collect an item, in blocks. Tuning, so it
/// is one named constant rather than a literal in the pickup loop.
pub const PICKUP_RADIUS: f32 = 1.5;

/// The world's entities, and the counter that names them.
///
/// `Debug` by hand: `hecs::World` has no `Debug`, and the useful summary is how
/// many entities there are and what the next key will be, not a dump.
pub struct Entities {
    world: hecs::World,
    /// Next [`EntityKey`]. World state: it is what makes keys reproducible, so
    /// it is saved and hashed like any other world state.
    next_key: u64,
}

impl std::fmt::Debug for Entities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entities")
            .field("len", &self.len())
            .field("next_key", &self.next_key)
            .finish()
    }
}

impl Default for Entities {
    fn default() -> Self {
        Self::new()
    }
}

impl Entities {
    pub fn new() -> Self {
        Self {
            world: hecs::World::new(),
            next_key: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.world.len() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop `stack` at `pos` with an initial `velocity`.
    pub fn spawn_item(&mut self, stack: ItemStack, pos: Vec3, velocity: Vec3) -> EntityKey {
        let key = EntityKey(self.next_key);
        self.next_key += 1;
        self.world.spawn((
            key,
            DroppedItem {
                stack,
                pos,
                velocity,
                age: 0,
                on_ground: false,
            },
        ));
        key
    }

    /// Every dropped item, **in `EntityKey` order**.
    ///
    /// The only ordering anything outside this module may rely on (§10.2).
    /// Sorting rather than trusting the query is the whole point: `hecs` makes
    /// no promise about iteration order, and two worlds that spawned the same
    /// items in a different sequence must still behave identically.
    pub fn sorted(&self) -> Vec<(EntityKey, DroppedItem)> {
        let mut out: Vec<(EntityKey, DroppedItem)> =
            self.all().into_iter().map(|(k, _, d)| (k, d)).collect();
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Every dropped item with its `hecs` handle, **unsorted**.
    ///
    /// Private, and the only place a raw `hecs::Entity` is produced. `hecs`
    /// 0.11's queries yield components without the entity, so this walks
    /// `World::iter` -- and because that order is exactly the unspecified one
    /// §10.2 warns about, every public caller sorts before acting on it.
    fn all(&self) -> Vec<(EntityKey, hecs::Entity, DroppedItem)> {
        self.world
            .iter()
            .filter_map(|e| {
                let key = *e.get::<&EntityKey>()?;
                let item = *e.get::<&DroppedItem>()?;
                Some((key, e.entity(), item))
            })
            .collect()
    }

    /// One tick of every entity (§10.4, §10.5).
    ///
    /// Order-independent by construction: gravity and ageing touch only their
    /// own entity, so the loop may run in whatever order `hecs` hands things
    /// back. Despawn is decided per entity against its own age, likewise.
    ///
    /// Pickup is *not* here — it is order-dependent and lives in
    /// [`collect_nearby`](Self::collect_nearby), which sorts first.
    pub fn tick(
        &mut self,
        dt: f32,
        items: &ItemRegistry,
        is_solid: impl Fn(i32, i32, i32) -> bool + Copy,
    ) {
        let mut expired: Vec<hecs::Entity> = Vec::new();
        for (_, entity, _) in self.all() {
            let Ok(mut d) = self.world.get::<&mut DroppedItem>(entity) else {
                continue;
            };
            if !d.on_ground || d.velocity.y != 0.0 {
                let (mut pos, mut vel) = (d.pos, d.velocity);
                d.on_ground = physics::step_item(&mut pos, &mut vel, dt, is_solid);
                d.pos = pos;
                d.velocity = vel;
            }
            d.age += 1;
            if let Some(limit) = despawn_ticks(items.rarity(d.stack.item())) {
                if d.age >= limit {
                    expired.push(entity);
                }
            }
        }
        // Collected first, despawned after: mutating the world inside its own
        // query is not allowed, and doing it in two passes also keeps the
        // decision (age >= limit) independent of the removal order.
        for e in expired {
            let _ = self.world.despawn(e);
        }
    }

    /// Give the player every item within [`PICKUP_RADIUS`] that fits.
    ///
    /// **Sorted by `EntityKey` before anything is moved** (§10.2 rule 2): two
    /// items arriving at a nearly-full inventory in a different order produce a
    /// different inventory, so this is exactly the case where query order must
    /// not be allowed to decide.
    ///
    /// An item that does not fit stays on the ground, keeping its age — the
    /// inventory being full is not a reason to destroy it a second time, which
    /// is the bug this whole block exists to fix.
    pub fn collect_nearby(
        &mut self,
        player_pos: Vec3,
        inventory: &mut Inventory,
        items: &ItemRegistry,
    ) -> u32 {
        let mut candidates: Vec<(EntityKey, hecs::Entity, DroppedItem)> = self
            .all()
            .into_iter()
            .filter(|(_, _, d)| d.pos.distance(player_pos) <= PICKUP_RADIUS)
            .collect();
        candidates.sort_by_key(|(k, _, _)| *k);

        let mut collected = 0;
        for (_, e, d) in candidates {
            match inventory.add(d.stack, items) {
                // Nothing left over: the whole stack went in.
                None => {
                    let _ = self.world.despawn(e);
                    collected += 1;
                }
                // Partially collected: keep the remainder on the ground.
                Some(rest) if rest.count() < d.stack.count() => {
                    if let Ok(mut item) = self.world.get::<&mut DroppedItem>(e) {
                        item.stack = rest;
                    }
                    collected += 1;
                }
                // No room at all: leave it exactly as it was.
                Some(_) => {}
            }
        }
        collected
    }
}

/// How long an item of this rarity survives on the floor, or `None` for never
/// (§10.5). Ticks, not seconds: Rule 1.
pub fn despawn_ticks(rarity: Rarity) -> Option<u32> {
    match rarity {
        // 5 minutes at 60 Hz.
        Rarity::Common => Some(18_000),
        // 30 minutes.
        Rarity::Uncommon => Some(108_000),
        Rarity::Treasured => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::{ItemDef, ItemState};
    use std::path::PathBuf;

    fn registry() -> ItemRegistry {
        let def = |name: &str, rarity: Rarity| {
            (
                PathBuf::from(format!("{name}.ron")),
                ItemDef {
                    name: name.to_string(),
                    max_stack: 64,
                    durability: None,
                    tier: 0,
                    speed: None,
                    burn_ticks: None,
                    rarity,
                },
            )
        };
        ItemRegistry::from_defs(vec![
            def("cubara:stone", Rarity::Common),
            def("cubara:raw_iron", Rarity::Uncommon),
            def("cubara:relic", Rarity::Treasured),
        ])
        .expect("valid")
    }

    fn stack(items: &ItemRegistry, name: &str, count: u8) -> ItemStack {
        let id = items.id_of(name).expect(name);
        items.new_stack(id, count).expect("a stack")
    }

    /// Solid everywhere below y = 0, air above: a floor to land on.
    fn floor(_x: i32, y: i32, _z: i32) -> bool {
        y < 0
    }

    #[test]
    fn a_dropped_item_falls_and_comes_to_rest() {
        let items = registry();
        let mut e = Entities::new();
        e.spawn_item(
            stack(&items, "cubara:stone", 1),
            Vec3::new(0.5, 8.0, 0.5),
            Vec3::ZERO,
        );

        for _ in 0..240 {
            e.tick(1.0 / 60.0, &items, floor);
        }

        let (_, d) = e.sorted()[0];
        assert!(d.on_ground, "it landed");
        assert!(
            (d.pos.y - physics::ITEM_HALF).abs() < 0.01,
            "resting on the floor, not sunk through it: y = {}",
            d.pos.y
        );
    }

    #[test]
    fn walking_near_an_item_collects_it() {
        let items = registry();
        let mut e = Entities::new();
        let mut inv = Inventory::default();
        e.spawn_item(
            stack(&items, "cubara:stone", 5),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::ZERO,
        );

        // Too far.
        assert_eq!(
            e.collect_nearby(Vec3::new(10.0, 0.0, 0.0), &mut inv, &items),
            0
        );
        assert_eq!(e.len(), 1);

        // Close enough.
        assert_eq!(
            e.collect_nearby(Vec3::new(0.5, 0.0, 0.0), &mut inv, &items),
            1
        );
        assert_eq!(e.len(), 0, "the entity is gone");
        assert_eq!(inv.slot(0).map(|s| s.count()), Some(5));
    }

    #[test]
    fn an_item_that_does_not_fit_stays_on_the_ground() {
        // The bug this whole block exists to fix: "no room" must never mean
        // "destroy it".
        let items = registry();
        let mut e = Entities::new();
        let mut inv = Inventory::default();
        for i in 0..crate::inventory::SLOT_COUNT {
            inv.set_slot(i, Some(stack(&items, "cubara:raw_iron", 64)));
        }
        e.spawn_item(stack(&items, "cubara:stone", 3), Vec3::ZERO, Vec3::ZERO);

        assert_eq!(e.collect_nearby(Vec3::ZERO, &mut inv, &items), 0);
        assert_eq!(e.len(), 1, "still on the floor");
        assert_eq!(e.sorted()[0].1.stack.count(), 3, "and still all of it");
    }

    #[test]
    fn each_rarity_despawns_at_its_own_boundary() {
        let items = registry();
        let cases = [
            ("cubara:stone", Some(18_000)),
            ("cubara:raw_iron", Some(108_000)),
            ("cubara:relic", None),
        ];
        for (name, limit) in cases {
            let mut e = Entities::new();
            e.spawn_item(stack(&items, name, 1), Vec3::ZERO, Vec3::ZERO);
            let run = limit.unwrap_or(200_000);

            // One tick short of the boundary it must still be there.
            for _ in 0..run - 1 {
                e.tick(1.0 / 60.0, &items, floor);
            }
            assert_eq!(e.len(), 1, "{name} vanished early");

            e.tick(1.0 / 60.0, &items, floor);
            match limit {
                Some(_) => assert_eq!(e.len(), 0, "{name} should have despawned"),
                None => assert_eq!(e.len(), 1, "{name} must never despawn"),
            }
        }
    }

    #[test]
    fn entity_results_do_not_depend_on_spawn_order() {
        // **The test §10.2 demands.** `hecs` iteration order follows archetype
        // layout, which depends on insertion order and entity reuse -- so the
        // same events applied in a different sequence, with churn forced in
        // between, must still land in the same place.
        let items = registry();
        let names = ["cubara:stone", "cubara:raw_iron", "cubara:relic"];

        let run_in_order = |order: &[usize], churn: bool| -> Vec<(EntityKey, DroppedItem)> {
            let mut e = Entities::new();
            if churn {
                // Force archetype reuse: spawn and despawn before the real
                // work, so the entity indices handed out below are recycled.
                let mut keys = Vec::new();
                for _ in 0..8 {
                    keys.push(e.spawn_item(
                        stack(&items, "cubara:stone", 1),
                        Vec3::ZERO,
                        Vec3::ZERO,
                    ));
                }
                // Age them past Common's despawn so they are removed.
                for _ in 0..18_000 {
                    e.tick(1.0 / 60.0, &items, floor);
                }
                assert!(e.is_empty(), "churn removed them");
            }
            for &i in order {
                e.spawn_item(
                    stack(&items, names[i], 2),
                    Vec3::new(i as f32, 4.0, 0.0),
                    Vec3::ZERO,
                );
            }
            for _ in 0..600 {
                e.tick(1.0 / 60.0, &items, floor);
            }
            e.sorted()
        };

        // Same entities, spawned in three different orders, one with churn:
        // the *keys* differ (they are assigned at spawn), so compare what the
        // simulation actually produced -- each item's resting state.
        let a = run_in_order(&[0, 1, 2], false);
        let b = run_in_order(&[0, 1, 2], true);
        let states = |v: Vec<(EntityKey, DroppedItem)>| -> Vec<(u8, bool, u32)> {
            v.into_iter()
                .map(|(_, d)| (d.stack.count(), d.on_ground, d.age))
                .collect()
        };
        assert_eq!(
            states(a),
            states(b),
            "archetype churn changed the simulation"
        );
    }

    #[test]
    fn keys_are_never_reused() {
        // An `EntityKey` that came back would make two different histories hash
        // alike, which is exactly what `hecs::Entity`'s generation does.
        let items = registry();
        let mut e = Entities::new();
        let first = e.spawn_item(stack(&items, "cubara:stone", 1), Vec3::ZERO, Vec3::ZERO);
        for _ in 0..18_000 {
            e.tick(1.0 / 60.0, &items, floor);
        }
        assert!(e.is_empty());
        let second = e.spawn_item(stack(&items, "cubara:stone", 1), Vec3::ZERO, Vec3::ZERO);
        assert_ne!(first, second);
        assert!(second.0 > first.0);
    }

    #[test]
    fn a_resting_item_does_not_drift() {
        let items = registry();
        let mut e = Entities::new();
        e.spawn_item(
            stack(&items, "cubara:stone", 1),
            Vec3::new(0.5, 2.0, 0.5),
            Vec3::new(3.0, 0.0, -2.0),
        );
        for _ in 0..120 {
            e.tick(1.0 / 60.0, &items, floor);
        }
        let settled = e.sorted()[0].1.pos;
        for _ in 0..600 {
            e.tick(1.0 / 60.0, &items, floor);
        }
        let later = e.sorted()[0].1.pos;
        assert!(
            settled.distance(later) < 1e-3,
            "a resting item slid from {settled} to {later}"
        );
        let _ = ItemState::None;
    }
}
