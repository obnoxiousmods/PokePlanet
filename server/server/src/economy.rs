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

#[cfg(test)]
mod tests {
    use super::*;

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
