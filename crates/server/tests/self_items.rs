//! A client learns what it is carrying (block 2.12b).
//!
//! Driven through `Session::attach`, so delivery is a fact rather than a race —
//! and through the same `welcome` path a socket takes, so nothing here is true
//! only because it is in-process.

use cubara_server::headless::{Config, Session};
use cubara_server::wire::{ClientMessage, ServerMessage};
use cubara_server::Effect;

fn session() -> (Session, Config) {
    let cfg = Config {
        world: std::path::PathBuf::from("cubara-nonexistent-self-items-fixture"),
        autosave_ticks: 0,
        ..Config::default()
    };
    let s = Session::open(&cfg);
    (s, cfg)
}

fn items_in(messages: &[ServerMessage]) -> Vec<cubara_server::ClientItems> {
    messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Effects(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|e| match e {
            Effect::SelfItems(items) => Some(*items),
            _ => None,
        })
        .collect()
}

/// A joining client is told what it is holding, without having to do anything.
///
/// Without this a remote client's hotbar is empty until it happens to pick
/// something up — and since a fresh player holds nothing, that could go
/// unnoticed for a long time in exactly the wrong way.
#[test]
fn a_client_is_told_its_items_when_it_joins() {
    let (mut s, cfg) = session();
    let mut link = s.attach();
    link.send(ClientMessage::Hello);
    s.advance(1, &cfg);

    let got = items_in(&link.poll());
    assert_eq!(
        got.len(),
        1,
        "a joining client was sent {} inventories; it should be told exactly once",
        got.len()
    );
    assert_eq!(
        got[0].inventory.len(),
        cubara_sim::SLOT_COUNT,
        "the inventory that arrived is not the right shape"
    );
}

/// It is told again when something changes, and **not** when nothing does.
///
/// The second half is the one that matters for a world of five thousand people:
/// an inventory is large, and re-sending it every tick would cost more than
/// everything else this protocol carries put together.
#[test]
fn items_are_sent_on_change_and_not_otherwise() {
    let (mut s, cfg) = session();
    let mut link = s.attach();
    link.send(ClientMessage::Hello);
    s.advance(1, &cfg);
    // The id from the welcome, which is how a client actually learns it -- not
    // from poking at the server's player list, which would happen to be right
    // for the wrong reason.
    let messages = link.poll();
    let who = messages
        .iter()
        .find_map(|m| match m {
            ServerMessage::Welcome { you, .. } => Some(*you),
            _ => None,
        })
        .expect("a welcome");

    // Nothing happens.
    for _ in 0..10 {
        s.advance(1, &cfg);
    }
    let quiet = items_in(&link.poll());
    assert!(
        quiet.is_empty(),
        "an unchanged inventory was re-sent {} time(s) in ten idle ticks",
        quiet.len()
    );

    // Something happens.
    let stack = s
        .server
        .items
        .as_ref()
        .and_then(|r| r.id_of("cubara:stone").map(|id| r.new_stack(id, 5)))
        .expect("the shipped assets have stone")
        .expect("five stone is a stack");
    s.server
        .sim
        .player_mut(who)
        .inventory
        .set_slot(0, Some(stack));
    s.advance(1, &cfg);

    let after = items_in(&link.poll());
    assert_eq!(
        after.len(),
        1,
        "a changed inventory was sent {} time(s); expected exactly once",
        after.len()
    );
    assert_eq!(
        after[0].inventory[0].map(|s| s.count()),
        Some(5),
        "the inventory that arrived is not the one the server has"
    );
}
