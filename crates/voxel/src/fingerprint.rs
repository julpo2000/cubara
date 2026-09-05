//! A number two machines can compare to find out whether they mean the same
//! thing by an id.
//!
//! Block 2.12b. `BlockId` and `ItemId` cross the network **raw**, and they are
//! assigned from the *sorted names* of whatever `assets/` held at load time
//! ([`crate::ItemRegistry::from_defs`]). So two machines with identical assets
//! agree exactly — and two machines whose assets differ by one file disagree
//! about the whole table from that name onward. Stone becomes iron, quietly.
//!
//! The save format met this problem first and solved it by storing names and
//! remapping on load (`PHASE2_ARCHITECTURE.md` §8.1). That is right *there*: it
//! is your own world, the only copy, and refusing it means losing it.
//!
//! Over a wire it is the wrong answer, for two reasons. There is a good outcome
//! available — update your assets — and nobody can reach it unless they are told
//! the assets differ. And remapping cannot actually fix it: renamed ids can be
//! translated, an item one side has and the other does not cannot be invented,
//! so you would get a world with blocks silently missing. That is the same bug,
//! later and harder to find.
//!
//! So the wire compares fingerprints and **refuses**, naming which registry
//! disagrees.
//!
//! # What is hashed, and what deliberately is not
//!
//! **The sorted names, and nothing else** — because the sorted names are exactly
//! what decides the ids. Hashing the files instead would reject connections that
//! are perfectly compatible: an added comment, a `max_stack` tweak, or a git
//! checkout that changed the line endings. A check that cries wolf is a check
//! someone turns off.
//!
//! # Why FNV-1a again
//!
//! It is already this project's hash: `cubara_sim::WorldHash` uses it, written
//! in-crate for the same "no dependency" reason. This is a second small
//! implementation rather than a shared one because dependencies point one way
//! (Rule 3) — `cubara-sim` may use `cubara-voxel`, not the reverse — and because
//! the world hash's encoding is pinned by tests across two platforms. Making it
//! a shared surface would invite a change there for a reason that had nothing to
//! do with the world.

/// The 64-bit FNV-1a constants, as in `cubara_sim::hash`.
const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x100_0000_01b3;

/// Fold a run of names into one number.
///
/// Each name is followed by a zero byte. Without a separator, `["ab", "c"]` and
/// `["a", "bc"]` would hash alike, and a rename that shifted a character between
/// two names would be invisible — which is precisely the case this exists to
/// catch. A zero byte cannot appear inside a name, so it cannot be forged.
pub fn of_names<'a>(names: impl Iterator<Item = &'a str>) -> u64 {
    let mut h = OFFSET_BASIS;
    let mut fold = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    };
    for name in names {
        for b in name.as_bytes() {
            fold(*b);
        }
        fold(0);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_names_in_the_same_order_agree() {
        let a = of_names(["cubara:stone", "cubara:soil"].into_iter());
        let b = of_names(["cubara:stone", "cubara:soil"].into_iter());
        assert_eq!(a, b);
    }

    #[test]
    fn a_missing_name_shows_up() {
        let full = of_names(["cubara:stone", "cubara:soil", "cubara:grass"].into_iter());
        let short = of_names(["cubara:stone", "cubara:soil"].into_iter());
        assert_ne!(full, short, "a registry with one item fewer hashed alike");
    }

    #[test]
    fn order_matters_because_ids_do() {
        let a = of_names(["cubara:soil", "cubara:stone"].into_iter());
        let b = of_names(["cubara:stone", "cubara:soil"].into_iter());
        assert_ne!(
            a, b,
            "two orders hashed alike, but they hand out different ids"
        );
    }

    /// The separator earning its place: without it these two collide.
    #[test]
    fn a_boundary_between_names_is_part_of_the_hash() {
        let a = of_names(["ab", "c"].into_iter());
        let b = of_names(["a", "bc"].into_iter());
        assert_ne!(a, b, "the name boundary was not hashed");
    }

    #[test]
    fn an_empty_registry_is_not_zero() {
        // Not a strong property, but a fingerprint that came back 0 for "I read
        // nothing" would compare equal to a peer that failed the same way.
        assert_ne!(of_names(std::iter::empty()), 0);
    }
}
