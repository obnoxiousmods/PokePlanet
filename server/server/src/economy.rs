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

#[cfg(test)]
mod tests {
    use super::*;

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
