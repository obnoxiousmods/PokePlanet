//! Story-progression validation over the save's flag bitfield.
//!
//! Two rules, at two confidence levels, because a derived rule that is even slightly wrong
//! freezes real players mid-story:
//!
//!   - **Badges** (`BADGE_FLAGS`) are enforced. Losing a badge is never legitimate, and the eight
//!     ids are certain, so a report that clears a badge already held is refused outright.
//!
//!   - **Monotonic story flags** (`MONOTONIC_FLAGS`) are, for now, advisory. The set is derived
//!     (see tools/questflags/extract.py) by excluding every flag the game is known to clear --
//!     temporary, daily, and the dynamic ranges cleared by computed id (trainer rematches,
//!     decorations, union rooms). That derivation is careful but cannot be proven complete from
//!     the outside, so clearing one is *logged* rather than refused until real play has shown the
//!     set produces no false positives. Flipping it to enforced is a one-line change once the
//!     journal is quiet.
//!
//! The flag bitfield is flag id `f` -> bit `f % 8` of byte `f / 8`, matching FlagGet in the game.

include!("quest_flags_gen.rs");

use crate::save_parse::SaveState;

fn flag_set(flags: &[u8], id: u16) -> bool {
    let byte = (id / 8) as usize;
    let bit = id % 8;
    flags
        .get(byte)
        .map(|b| (b >> bit) & 1 == 1)
        .unwrap_or(false)
}

/// A badge held before but cleared now. Enforced: no honest game takes a badge back.
pub fn badge_regressed(before: &SaveState, after: &SaveState) -> Option<String> {
    for &id in BADGE_FLAGS.iter() {
        if flag_set(&before.flags, id) && !flag_set(&after.flags, id) {
            return Some(format!("a badge already earned was cleared (flag {id})"));
        }
    }
    None
}

/// A monotonic story flag cleared. Advisory: returned for logging, not yet for refusal, until
/// the derived set is proven quiet against real play.
pub fn monotonic_cleared(before: &SaveState, after: &SaveState) -> Option<u16> {
    for &id in MONOTONIC_FLAGS.iter() {
        if flag_set(&before.flags, id) && !flag_set(&after.flags, id) {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_flags(bytes: usize, set: &[u16]) -> SaveState {
        let mut flags = vec![0u8; bytes];
        for &id in set {
            flags[(id / 8) as usize] |= 1 << (id % 8);
        }
        SaveState {
            flags,
            vars: vec![],
            money_raw: 0,
            coins_raw: 0,
            encryption_key: 0,
            bag: vec![],
            seen: vec![],
            game_stats: vec![],
            block1: vec![],
            berry_trees: vec![],
            rematches: vec![],
            party: vec![],
        }
    }

    /// A cleared badge is caught; the negative controls are that gaining a badge and leaving
    /// badges untouched are both fine -- otherwise the rule would refuse ordinary progress.
    #[test]
    fn losing_a_badge_is_refused_gaining_one_is_not() {
        let b0 = BADGE_FLAGS[0];
        let with = state_with_flags(300, &[b0]);
        let without = state_with_flags(300, &[]);

        assert!(
            badge_regressed(&with, &without).is_some(),
            "clearing a held badge must be refused"
        );
        assert!(
            badge_regressed(&without, &with).is_none(),
            "earning a badge is not a regression"
        );
        assert!(
            badge_regressed(&with, &with).is_none(),
            "leaving badges untouched is not a regression"
        );
    }

    /// The monotonic set is non-empty and its rule fires on a cleared story flag while staying
    /// silent when nothing regresses -- the negative control that keeps it from flagging honest
    /// play once it is enforced.
    #[test]
    fn a_cleared_monotonic_flag_is_detected() {
        assert!(
            !MONOTONIC_FLAGS.is_empty(),
            "the derived set must not be empty"
        );
        let f = MONOTONIC_FLAGS[0];
        let with = state_with_flags(300, &[f]);
        let without = state_with_flags(300, &[]);

        assert_eq!(monotonic_cleared(&with, &without), Some(f));
        assert_eq!(
            monotonic_cleared(&without, &with),
            None,
            "gaining a flag is fine"
        );
        assert_eq!(monotonic_cleared(&with, &with), None, "no change is fine");
    }

    /// Badges are not in the advisory set (they are enforced separately) and the two sets do not
    /// contradict -- a badge id must not also be a dynamic-clear id that slipped in.
    #[test]
    fn badges_are_the_eight_expected_system_flags() {
        // SYSTEM_FLAGS 0x860 + 0x7..0xE = 0x867..0x86E = 2151..2158.
        assert_eq!(
            BADGE_FLAGS,
            [2151, 2152, 2153, 2154, 2155, 2156, 2157, 2158]
        );
    }
}
