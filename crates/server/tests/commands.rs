//! The console's commands (owner's call, 2026-09-26 -- `ROADMAP.md`'s phase 3
//! note): `/tp`, `/give`, `/gamemode` and `/seed`, and the answer each sends
//! back to whoever typed it.
//!
//! The same fixture as `creative.rs`: empty sky, the real shipped assets.

use std::sync::Arc;

use cubara_server::{Action, Effect, Server};
use cubara_sim::{Player, PlayerId, SLOT_COUNT};
use cubara_voxel::{Angle, FixedVec3, ItemState};
use cubara_world::World;

const SKY: i32 = 400;

fn fixture() -> (Server, PlayerId) {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-commands-fixture"));
    let who = join(&mut s);
    (s, who)
}

fn join(s: &mut Server) -> PlayerId {
    let who = s.sim.join(Player::new(
        FixedVec3::from_blocks(0, SKY, 0),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(who);
    who
}

/// Type `text` as `who`, and return every answer `who` was sent.
fn run(s: &mut Server, who: PlayerId, text: &str) -> Vec<String> {
    s.drain_effects_for(who);
    s.apply_as(who, Action::Command(text.to_string()));
    s.drain_effects_for(who)
        .into_iter()
        .filter_map(|e| match e {
            Effect::CommandReply(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// How many of the item called `name` `who` is carrying, in all.
fn carried(s: &Server, who: PlayerId, name: &str) -> u32 {
    let items = s.items.as_ref().expect("assets loaded");
    let id = items.id_of(name).expect("a shipped item");
    s.sim
        .player(who)
        .inventory
        .slots()
        .flatten()
        .filter(|stack| stack.item() == id)
        .map(|stack| stack.count() as u32)
        .sum()
}

#[test]
fn seed_answers_with_this_worlds_seed_and_only_to_whoever_asked() {
    let (mut s, who) = fixture();
    let other = join(&mut s);
    // Not 0 or 1, which a hard-coded answer could hit by accident.
    s.world = Arc::new(World::with_seed(987_654_321));
    s.drain_effects_for(other);

    assert_eq!(run(&mut s, who, "seed"), ["seed: 987654321"]);
    assert!(
        !s.drain_effects_for(other)
            .iter()
            .any(|e| matches!(e, Effect::CommandReply(_))),
        "an answer is for the one who asked, not for everyone watching"
    );
}

#[test]
fn gamemode_switches_play_mode_the_way_the_pause_menu_does() {
    let (mut s, who) = fixture();

    assert_eq!(
        run(&mut s, who, "gamemode creative"),
        ["play mode: creative"]
    );
    assert!(s.sim.player(who).is_creative());
    assert!(
        s.sim
            .player(who)
            .inventory
            .slots()
            .any(|slot| slot.is_some()),
        "creative by command grants the loadout, as [C] does"
    );

    assert_eq!(
        run(&mut s, who, "gamemode survival"),
        ["play mode: survival"]
    );
    assert!(!s.sim.player(who).is_creative());
}

#[test]
fn a_gamemode_it_does_not_know_changes_nothing_and_says_how() {
    let (mut s, who) = fixture();
    for text in ["gamemode", "gamemode spectator"] {
        assert_eq!(
            run(&mut s, who, text),
            ["usage: /gamemode survival|creative"]
        );
        assert!(
            !s.sim.player(who).is_creative(),
            "{text:?} changed the mode"
        );
    }
}

#[test]
fn give_puts_the_items_in_the_inventory_across_stacks() {
    let (mut s, who) = fixture();

    // 70 is past one stack of 64, so the second stack is part of the test.
    assert_eq!(run(&mut s, who, "give stone 70"), ["gave 70 cubara:stone"]);
    assert_eq!(carried(&s, who, "cubara:stone"), 70);

    // The full name works too, and no count means one.
    assert_eq!(
        run(&mut s, who, "give cubara:stone"),
        ["gave 1 cubara:stone"]
    );
    assert_eq!(carried(&s, who, "cubara:stone"), 71);
}

#[test]
fn a_given_tool_is_a_fresh_one_per_slot() {
    let (mut s, who) = fixture();
    assert_eq!(
        run(&mut s, who, "give iron_pick 2"),
        ["gave 2 cubara:iron_pick"]
    );
    let picks: Vec<_> = s
        .sim
        .player(who)
        .inventory
        .slots()
        .flatten()
        .filter(|stack| stack.count() == 1)
        .collect();
    assert_eq!(
        picks.len(),
        2,
        "two tools are two slots, not a stack of two"
    );
    for pick in picks {
        assert_eq!(pick.state(), ItemState::Durability { remaining: 400 });
    }
}

#[test]
fn give_stops_at_a_full_inventory_and_says_how_many_fitted() {
    let (mut s, who) = fixture();
    let room = SLOT_COUNT as u32 * 64;
    let reply = run(&mut s, who, "give stone 999999");
    assert_eq!(
        reply,
        [format!(
            "gave {room} of 999999 cubara:stone -- the inventory is full"
        )]
    );
    assert_eq!(carried(&s, who, "cubara:stone"), room);
}

#[test]
fn give_with_a_bad_item_or_count_gives_nothing_and_says_why() {
    let (mut s, who) = fixture();
    assert_eq!(
        run(&mut s, who, "give unobtainium"),
        ["no item called unobtainium"]
    );
    for text in ["give", "give stone 0", "give stone -3", "give stone lots"] {
        assert_eq!(
            run(&mut s, who, text),
            ["usage: /give <item> [count]"],
            "{text:?}"
        );
    }
    assert!(
        s.sim
            .player(who)
            .inventory
            .slots()
            .all(|slot| slot.is_none()),
        "nothing was given"
    );
}

#[test]
fn a_malformed_tp_says_how_rather_than_nothing() {
    let (mut s, who) = fixture();
    let before = s.sim.player(who).pos;
    assert_eq!(run(&mut s, who, "tp 1 2"), ["usage: /tp x y z"]);
    assert_eq!(s.sim.player(who).pos, before);
    // A good one moves, and has nothing to add.
    assert!(run(&mut s, who, "tp 1 2 3").is_empty());
    assert_ne!(s.sim.player(who).pos, before);
}

#[test]
fn time_and_unknown_commands_answer_rather_than_vanish() {
    let (mut s, who) = fixture();
    assert_eq!(
        run(&mut s, who, "time set day"),
        ["/time: the world has no time of day yet"]
    );
    assert_eq!(
        run(&mut s, who, "fly"),
        ["unknown command /fly -- try /tp, /give, /gamemode or /seed"]
    );
}
