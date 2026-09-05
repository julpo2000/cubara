//! Joining a server: checking that the far end means the same thing we do.
//!
//! Block 2.12b. `Welcome` has carried registry fingerprints since the
//! correction channel landed, and until this module **nobody looked at them** —
//! a safety check that is sent and never verified, which is the shape that looks
//! finished while doing nothing.

use cubara_sim::PlayerId;
use cubara_voxel::{BlockRegistry, ItemRegistry};

use crate::wire::ServerMessage;

/// What a client learns by joining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Joined {
    pub seed: u64,
    pub you: PlayerId,
}

/// Why a join was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinError {
    /// The first message was not a `Welcome`.
    NotAWelcome,
    /// The server's assets are not ours.
    Mismatch {
        /// Which registry, so the message can say. "handshake failed" costs
        /// somebody an evening; "your items differ from the server's" costs a
        /// minute.
        which: &'static str,
        ours: u64,
        theirs: u64,
    },
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::NotAWelcome => write!(f, "the server did not open with a welcome"),
            JoinError::Mismatch {
                which,
                ours,
                theirs,
            } => write!(
                f,
                "your {which} differ from the server's (yours {ours:#018x}, \
                 theirs {theirs:#018x}) — the two of you are running different assets"
            ),
        }
    }
}

impl std::error::Error for JoinError {}

/// Check a `Welcome` against our own registries.
pub fn accept(
    welcome: &ServerMessage,
    blocks: &BlockRegistry,
    items: &ItemRegistry,
) -> Result<Joined, JoinError> {
    let ServerMessage::Welcome {
        seed,
        you,
        blocks: theirs_blocks,
        items: theirs_items,
    } = welcome
    else {
        return Err(JoinError::NotAWelcome);
    };

    // Blocks first, then items, so a world that differs in both blames the one
    // further down the stack -- terrain is what a client generates for itself,
    // and a block table it disagrees about is the more fundamental problem.
    for (which, ours, theirs) in [
        ("blocks", blocks.fingerprint(), *theirs_blocks),
        ("items", items.fingerprint(), *theirs_items),
    ] {
        if ours != theirs {
            return Err(JoinError::Mismatch {
                which,
                ours,
                theirs,
            });
        }
    }

    Ok(Joined {
        seed: *seed,
        you: *you,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::{DropRule, Faces, Interact, ItemDef, Material, Rarity, Shape};
    use std::path::PathBuf;

    fn items_named(names: &[&str]) -> ItemRegistry {
        ItemRegistry::from_defs(
            names
                .iter()
                .map(|n| {
                    (
                        PathBuf::from(format!("{n}.ron")),
                        ItemDef {
                            name: (*n).to_string(),
                            max_stack: 64,
                            durability: None,
                            tier: 0,
                            speed: None,
                            burn_ticks: None,
                            rarity: Rarity::Common,
                        },
                    )
                })
                .collect(),
        )
        .expect("valid items")
    }

    fn blocks_named(names: &[&str]) -> BlockRegistry {
        BlockRegistry::from_materials(
            names
                .iter()
                .map(|n| {
                    (
                        PathBuf::from(format!("{n}.ron")),
                        Material {
                            name: (*n).to_string(),
                            solid: true,
                            faces: Faces::All("x".to_string()),
                            shapes: vec![Shape::Full],
                            drops: DropRule::SameName,
                            requires_tier: 0,
                            hardness: Some(1),
                            interact: Interact::None,
                        },
                    )
                })
                .collect(),
        )
        .expect("valid blocks")
    }

    fn welcome(blocks: u64, items: u64) -> ServerMessage {
        ServerMessage::Welcome {
            seed: 42,
            you: PlayerId(1),
            blocks,
            items,
        }
    }

    /// The same assets on both sides join.
    #[test]
    fn matching_assets_are_accepted() {
        let b = blocks_named(&["cubara:stone", "cubara:soil"]);
        let i = items_named(&["cubara:stone", "cubara:soil"]);
        let msg = welcome(b.fingerprint(), i.fingerprint());
        assert_eq!(
            accept(&msg, &b, &i),
            Ok(Joined {
                seed: 42,
                you: PlayerId(1)
            })
        );
    }

    /// **The check this module exists for.** One extra item on the server, and
    /// every id from that name onward means something different.
    #[test]
    fn an_item_the_server_has_and_we_do_not_is_refused_by_name() {
        let b = blocks_named(&["cubara:stone"]);
        let ours = items_named(&["cubara:stone", "cubara:soil"]);
        let theirs = items_named(&["cubara:stone", "cubara:soil", "cubara:iron"]);

        let msg = welcome(b.fingerprint(), theirs.fingerprint());
        match accept(&msg, &b, &ours) {
            Err(JoinError::Mismatch { which, .. }) => assert_eq!(which, "items"),
            other => panic!("a client with different items was let in: {other:?}"),
        }
    }

    /// The same for blocks, and it must name *blocks* — a refusal that blames
    /// the wrong registry sends someone looking in the wrong directory.
    #[test]
    fn a_block_mismatch_is_refused_and_names_blocks() {
        let ours = blocks_named(&["cubara:stone"]);
        let theirs = blocks_named(&["cubara:stone", "cubara:granite"]);
        let i = items_named(&["cubara:stone"]);

        let msg = welcome(theirs.fingerprint(), i.fingerprint());
        match accept(&msg, &ours, &i) {
            Err(JoinError::Mismatch { which, .. }) => assert_eq!(which, "blocks"),
            other => panic!("a client with different blocks was let in: {other:?}"),
        }
    }

    /// A renamed item shifts the table without changing its size, which is the
    /// case a length check would miss.
    #[test]
    fn a_rename_is_caught_even_though_the_count_is_the_same() {
        let b = blocks_named(&["cubara:stone"]);
        let ours = items_named(&["cubara:stone", "cubara:soil"]);
        let theirs = items_named(&["cubara:stone", "cubara:dirt"]);
        assert_eq!(ours.ids().count(), theirs.ids().count(), "same size");

        let msg = welcome(b.fingerprint(), theirs.fingerprint());
        assert!(
            matches!(accept(&msg, &b, &ours), Err(JoinError::Mismatch { .. })),
            "a rename that kept the count was let through"
        );
    }

    /// The order the files were read in must *not* matter: `from_defs` sorts by
    /// name, so two servers whose directories listed differently still agree.
    /// A check that rejected this would cry wolf, and a check that cries wolf
    /// gets turned off.
    #[test]
    fn the_order_the_files_arrived_in_does_not_matter() {
        let b = blocks_named(&["cubara:stone"]);
        let ours = items_named(&["cubara:soil", "cubara:stone"]);
        let theirs = items_named(&["cubara:stone", "cubara:soil"]);

        let msg = welcome(b.fingerprint(), theirs.fingerprint());
        assert!(
            accept(&msg, &b, &ours).is_ok(),
            "the same assets in a different file order were refused"
        );
    }

    #[test]
    fn anything_that_is_not_a_welcome_is_refused() {
        let b = blocks_named(&["cubara:stone"]);
        let i = items_named(&["cubara:stone"]);
        assert_eq!(
            accept(&ServerMessage::Tick(7), &b, &i),
            Err(JoinError::NotAWelcome)
        );
    }
}
