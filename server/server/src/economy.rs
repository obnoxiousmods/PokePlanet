//! Server-authoritative money moves against a character's stored save.
//!
//! Wagered battles (Feature 1), trades (Feature 3) and item drops (Feature 4) all move money
//! between characters. Money only ever changes HERE, in the server's own copy of the save, so a
//! client can never mint or duplicate it — it does not report a gain and get believed; the server
//! computes the new balance and rewrites the save. Every write goes through
//! `save_parse::reauthor`, which proves it can author the image faithfully before storing, exactly
//! like the reported-money path in the control loop.
//!
//! Callers must hold the character's save lock (`Server::save_lock`) across a read-then-write pair,
//! so a deduct/credit cannot interleave with another writer for the same character.

use crate::db::{self, Db};
use crate::save_parse::{self, MAX_MONEY};

/// The balance after taking `amount`, or `None` if the character cannot cover it. Pure.
fn after_deduct(current: u32, amount: u32) -> Option<u32> {
    (current >= amount).then(|| current - amount)
}

/// The balance after receiving `amount`, clamped to the game's money cap (and overflow-safe). Pure.
fn after_credit(current: u32, amount: u32) -> u32 {
    current.saturating_add(amount).min(MAX_MONEY)
}

/// Deposit the whole carried wallet into the bank: the new (carried, bank). Pure.
///
/// The bank is a `u64` so repeated deposits accumulate past the in-game money cap without loss.
#[allow(dead_code)] // wired in with the PC bank UI
pub fn deposit_all(carried: u32, bank: u64) -> (u32, u64) {
    (0, bank + carried as u64)
}

/// Withdraw from the bank into the carried wallet, capped at the game's money limit: as much as the
/// wallet can still hold moves out, the rest stays banked. Returns the new (carried, bank). Pure.
#[allow(dead_code)] // wired in with the PC bank UI
pub fn withdraw_all(carried: u32, bank: u64) -> (u32, u64) {
    let room = (MAX_MONEY.saturating_sub(carried)) as u64;
    let moved = room.min(bank);
    (carried + moved as u32, bank - moved)
}

/// Rebuild `stored` with `new_money`. `None` if the image cannot be authored faithfully.
fn image_with_money(stored: &[u8], new_money: u32) -> Option<Vec<u8>> {
    let old = save_parse::parse(stored)?;
    save_parse::reauthor(stored, &save_parse::with_money(&old, new_money))
}

/// A character's current money, or `None` if there is no readable save.
#[allow(dead_code)] // wired in as the economy features land
pub async fn money(db: &Db, character_id: i64) -> anyhow::Result<Option<u32>> {
    let Some(stored) = db::load_save(db, character_id).await? else {
        return Ok(None);
    };
    Ok(save_parse::parse(&stored).map(|s| s.money()))
}

/// Escrow a stake: take `amount` if the character can afford it, returning the remaining balance.
/// `None` means they could not cover it (or had no readable save) and nothing was changed — the
/// affordability check and the write are one operation under the caller's save lock.
#[allow(dead_code)] // wired in by Feature 1 (wager escrow)
pub async fn try_deduct(db: &Db, character_id: i64, amount: u32) -> anyhow::Result<Option<u32>> {
    let Some(stored) = db::load_save(db, character_id).await? else {
        return Ok(None);
    };
    let Some(old) = save_parse::parse(&stored) else {
        return Ok(None);
    };
    let Some(remaining) = after_deduct(old.money(), amount) else {
        return Ok(None);
    };
    let Some(candidate) = image_with_money(&stored, remaining) else {
        return Ok(None);
    };
    db::store_save(db, character_id, &candidate).await?;
    Ok(Some(remaining))
}

/// Pay out a pot or refund a stake: give `amount`, clamped to the money cap, returning the new
/// balance. `None` only if there is no readable save. Hold the character's save lock.
#[allow(dead_code)] // wired in by Feature 1 (payout / refund)
pub async fn credit(db: &Db, character_id: i64, amount: u32) -> anyhow::Result<Option<u32>> {
    let Some(stored) = db::load_save(db, character_id).await? else {
        return Ok(None);
    };
    let Some(old) = save_parse::parse(&stored) else {
        return Ok(None);
    };
    let total = after_credit(old.money(), amount);
    let Some(candidate) = image_with_money(&stored, total) else {
        return Ok(None);
    };
    db::store_save(db, character_id, &candidate).await?;
    Ok(Some(total))
}

/// The amount that can actually move from a sender holding `from_balance` to a receiver holding
/// `to_balance`: capped by what the sender has AND by the room left under the receiver's money cap,
/// so a transfer never mints money (over the cap) nor lets a sender spend what they lack. Pure.
fn movable(from_balance: u32, to_balance: u32, amount: u32) -> u32 {
    let room = MAX_MONEY.saturating_sub(to_balance);
    amount.min(from_balance).min(room)
}

/// Move up to `amount` pokedollars from `from` to `to` as one server-authored step. Returns the new
/// `(from_balance, to_balance)`, or `None` if nothing moved (either save unreadable, the sender is
/// broke, or the receiver is already at the cap). The caller MUST hold BOTH characters' save locks
/// across this call so no other writer can interleave; lock them in a stable order to avoid deadlock.
pub async fn transfer(
    db: &Db,
    from: i64,
    to: i64,
    amount: u32,
) -> anyhow::Result<Option<(u32, u32)>> {
    let (Some(from_bal), Some(to_bal)) = (money(db, from).await?, money(db, to).await?) else {
        return Ok(None);
    };
    let moved = movable(from_bal, to_bal, amount);
    if moved == 0 {
        return Ok(None);
    }
    // Take first: if the deduct somehow fails the receiver is never credited, so money can't appear.
    let Some(from_left) = try_deduct(db, from, moved).await? else {
        return Ok(None);
    };
    let Some(to_total) = credit(db, to, moved).await? else {
        return Ok(None);
    };
    Ok(Some((from_left, to_total)))
}

/// How many of one item can move from a holder of `from_qty` to a holder of `to_qty`: capped by what
/// the sender actually holds AND by the room left under the receiver's stack cap, so an item transfer
/// never duplicates an item nor overflows a slot. Pure.
fn movable_items(from_qty: u16, to_qty: u16, amount: u16) -> u16 {
    let room = save_parse::MAX_ITEM_QUANTITY.saturating_sub(to_qty);
    amount.min(from_qty).min(room)
}

/// Move up to `amount` of one item between two characters as one server-authored step, mirroring the
/// money transfer: the server rewrites both saves so a client can never duplicate an item by claiming
/// it kept its copy. Returns the new `(from_qty, to_qty)` for that item, or `None` if nothing moved
/// (a save unreadable, the sender lacks the item, the receiver's slot is full, or their pocket has no
/// free slot to hold a new stack). The pocket is located from the sender's own bag -- an item id
/// belongs to exactly one pocket -- so the client never names it. Both images are authored before
/// either is stored, so a failure to place the item on the receiver never leaves the sender already
/// debited. Hold BOTH save locks.
pub async fn transfer_item(
    db: &Db,
    from: i64,
    to: i64,
    item: u16,
    amount: u16,
) -> anyhow::Result<Option<(u16, u16)>> {
    if item == 0 {
        return Ok(None);
    }
    let (Some(from_bytes), Some(to_bytes)) =
        (db::load_save(db, from).await?, db::load_save(db, to).await?)
    else {
        return Ok(None);
    };
    let (Some(from_state), Some(to_state)) =
        (save_parse::parse(&from_bytes), save_parse::parse(&to_bytes))
    else {
        return Ok(None);
    };
    // The sender must actually hold the item; its pocket comes from where it sits in their bag.
    let Some((pocket, from_qty)) = from_state.find_item(item) else {
        return Ok(None);
    };
    // Key items are never tradeable -- they are bikes, passes and story items, not fungible goods.
    // The client blocks them too (by importance), but a modified client must not get past this.
    // Pocket 1 is Key Items in the save's pocket order (see BAG_POCKETS).
    const KEY_ITEMS_POCKET: u8 = 1;
    if pocket == KEY_ITEMS_POCKET {
        return Ok(None);
    }
    let to_qty = to_state.item_quantity(pocket, item);
    let moved = movable_items(from_qty, to_qty, amount);
    if moved == 0 {
        return Ok(None);
    }
    let from_new = from_qty - moved;
    let to_new = to_qty + moved;
    // Author both images first; only store once both are known to be buildable (the receiver's
    // pocket might be full, which with_item signals by returning None).
    let Some(from_block1) = save_parse::with_item(&from_state, pocket, item, from_new) else {
        return Ok(None);
    };
    let Some(to_block1) = save_parse::with_item(&to_state, pocket, item, to_new) else {
        return Ok(None);
    };
    let (Some(from_cand), Some(to_cand)) = (
        save_parse::reauthor(&from_bytes, &from_block1),
        save_parse::reauthor(&to_bytes, &to_block1),
    ) else {
        return Ok(None);
    };
    db::store_save(db, from, &from_cand).await?;
    db::store_save(db, to, &to_cand).await?;
    Ok(Some((from_new, to_new)))
}

/// The party bytes after removing the mon in `slot`, packed down so the survivors stay contiguous
/// and the vacated tail slot is zeroed. Pure; operates only on the game's own bytes, never decoding
/// a Pokemon. `party` is `MAX_PARTY * MON_BYTES` long.
fn party_without(party: &[u8], slot: usize, count: usize) -> Vec<u8> {
    let mon = save_parse::SaveState::MON_BYTES;
    let mut out = vec![0u8; party.len()];
    let mut w = 0;
    for s in 0..count {
        if s == slot {
            continue;
        }
        out[w * mon..w * mon + mon].copy_from_slice(&party[s * mon..s * mon + mon]);
        w += 1;
    }
    out
}

/// The party bytes after appending `mon` at the first free slot (index `count`). Pure. The caller
/// guarantees there is room (`count < MAX_PARTY`).
fn party_with(party: &[u8], count: usize, mon: &[u8]) -> Vec<u8> {
    let sz = save_parse::SaveState::MON_BYTES;
    let mut out = party.to_vec();
    out[count * sz..count * sz + sz].copy_from_slice(mon);
    out
}

/// Move one Pokemon (identified by its `personality`, which is stable across party reordering) from
/// `from`'s party to `to`'s party as one server-authored step, mirroring the money and item gifts.
/// The Pokemon travels as the game's own bytes -- never decoded or re-encoded -- so it cannot be
/// corrupted, and it exists in exactly one save at every moment because both parties are authored
/// before either is stored. Returns the new `(from_count, to_count)`, or `None` if nothing moved:
/// the sender does not hold that Pokemon, it is their last one (a party may not be emptied), or the
/// receiver's party is already at `max_to_party` (their badge-based cap, never above the engine's 6).
/// Hold BOTH save locks.
pub async fn transfer_pokemon(
    db: &Db,
    from: i64,
    to: i64,
    personality: u32,
    max_to_party: u8,
) -> anyhow::Result<Option<(u8, u8)>> {
    let (Some(from_bytes), Some(to_bytes)) =
        (db::load_save(db, from).await?, db::load_save(db, to).await?)
    else {
        return Ok(None);
    };
    let (Some(from_state), Some(to_state)) =
        (save_parse::parse(&from_bytes), save_parse::parse(&to_bytes))
    else {
        return Ok(None);
    };

    // Parsed party slots are packed (no gaps), so a Pokemon's index in the parsed party is its raw
    // slot. Identify by personality so a reordered party cannot make us move the wrong Pokemon.
    let from_count = from_state.party.len();
    let Some(slot) = from_state
        .party
        .iter()
        .position(|m| m.personality == personality)
    else {
        return Ok(None);
    };
    // Never let a player trade away their last Pokemon -- that would leave them unable to play (and,
    // in Deadman, is indistinguishable from a wipe).
    if from_count < 2 {
        return Ok(None);
    }

    let cap = (max_to_party as usize).min(save_parse::SaveState::MAX_PARTY);
    let to_count = to_state.party.len();
    if to_count >= cap {
        return Ok(None);
    }

    let mon = save_parse::SaveState::MON_BYTES;
    let from_party = from_state.party_bytes();
    let to_party = to_state.party_bytes();
    if from_party.len() < save_parse::SaveState::MAX_PARTY * mon
        || to_party.len() < save_parse::SaveState::MAX_PARTY * mon
    {
        return Ok(None);
    }
    let moved = from_party[slot * mon..slot * mon + mon].to_vec();

    let new_from = party_without(&from_party, slot, from_count);
    let new_to = party_with(&to_party, to_count, &moved);
    let from_new_count = (from_count - 1) as u8;
    let to_new_count = (to_count + 1) as u8;

    // Author both images first; only store once both are known to be buildable, so a Pokemon can
    // never be removed from one save without being placed in the other.
    let Some(from_block1) = save_parse::with_party(&from_state, from_new_count, &new_from) else {
        return Ok(None);
    };
    let Some(to_block1) = save_parse::with_party(&to_state, to_new_count, &new_to) else {
        return Ok(None);
    };
    let (Some(from_cand), Some(to_cand)) = (
        save_parse::reauthor(&from_bytes, &from_block1),
        save_parse::reauthor(&to_bytes, &to_block1),
    ) else {
        return Ok(None);
    };
    db::store_save(db, from, &from_cand).await?;
    db::store_save(db, to, &to_cand).await?;
    Ok(Some((from_new_count, to_new_count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removing a party slot packs the survivors down and clears the freed tail, and appending puts
    /// the Pokemon in the first free slot -- so a trade conserves every Pokemon's bytes exactly, with
    /// none duplicated and none dropped. Uses sentinel bytes rather than real Pokemon: this is the
    /// byte bookkeeping, which is all that must be proven correct (the bytes themselves never change).
    #[test]
    fn a_pokemon_move_is_exact_byte_bookkeeping() {
        let mon = save_parse::SaveState::MON_BYTES;
        let slots = save_parse::SaveState::MAX_PARTY;
        // A party of three: slot s filled with the byte (s+1).
        let mut party = vec![0u8; slots * mon];
        for s in 0..3 {
            for b in &mut party[s * mon..s * mon + mon] {
                *b = (s + 1) as u8;
            }
        }

        // Remove the middle mon: [1,2,3,..] -> [1,3,..], tail cleared.
        let after = party_without(&party, 1, 3);
        assert!(after[0..mon].iter().all(|&b| b == 1), "first mon stays");
        assert!(
            after[mon..2 * mon].iter().all(|&b| b == 3),
            "third packs into slot 1"
        );
        assert!(
            after[2 * mon..].iter().all(|&b| b == 0),
            "everything past the survivors is clear"
        );

        // Append a fourth mon (byte 9) to a party of two.
        let two = after; // now holds two mons (bytes 1 and 3)
        let mut newcomer = vec![0u8; mon];
        newcomer.iter_mut().for_each(|b| *b = 9);
        let joined = party_with(&two, 2, &newcomer);
        assert!(joined[0..mon].iter().all(|&b| b == 1));
        assert!(joined[mon..2 * mon].iter().all(|&b| b == 3));
        assert!(
            joined[2 * mon..3 * mon].iter().all(|&b| b == 9),
            "newcomer lands in slot 2"
        );
    }

    /// An item transfer moves only what the sender holds and only what fits under the receiver's
    /// stack cap, so it can neither take an item the sender lacks nor duplicate one past a full slot.
    #[test]
    fn an_item_transfer_conserves_items() {
        let cap = save_parse::MAX_ITEM_QUANTITY;
        assert_eq!(
            movable_items(5, 0, 3),
            3,
            "a plain move hands over the asked count"
        );
        assert_eq!(movable_items(2, 0, 3), 2, "capped by what the sender holds");
        assert_eq!(
            movable_items(80, cap - 10, 40),
            10,
            "capped by the room left in the receiver's slot"
        );
        assert_eq!(
            movable_items(5, cap, 3),
            0,
            "a full receiver slot takes nothing"
        );
        assert_eq!(
            movable_items(0, 0, 3),
            0,
            "a sender without the item moves nothing"
        );
    }

    /// A transfer moves only what the sender has and only what fits under the receiver's cap, so it
    /// can neither overdraw the sender nor mint money past the game's limit.
    #[test]
    fn a_transfer_conserves_money() {
        assert_eq!(
            movable(1000, 0, 400),
            400,
            "a plain move takes the asked amount"
        );
        assert_eq!(movable(300, 0, 400), 300, "capped by what the sender holds");
        assert_eq!(
            movable(1000, MAX_MONEY - 100, 400),
            100,
            "capped by the room left under the receiver's cap"
        );
        assert_eq!(
            movable(1000, MAX_MONEY, 400),
            0,
            "a full receiver takes nothing"
        );
        assert_eq!(movable(0, 0, 400), 0, "a broke sender moves nothing");
    }

    /// A stake you cannot cover is refused, and the boundary (exactly your whole balance) is allowed.
    #[test]
    fn cannot_escrow_more_than_held() {
        assert_eq!(after_deduct(1000, 400), Some(600));
        assert_eq!(
            after_deduct(1000, 1000),
            Some(0),
            "staking your whole balance is fine"
        );
        assert_eq!(
            after_deduct(1000, 1001),
            None,
            "a stake you can't cover takes nothing"
        );
        assert_eq!(after_deduct(0, 1), None);
    }

    /// Depositing empties the wallet into the bank; withdrawing fills the wallet to the cap and
    /// leaves any surplus banked, so no money is ever lost across a deposit/withdraw round-trip.
    #[test]
    fn the_bank_never_loses_money() {
        // Deposit all: wallet emptied, bank grows by exactly the wallet.
        assert_eq!(deposit_all(5000, 0), (0, 5000));
        assert_eq!(deposit_all(3000, 5000), (0, 8000));
        assert_eq!(
            deposit_all(0, 8000),
            (0, 8000),
            "depositing nothing changes nothing"
        );

        // Withdraw all: fills the wallet, surplus over the cap stays in the bank.
        assert_eq!(withdraw_all(0, 5000), (5000, 0));
        let over_cap = MAX_MONEY as u64 + 12_345;
        let (carried, bank) = withdraw_all(0, over_cap);
        assert_eq!(carried, MAX_MONEY, "the wallet fills only to the money cap");
        assert_eq!(bank, 12_345, "the surplus stays banked, not lost");

        // A full round-trip conserves the total no matter the cap.
        let (c1, b1) = deposit_all(MAX_MONEY, 900_000); // bank now well over the cap
        let (c2, b2) = withdraw_all(c1, b1);
        assert_eq!(
            c2 as u64 + b2,
            MAX_MONEY as u64 + 900_000,
            "no money created or destroyed"
        );
    }

    /// A payout never pushes a balance past the cap, and never overflows.
    #[test]
    fn a_payout_clamps_at_the_money_cap() {
        assert_eq!(after_credit(1000, 500), 1500);
        assert_eq!(
            after_credit(MAX_MONEY - 10, 100),
            MAX_MONEY,
            "clamped to the cap"
        );
        assert_eq!(after_credit(MAX_MONEY, 1), MAX_MONEY);
        assert_eq!(
            after_credit(u32::MAX - 5, 100),
            MAX_MONEY,
            "saturating, no wrap"
        );
    }
}
