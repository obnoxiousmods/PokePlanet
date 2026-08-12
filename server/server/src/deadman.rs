//! The rules that define a Deadman character's world.
//!
//! Deadman Mode is a separate, permadeath world: a fainted Pokemon dies forever, progression is
//! capped to the next gym leader, and players can only fight others near their own badge count.
//! These are the pure "laws" of that world -- the client enforces them as the player leans on the
//! caps, and the server validates every report against the same functions here, so a patched
//! client cannot buy its way past them.
//!
//! All pure and unit-tested; the values are tunable but the shape is fixed.

/// Level cap by badges earned.
///
/// A Deadman party mon may not exceed the level of the next gym leader the player has to face, so
/// progress is gated to real gym wins rather than grinding. Derived from the Emerald gym leaders'
/// own top party levels (Roxanne .. the Elite Four), indexed by badge count 0..=8.
const LEVEL_CAP_BY_BADGES: [u8; 9] = [15, 19, 24, 29, 31, 33, 42, 46, 58];

/// The highest level any Deadman party mon may reach with `badges` badges earned.
pub fn level_cap(badges: u8) -> u8 {
    LEVEL_CAP_BY_BADGES[(badges as usize).min(8)]
}

/// Party size by badges earned.
///
/// Early Deadman is a knife-edge of two mons; the party grows toward a full six as gyms fall, so a
/// young character cannot hide behind a deep bench.
const PARTY_CAP_BY_BADGES: [u8; 9] = [2, 2, 3, 3, 4, 4, 5, 5, 6];

/// How many Pokemon a Deadman character may carry with `badges` badges earned.
pub fn party_cap(badges: u8) -> u8 {
    PARTY_CAP_BY_BADGES[(badges as usize).min(8)]
}

/// The combat level (3..=126) shown next to a Deadman character and used to rank the ladder.
///
/// OSRS-style: a single number summarising how dangerous an account is, from its party's levels and
/// its badges. Not a gate on its own -- PvP eligibility is the badge range below -- but the headline
/// figure players compare. Empty party (a fresh or wiped character) is the floor, 3.
#[allow(dead_code)] // wired into the ladder + profile display in Phase F
pub fn combat_level(party_levels: &[u8], badges: u8) -> u16 {
    if party_levels.is_empty() {
        return 3;
    }
    let max = *party_levels.iter().max().unwrap() as f32;
    let avg = party_levels.iter().map(|&l| l as f32).sum::<f32>() / party_levels.len() as f32;
    let raw = (max * 0.9 + avg * 0.4 + badges as f32 * 4.0).round() as i32;
    raw.clamp(3, 126) as u16
}

/// Whether two Deadman players may fight: within two badges of each other.
///
/// The wilderness-style gate that stops an eight-badge veteran from farming a one-badge newcomer,
/// while still leaving a real spread of opponents in range.
pub fn pvp_in_badge_range(a: u8, b: u8) -> bool {
    a.abs_diff(b) <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_cap_rises_with_badges_and_saturates() {
        assert_eq!(
            level_cap(0),
            15,
            "a fresh character is capped at the first gym"
        );
        assert_eq!(level_cap(4), 31);
        assert_eq!(
            level_cap(8),
            58,
            "eight badges caps at the Elite Four's level"
        );
        assert_eq!(
            level_cap(9),
            58,
            "an impossible ninth badge does not overflow the table"
        );
        assert_eq!(level_cap(255), 58);
        // Monotonic non-decreasing: more badges never lowers the cap.
        for b in 1..=8u8 {
            assert!(level_cap(b) >= level_cap(b - 1));
        }
    }

    #[test]
    fn party_cap_grows_from_two_to_six() {
        assert_eq!(party_cap(0), 2, "early deadman is two mons");
        assert_eq!(party_cap(8), 6, "a full team only at the end");
        assert_eq!(party_cap(200), 6, "never overflows the table");
        for b in 1..=8u8 {
            assert!(party_cap(b) >= party_cap(b - 1));
            assert!(party_cap(b) <= 6);
        }
    }

    #[test]
    fn combat_level_is_bounded_and_ordered() {
        assert_eq!(combat_level(&[], 0), 3, "no party is the floor");
        let low = combat_level(&[5, 5], 0);
        let high = combat_level(&[50, 48, 45], 6);
        assert!((3..=126).contains(&low) && (3..=126).contains(&high));
        assert!(high > low, "a stronger, further-along team ranks higher");
        // Never exceeds the ceiling even at an absurd input.
        assert_eq!(combat_level(&[100, 100, 100, 100, 100, 100], 8), 126);
    }

    #[test]
    fn pvp_range_is_two_badges_either_way() {
        assert!(pvp_in_badge_range(3, 5), "two up is in range");
        assert!(pvp_in_badge_range(5, 3), "two down is in range");
        assert!(pvp_in_badge_range(4, 4), "same badges is in range");
        assert!(!pvp_in_badge_range(1, 4), "three apart is out of range");
        assert!(!pvp_in_badge_range(8, 0));
    }
}
