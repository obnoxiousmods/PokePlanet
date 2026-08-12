// The drop layer's world state and wire are built on this pure ruleset; the model and rules land
// first, tested, then the store and handlers that use them.
#![allow(dead_code)]

//! Items dropped on the ground for other players to pick up.
//!
//! A drop is a stack of one item sitting on a tile of a map. Any player can drop from their bag, and
//! any player standing on the drop can take it -- except for Deadman death-drops, which the killer
//! gets first. The rules here are pure and unit-tested; the server owns every drop and moves the
//! item between bags itself, so a drop can never duplicate an item (the dropper's bag is debited
//! server-side before the drop exists, and the taker's is credited when it is removed).

/// One dropped stack sitting in the world.
#[derive(Debug, Clone)]
pub struct WorldItem {
    pub id: u64,
    pub map_group: u8,
    pub map_num: u8,
    pub x: i16,
    pub y: i16,
    pub item: u16,
    pub quantity: u16,
    /// The player who gets first claim (a PvP killer), or `None` for a freely-dropped item.
    pub owner: Option<u64>,
    /// Seconds since the drop was created, stamped by the world tick (not wall-clock here).
    pub age_s: u64,
}

/// How long the killer of a Deadman player has the exclusive right to their death-drop.
pub const OWNER_WINDOW_S: u64 = 60;
/// How long a drop lingers in total before it is gone forever.
pub const EXPIRE_S: u64 = 180;

/// Whether a given player may pick up a drop right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Pickup {
    /// The taker may have it.
    Allowed,
    /// Owned by someone else and still inside their exclusive window.
    Reserved,
    /// Past its lifetime; it should be reaped, and nobody gets it.
    Expired,
}

/// The pickup rule: a freely-dropped item (no owner) is anyone's immediately. A death-drop is the
/// owner's alone for the first `OWNER_WINDOW_S`, then anyone's until `EXPIRE_S`, then gone.
pub fn can_pick_up(age_s: u64, owner: Option<u64>, taker: u64) -> Pickup {
    if age_s >= EXPIRE_S {
        return Pickup::Expired;
    }
    match owner {
        Some(o) if o != taker && age_s < OWNER_WINDOW_S => Pickup::Reserved,
        _ => Pickup::Allowed,
    }
}

/// Whether a drop has outlived its lifetime and should be reaped.
pub fn is_expired(age_s: u64) -> bool {
    age_s >= EXPIRE_S
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_drop_is_anyones_at_once() {
        assert_eq!(can_pick_up(0, None, 7), Pickup::Allowed);
        assert_eq!(can_pick_up(1, None, 999), Pickup::Allowed);
    }

    #[test]
    fn a_death_drop_is_the_killers_first_then_public_then_gone() {
        let killer = 10u64;
        let stranger = 20u64;
        // 0..60s: killer only.
        assert_eq!(can_pick_up(0, Some(killer), killer), Pickup::Allowed);
        assert_eq!(can_pick_up(0, Some(killer), stranger), Pickup::Reserved);
        assert_eq!(can_pick_up(59, Some(killer), stranger), Pickup::Reserved);
        // 60..180s: anyone.
        assert_eq!(can_pick_up(60, Some(killer), stranger), Pickup::Allowed);
        assert_eq!(can_pick_up(179, Some(killer), stranger), Pickup::Allowed);
        assert_eq!(can_pick_up(60, Some(killer), killer), Pickup::Allowed);
        // 180s+: gone for everyone, even the killer.
        assert_eq!(can_pick_up(180, Some(killer), killer), Pickup::Expired);
        assert_eq!(can_pick_up(180, Some(killer), stranger), Pickup::Expired);
        assert_eq!(can_pick_up(5000, None, stranger), Pickup::Expired);
    }

    #[test]
    fn expiry_matches_the_window() {
        assert!(!is_expired(0));
        assert!(!is_expired(EXPIRE_S - 1));
        assert!(is_expired(EXPIRE_S));
        assert!(is_expired(EXPIRE_S + 100));
    }
}
