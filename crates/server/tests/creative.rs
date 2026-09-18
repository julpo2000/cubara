//! Creative mode (owner's call, 2026-09-18 -- `ROADMAP.md`'s phase 3 note):
//! `Action::SetCreative` grants unlimited blocks and instant, drop-free
//! mining, and `Action::Command("tp x y z")` teleports.
//!
//! Same lightweight fixture style as `untrusted.rs`'s mining-time tests
//! (empty sky, a fixed target rather than a raycast aimed with `look()`) --
//! these are server-mechanism tests, not a survival playthrough.

use cubara_server::{Action, Server};
use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs};
use cubara_voxel::{Angle, FixedVec3};

const SKY: i32 = 400;

/// One player in empty sky, looking down −Z (`Player::new`'s default look),
/// with the real shipped assets loaded.
fn fixture() -> (Server, PlayerId) {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-creative-fixture"));
    let who = s.sim.join(Player::new(
        FixedVec3::from_blocks(0, SKY, 0),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(who);
    (s, who)
}

#[test]
fn entering_creative_grants_one_of_every_placeable_block_into_empty_slots() {
    let (mut s, who) = fixture();
    s.apply_as(who, Action::SetCreative(true));

    assert!(s.sim.player(who).is_creative());
    let filled = s
        .sim
        .player(who)
        .inventory
        .slots()
        .filter(Option::is_some)
        .count();
    assert!(
        filled > 1,
        "expected more than one placeable block granted, got {filled}"
    );

    let cobble = s
        .items
        .as_ref()
        .expect("assets")
        .id_of("cubara:cobble")
        .expect("cobble is an item");
    let has_cobble = s
        .sim
        .player(who)
        .inventory
        .slots()
        .flatten()
        .any(|stack| stack.item() == cobble);
    assert!(
        has_cobble,
        "cobble is a placeable block and should be among what was granted"
    );
}

#[test]
fn entering_creative_does_not_touch_a_slot_that_already_has_something() {
    let (mut s, who) = fixture();
    let items = s.items.as_ref().expect("assets").clone();
    let oak_log = items.id_of("cubara:oak_log").expect("oak_log is an item");
    let stack = items.new_stack(oak_log, 3).expect("a stack of 3");
    s.sim.player_mut(who).inventory.set_slot(0, Some(stack));

    s.apply_as(who, Action::SetCreative(true));

    let slot0 = s.sim.player(who).inventory.slot(0).expect("still there");
    assert_eq!(
        slot0.item(),
        oak_log,
        "creative must not overwrite what survival already earned"
    );
    assert_eq!(slot0.count(), 3, "and must not touch its count either");
}

#[test]
fn creative_placing_does_not_consume_the_stack() {
    let (mut s, who) = fixture();
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");
    // 3 blocks down the default look direction (−Z), so `Action::Place`'s
    // raycast hits it with no extra aiming.
    s.set_block([0, SKY, -3], stone);

    let items = s.items.as_ref().expect("assets").clone();
    let cobble = items.id_of("cubara:cobble").expect("cobble is an item");
    let stack = items.new_stack(cobble, 5).expect("a stack of 5");
    s.sim.player_mut(who).inventory.set_slot(0, Some(stack));
    s.sim.player_mut(who).inventory.select(0);

    s.apply_as(who, Action::SetCreative(true));
    // `SetCreative` only fills *empty* slots, so slot 0's 5 cobble survive
    // the switch -- asserted here as the test's own precondition, not
    // assumed from the test above.
    assert_eq!(
        s.sim.player(who).inventory.slot(0).map(|s| s.count()),
        Some(5)
    );

    s.apply_as(who, Action::Place);

    assert_eq!(
        s.world.block_at(0, SKY, -2, s.terrain()),
        cobble_block(&s),
        "the block was placed"
    );
    assert_eq!(
        s.sim.player(who).inventory.slot(0).map(|s| s.count()),
        Some(5),
        "but creative never spends it"
    );
}

fn cobble_block(s: &Server) -> cubara_voxel::BlockId {
    s.blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:cobble"))
        .expect("cobble is a block")
}

#[test]
fn creative_breaking_is_instant_and_grants_no_drop() {
    let (mut s, who) = fixture();
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");
    let target = [0, SKY, -3];
    s.set_block(target, stone);
    s.apply_as(who, Action::SetCreative(true));

    // Entering creative already granted a stone stack of its own
    // (`entering_creative_grants_one_of_every_placeable_block_into_empty_slots`)
    // -- what this test asserts is that the *break* adds nothing on top of
    // that, so it compares the count before and after rather than just
    // presence.
    let stone_item = s
        .items
        .as_ref()
        .expect("assets")
        .id_of("cubara:stone")
        .expect("stone is an item");
    let count_of_stone = |s: &Server| -> u32 {
        s.sim
            .player(who)
            .inventory
            .slots()
            .flatten()
            .filter(|st| st.item() == stone_item)
            .map(|st| st.count() as u32)
            .sum()
    };
    let before = count_of_stone(&s);

    let holding = InputFrame {
        breaking: true,
        ..InputFrame::default()
    };
    // One tick -- stone's real hardness takes many more than this in
    // survival (`untrusted.rs`'s `a_block_is_not_broken_before_its_hardness_is_paid`
    // asserts exactly that number is > 2).
    s.tick_mining_all(&PlayerInputs::one(who, holding));

    assert_eq!(
        s.world
            .block_at(target[0], target[1], target[2], s.terrain()),
        cubara_voxel::BlockId::AIR,
        "creative breaks in one tick regardless of hardness"
    );
    assert_eq!(
        count_of_stone(&s),
        before,
        "creative doesn't need the drop -- it already has unlimited access"
    );
}

#[test]
fn tp_command_moves_the_player_and_clears_its_fall() {
    let (mut s, who) = fixture();
    s.sim.player_mut(who).velocity = FixedVec3::from_f32([1.0, -5.0, 2.0]);
    s.sim.player_mut(who).fall_distance = cubara_voxel::Fixed::from_blocks(40);

    s.apply_as(who, Action::Command("tp 10 64 -20".to_string()));

    let p = s.sim.player(who);
    assert_eq!(p.pos, FixedVec3::from_f32([10.0, 64.0, -20.0]));
    assert_eq!(p.velocity, FixedVec3::ZERO, "a teleport is not a fall");
    assert_eq!(p.fall_distance, cubara_voxel::Fixed::ZERO);
}

#[test]
fn a_malformed_tp_is_ignored_rather_than_panicking() {
    let (mut s, who) = fixture();
    let before = s.sim.player(who).pos;

    s.apply_as(who, Action::Command("tp not numbers".to_string()));
    assert_eq!(s.sim.player(who).pos, before);

    s.apply_as(who, Action::Command("tp 1 2".to_string()));
    assert_eq!(s.sim.player(who).pos, before);

    s.apply_as(who, Action::Command("nonsense".to_string()));
    assert_eq!(s.sim.player(who).pos, before);
}
