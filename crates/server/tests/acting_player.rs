//! An action is performed by the player who sent it.
//!
//! This looks too obvious to test, which is exactly why it was wrong. From
//! block 2.10 until this file existed, `Server::apply_as(who, action)` used
//! `who` only to address the `Open` effect back at the right screen; every
//! action underneath it read `self.local` — the server's *own* player. So a
//! second client breaking a block destroyed whatever the first player was
//! looking at, and the drop went into the first player's inventory.
//!
//! Nothing caught it, and one thing nearly did. During the LAN run for block
//! 2.12b a client on a second machine sent `Act(Break)` and got back
//! `Edit { pos: [2, 15, -5] }`. That was reported, by both sessions looking at
//! it, as proof the server had raycast from the sending client's pose. It was
//! the opposite: that client stood at yaw 0 / pitch 0 and could not have
//! produced a diagonal coordinate. `[2, 15, -5]` was the *local* player's
//! target, and the evidence presented as confirmation was the evidence of the
//! bug. "An `Edit` came back" had been read as "the right `Edit` came back".
//!
//! Hence the shape of every test here: two players far apart, each looking at a
//! block only they can reach, and an assertion about **which** one moved.
//! Distance is what makes the difference visible — the reason this survived so
//! long is that a joining client spawns exactly where the local player is
//! standing, where acting as the wrong player looks identical to acting as the
//! right one.

use cubara_server::{Action, FurnaceSlot, Server};
use cubara_sim::{Player, PlayerId};
use cubara_voxel::{Angle, BlockId, FixedVec3};

/// High above the terrain, so a raycast hits what the test put there and
/// nothing else. Asserted rather than assumed in [`pair`].
const SKY: i32 = 400;

/// How far apart the two players stand. Far enough that neither is within
/// `REACH` (6) of the other's block, so a mix-up cannot look like a near miss.
const APART: i32 = 80;

/// Two players in empty sky, `APART` blocks apart, each with a stone block
/// three ahead of them and nothing else in reach.
///
/// Returns the server, the two ids, and the block each is looking at.
fn pair() -> (Server, PlayerId, PlayerId, [i32; 3], [i32; 3], BlockId) {
    let mut s = Server::new();
    s.open(std::path::Path::new(
        "cubara-nonexistent-acting-player-fixture",
    ));

    let spawn = |x: i32| FixedVec3::from_blocks(x, SKY, 0);
    // Yaw 0 looks toward -Z, so "three ahead" is three blocks of -Z.
    let ahead = |x: i32| [x, SKY, -3];

    let a = s.sim.join(Player::new(spawn(0), Angle::ZERO, Angle::ZERO));
    let b = s
        .sim
        .join(Player::new(spawn(APART), Angle::ZERO, Angle::ZERO));
    s.open_view(a);
    s.open_view(b);

    let (block_a, block_b) = (ahead(0), ahead(APART));

    // The setup verifies itself: if the sky were not empty, a raycast would
    // find terrain and every assertion below would be about the wrong block.
    for who in [a, b] {
        let p = s.sim.player(who);
        assert!(
            s.world
                .raycast(
                    p.pos.to_f32(),
                    p.look_dir_f32().to_array(),
                    cubara_sim::REACH,
                    s.terrain()
                )
                .is_none(),
            "the sky at y={SKY} is not empty, so these tests would be aiming at terrain"
        );
    }

    // The registry's stone, not `BlockId::STONE`. Those are different numbers:
    // the constant is a fixed id from before there was a registry, and the
    // loaded registry assigns stone whatever its sorted position gives it. A
    // first draft of this file used the constant, which silently wrote some
    // *other* block into the world -- an interactive one, as it turned out, so
    // `Action::Place` took the "open a screen" branch and placed nothing. The
    // test failed for a reason that had nothing to do with what it was testing.
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");

    s.set_block(block_a, stone);
    s.set_block(block_b, stone);
    (s, a, b, block_a, block_b, stone)
}

fn block_at(s: &Server, at: [i32; 3]) -> BlockId {
    s.world.block_at(at[0], at[1], at[2], s.terrain())
}

/// A break destroys what its **sender** was looking at.
#[test]
fn a_break_destroys_what_its_sender_was_looking_at() {
    let (mut s, _a, b, block_a, block_b, stone) = pair();

    s.apply_as(b, Action::Break);

    assert_eq!(
        block_at(&s, block_b),
        BlockId::AIR,
        "the sender's block survived: the break was performed as somebody else"
    );
    assert_eq!(
        block_at(&s, block_a),
        stone,
        "a break sent by a player {APART} blocks away destroyed the other \
         player's block"
    );
}

/// The drop lands in the **sender's** inventory, and their tool is the one that
/// decides whether there is a drop at all.
///
/// Both players are given the same pickaxe, so the only thing that can explain
/// a difference is which of them the server acted as.
#[test]
fn a_break_fills_its_senders_inventory() {
    let (mut s, a, b, _block_a, _block_b, _stone) = pair();
    let items = s.items.as_ref().expect("assets loaded");
    let pick = items
        .id_of("cubara:wooden_pick")
        .expect("the wooden pick is an item");
    // Stone drops **cobble**, not stone (`assets/blocks/stone.ron`).
    let cobble = items.id_of("cubara:cobble").expect("cobble is an item");
    let tool = items.new_stack(pick, 1).expect("a pick");
    s.sim.player_mut(a).inventory.add(tool, items);
    s.sim.player_mut(b).inventory.add(tool, items);

    let cobble_held = |s: &Server, who: PlayerId| {
        (0..cubara_sim::SLOT_COUNT)
            .filter_map(|i| s.sim.player(who).inventory.slot(i))
            .filter(|st| st.item() == cobble)
            .count()
    };
    assert_eq!((cobble_held(&s, a), cobble_held(&s, b)), (0, 0));

    s.apply_as(b, Action::Break);

    assert_eq!(
        cobble_held(&s, b),
        1,
        "the sender broke a block and got nothing; the drop went elsewhere"
    );
    assert_eq!(
        cobble_held(&s, a),
        0,
        "a break by one player put an item in another player's inventory"
    );
}

/// A place spends the **sender's** item, and puts the block where they are
/// looking.
#[test]
fn a_place_spends_its_senders_item() {
    let (mut s, a, b, _block_a, block_b, stone) = pair();
    let items = s.items.as_ref().expect("assets loaded");
    let stone_item = items.id_of("cubara:stone").expect("stone is an item");
    let stack = items.new_stack(stone_item, 4).expect("a stack of stone");
    s.sim.player_mut(a).inventory.add(stack, items);
    s.sim.player_mut(b).inventory.add(stack, items);

    let held = |s: &Server, who: PlayerId| {
        s.sim
            .player(who)
            .inventory
            .selected_stack()
            .map(|st| st.count())
            .unwrap_or(0)
    };
    assert_eq!((held(&s, a), held(&s, b)), (4, 4));

    s.apply_as(b, Action::Place);

    assert_eq!(
        held(&s, b),
        3,
        "the sender's stack was untouched, so somebody else paid for the block"
    );
    assert_eq!(
        held(&s, a),
        4,
        "a place by one player spent another player's item"
    );
    // Placed against the face the sender was looking at, so one block nearer.
    let against = [block_b[0], block_b[1], block_b[2] + 1];
    assert_eq!(
        block_at(&s, against),
        stone,
        "the block did not land where the sender was aiming"
    );
}

/// Opening a bench widens the **sender's** crafting grid, which is world state
/// on their player rather than a property of the screen.
#[test]
fn an_interact_opens_on_its_senders_own_state() {
    let (mut s, a, b, _block_a, block_b, _stone) = pair();
    let bench = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:crafting_bench"))
        .expect("the bench is a block");
    s.set_block(block_b, bench);

    assert_eq!(s.sim.player(a).crafting.width(), 2);
    assert_eq!(s.sim.player(b).crafting.width(), 2);

    s.apply_as(b, Action::Interact);

    assert_eq!(
        s.sim.player(b).crafting.width(),
        3,
        "the sender opened a bench and their own grid stayed small"
    );
    assert_eq!(
        s.sim.player(a).crafting.width(),
        2,
        "one player opening a bench widened another player's crafting grid"
    );
}

/// A furnace click moves the **sender's** held item, not someone else's.
///
/// `ClickFurnace` is the one action that names its target, so it is also the
/// one where acting as the wrong player is invisible in the position: the
/// furnace is where the sender said, and the items came out of the wrong
/// pocket.
///
/// The two players hold **different** items on purpose. An earlier version gave
/// them the same one and could not fail: reading the wrong player's hand and
/// writing back to the right one puts an identical item in the furnace and
/// empties the correct hand, so every assertion passed while the bug was in
/// place. What the slot actually received is the only question that separates
/// the two.
#[test]
fn a_furnace_click_moves_its_senders_held_item() {
    let (mut s, a, b, _block_a, block_b, _stone) = pair();
    let items = s.items.as_ref().expect("assets loaded");
    let mine = items.id_of("cubara:oak_log").expect("oak log is an item");
    let theirs = items.id_of("cubara:plank").expect("plank is an item");
    let a_stack = items.new_stack(theirs, 1).expect("a stack");
    let b_stack = items.new_stack(mine, 1).expect("a stack");
    s.sim.player_mut(a).crafting.set_held(Some(a_stack));
    s.sim.player_mut(b).crafting.set_held(Some(b_stack));

    s.add_furnace(block_b);
    s.apply_as(
        b,
        Action::ClickFurnace {
            pos: block_b,
            slot: FurnaceSlot::Fuel,
        },
    );

    let fuel = s
        .world
        .furnace_at(block_b)
        .expect("the furnace is there")
        .fuel;
    assert_eq!(
        fuel.map(|(id, _)| id),
        Some(mine),
        "the furnace was fuelled from the wrong player's hand"
    );
    assert!(
        s.sim.player(b).crafting.held().is_none(),
        "the sender clicked a furnace and is still holding the item"
    );
    assert_eq!(
        s.sim.player(a).crafting.held().map(|st| st.item()),
        Some(theirs),
        "one player's furnace click disturbed another player's hand"
    );
}
