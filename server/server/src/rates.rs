//! Gameplay rates, held by the server so they are the same for everyone.
//!
//! Every rate is a multiplier on what the original game does: 1.0 is Emerald exactly, 2.0 is
//! double, 0.1 a tenth. Any positive value is allowed -- the point is a dial, not a preset.
//!
//! These live on the server rather than in the client for the reason everything else does: a
//! rate a client applies is a rate a patched client ignores. Publishing them here is also
//! what lets the server bound what a save may have gained between uploads, which is the piece
//! rate-of-gain validation was missing. It no longer has to enumerate every legitimate source
//! in the game; it states what the rates are and refuses more.

use std::collections::HashMap;

/// A multiplier on something the game hands the player.
///
/// Rejected rather than clamped when nonsensical: a typo that silently becomes 1.0 is worse
/// than one that refuses to start, because nobody notices the first until the economy is odd.
fn parse_rate(value: &str) -> anyhow::Result<f32> {
    let rate: f32 = value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("not a number: {value}"))?;
    anyhow::ensure!(
        rate.is_finite() && rate >= 0.0,
        "a rate must be finite and not negative, got {rate}"
    );
    Ok(rate)
}

#[derive(Debug, Clone)]
pub struct Rates {
    /// Experience from battles.
    pub experience: f32,
    /// How often wild encounters happen at all.
    pub encounter: f32,
    /// Prize money, sales, and every other source of pokedollars.
    pub money: f32,
    /// Items found, given, and dropped.
    pub items: f32,
    /// Catch probability.
    pub catch: f32,
    /// What shops charge.
    pub shop_price: f32,
    /// Per-species encounter multipliers, applied on top of `encounter`.
    ///
    /// Keyed by the game's internal species number. A species not named here uses 1.0, so a
    /// file only has to mention what it wants to change.
    pub species_encounter: HashMap<u16, f32>,
}

impl Default for Rates {
    /// Exactly the original game.
    fn default() -> Self {
        Self {
            experience: 1.0,
            encounter: 1.0,
            money: 1.0,
            items: 1.0,
            catch: 1.0,
            shop_price: 1.0,
            species_encounter: HashMap::new(),
        }
    }
}

impl Rates {
    /// The multiplier for encountering one species: the global rate and its own, together.
    pub fn encounter_for(&self, species: u16) -> f32 {
        self.encounter * self.species_encounter.get(&species).copied().unwrap_or(1.0)
    }

    /// Parse the rates file.
    ///
    /// Deliberately a plain `key = value` format rather than TOML or JSON: it is edited by
    /// hand, by someone tuning a number and restarting, and a format with no punctuation to
    /// get wrong is worth more here than one with structure nobody needs. `#` starts a
    /// comment, blank lines are ignored, and `species.NUMBER` names one species.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut rates = Rates::default();

        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }

            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("line {}: expected key = value", number + 1))?;
            let key = key.trim();
            let rate = parse_rate(value)
                .map_err(|e| anyhow::anyhow!("line {}: {key}: {e}", number + 1))?;

            match key {
                "experience" => rates.experience = rate,
                "encounter" => rates.encounter = rate,
                "money" => rates.money = rate,
                "items" => rates.items = rate,
                "catch" => rates.catch = rate,
                "shop_price" => rates.shop_price = rate,
                other => {
                    let species = other
                        .strip_prefix("species.")
                        .and_then(|n| n.parse::<u16>().ok())
                        .ok_or_else(|| {
                            anyhow::anyhow!("line {}: unknown rate {other}", number + 1)
                        })?;
                    rates.species_encounter.insert(species, rate);
                }
            }
        }

        Ok(rates)
    }

    /// Read the rates file, or use the original game's rates if there is not one.
    ///
    /// A missing file is not an error -- a server that has never been tuned should start --
    /// but a file that exists and does not parse is, because the alternative is running with
    /// rates nobody chose while believing otherwise.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let rates = Self::parse(&text)?;
                tracing::info!(
                    experience = rates.experience, encounter = rates.encounter,
                    money = rates.money, items = rates.items, catch = rates.catch,
                    species = rates.species_encounter.len(), "rates loaded"
                );
                Ok(rates)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no rates file; using the original game's rates");
                Ok(Self::default())
            }
            Err(e) => Err(e).map_err(anyhow::Error::from),
        }
    }
}

/// The most money the game can hand a player in a second, before the server's rate.
///
/// Deliberately far above anything real play produces. The job is catching a client that
/// awards itself a fortune between two saves, not policing an efficient player, and a rule
/// that refuses an honest save is worse than the cheating it prevents. A whole bag of Nuggets
/// sold at once is a few tens of thousands; this allows that every second, forever.
const MONEY_PER_SECOND: f32 = 50_000.0;

/// The most experience one Pokemon can earn in a second, before the server's rate.
///
/// A level 100 needs at most 1,640,000 in total, so this allows a Pokemon to go from nothing
/// to fully levelled in under a minute of continuous battling. Nothing legitimate comes close.
const EXPERIENCE_PER_SECOND: f32 = 30_000.0;

/// Whatever the rates are, a save cannot have gained more than this in the time available.
///
/// This is the rule the server could not write before it published the rates: bounding income
/// used to mean enumerating every legitimate source in the game, and a missed one refuses an
/// honest player. Now the server states the rates and refuses what exceeds them.
///
/// `elapsed` is the time since this character last uploaded. A first upload has no previous
/// save to compare against and is not checked here at all.
///
/// Deliberately generous, and only ever a ceiling. Returns None when nothing is provably
/// impossible -- not when the save is proven honest.
/// The multiplier to scale a ceiling by, for a configured rate.
///
/// Clamped against zero and nonsense, *not* against 1.0. Using `rate.max(1.0)` meant a server
/// configured below 1.0 -- deliberately stingy, which is exactly the setup where a suspicious
/// gain stands out most -- kept the same ceiling as a 1.0 server, so the tightening the operator
/// asked for silently did not apply to the anti-cheat. A rate of 0.1 should mean a tenth of the
/// headroom, not the same headroom.
///
/// The floor is a small positive number rather than zero so that a rate of 0 (an event server
/// with earning switched off, say) still permits the rounding and one-off scripted rewards that
/// do not go through the rate at all, instead of refusing every report.
fn ceiling_scale(rate: f32) -> f32 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0.01;
    }
    rate
}

pub fn gained_too_fast(
    before: &crate::save_parse::SaveState,
    after: &crate::save_parse::SaveState,
    rates: &Rates,
    elapsed: std::time::Duration,
) -> Option<String> {
    // A clock that has barely moved must still allow a whole save's worth of progress, or a
    // player who saves twice in quick succession is accused of something. One second minimum.
    let seconds = elapsed.as_secs_f32().max(1.0);

    let money_before = before.money();
    let money_after = after.money();
    if money_after > money_before {
        let gained = (money_after - money_before) as f32;
        let allowed = MONEY_PER_SECOND * ceiling_scale(rates.money) * seconds;
        if gained > allowed {
            return Some(format!(
                "gained {gained:.0} money in {seconds:.0}s, above the {allowed:.0} these rates allow"
            ));
        }
    }

    for old in &before.party {
        let Some(new) = after
            .party
            .iter()
            .find(|m| m.personality == old.personality && m.ot_id == old.ot_id)
        else {
            continue;
        };
        // Only records whose decrypted bytes agree with their own checksum are judged; the
        // rest are left alone rather than accused of something the decode may have invented.
        if !new.checksum_ok || !old.checksum_ok {
            continue;
        }
        if new.experience > old.experience {
            let gained = (new.experience - old.experience) as f32;
            let allowed = EXPERIENCE_PER_SECOND * ceiling_scale(rates.experience) * seconds;
            if gained > allowed {
                return Some(format!(
                    "a Pokemon gained {gained:.0} experience in {seconds:.0}s, above the \
                     {allowed:.0} these rates allow"
                ));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::save_parse::{PartyMon, SaveState};

    fn state(money_raw: u32, exp: u32) -> SaveState {
        SaveState {
            flags: vec![],
            vars: vec![],
            money_raw,
            coins_raw: 0,
            encryption_key: 0,
            bag: vec![],
            seen: vec![],
            game_stats: vec![],
            block1: vec![],
            berry_trees: vec![],
            rematches: vec![],
            party: vec![PartyMon {
                personality: 7,
                ot_id: 7,
                species: 1,
                level: 5,
                experience: exp,
                evs: [0; 6],
                checksum_ok: true,
            }],
        }
    }

    fn secs(n: u64) -> std::time::Duration {
        std::time::Duration::from_secs(n)
    }

    /// A stingy server actually tightens the ceiling.
    #[test]
    fn a_rate_below_one_lowers_the_ceiling() {
        let mut r = Rates::default();
        r.experience = 0.1;

        // A tenth of the rate means a tenth of the headroom: 30_000 * 0.1 * 1s = 3_000.
        assert!(
            gained_too_fast(&state(0, 0), &state(0, 10_000), &r, secs(1)).is_some(),
            "a 0.1x server must refuse a gain a 1.0x server would allow"
        );
        // Negative control: within the lowered ceiling is still fine, so this is a ceiling and
        // not a blanket refusal.
        assert!(
            gained_too_fast(&state(0, 0), &state(0, 2_000), &r, secs(1)).is_none(),
            "a gain inside the lowered ceiling must still pass"
        );
        // And the same gain is fine on a default server, which is what makes the first
        // assertion about the rate rather than about the amount.
        assert!(
            gained_too_fast(&state(0, 0), &state(0, 10_000), &Rates::default(), secs(1)).is_none(),
            "the refused gain must be acceptable at 1.0x"
        );
    }

    /// Ordinary play is never accused.
    #[test]
    fn a_normal_session_is_allowed() {
        let r = Rates::default();
        assert_eq!(gained_too_fast(&state(1000, 0), &state(50_000, 20_000), &r, secs(60)), None);
    }

    /// A client awarding itself a fortune between two saves is not.
    #[test]
    fn a_sudden_fortune_is_refused() {
        let r = Rates::default();
        let out = gained_too_fast(&state(0, 0), &state(900_000, 0), &r, secs(1));
        assert!(out.is_some(), "900k in a second is not play");
    }

    #[test]
    fn a_sudden_level_is_refused() {
        let r = Rates::default();
        let out = gained_too_fast(&state(0, 0), &state(0, 1_600_000), &r, secs(1));
        assert!(out.is_some(), "a full experience bar in a second is not play");
    }

    /// A generous server must allow generously, or its own rates become an accusation.
    #[test]
    fn the_servers_own_rates_widen_the_ceiling() {
        let mut r = Rates::default();
        let fast = gained_too_fast(&state(0, 0), &state(900_000, 0), &r, secs(1));
        assert!(fast.is_some());
        r.money = 100.0;
        assert_eq!(
            gained_too_fast(&state(0, 0), &state(900_000, 0), &r, secs(1)),
            None,
            "a server running 100x money must not refuse 100x money"
        );
    }

    /// Losing money is regression, not a gain, and is not this rule's business.
    #[test]
    fn spending_is_not_a_gain() {
        let r = Rates::default();
        assert_eq!(gained_too_fast(&state(900_000, 0), &state(10, 0), &r, secs(1)), None);
    }

    #[test]
    fn nothing_configured_is_the_original_game() {
        let rates = Rates::default();
        assert_eq!(rates.experience, 1.0);
        assert_eq!(rates.encounter_for(255), 1.0);
    }

    #[test]
    fn reads_every_rate() {
        let rates = Rates::parse(
            "experience = 2.0\nencounter = 0.1\nmoney = 3\nitems = 0.5\ncatch = 1.5\n",
        )
        .unwrap();
        assert_eq!(rates.experience, 2.0);
        assert_eq!(rates.encounter, 0.1);
        assert_eq!(rates.money, 3.0);
        assert_eq!(rates.items, 0.5);
        assert_eq!(rates.catch, 1.5);
    }

    /// A species rate multiplies the global one rather than replacing it, so halving
    /// encounters everywhere still halves them for a species that was already doubled.
    #[test]
    fn species_rates_compose_with_the_global_one() {
        let rates = Rates::parse("encounter = 0.5\nspecies.25 = 4\n").unwrap();
        assert_eq!(rates.encounter_for(25), 2.0);
        assert_eq!(rates.encounter_for(26), 0.5, "a species not named uses 1.0");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let rates = Rates::parse("# tuning\n\nexperience = 2 # double\n\n").unwrap();
        assert_eq!(rates.experience, 2.0);
    }

    /// A typo that silently became 1.0 would not be noticed until the economy was wrong.
    #[test]
    fn nonsense_is_refused_rather_than_ignored() {
        assert!(Rates::parse("experience = fast\n").is_err());
        assert!(Rates::parse("experience = -1\n").is_err());
        assert!(Rates::parse("expreience = 2\n").is_err(), "a misspelt key is a typo");
        assert!(Rates::parse("experience 2\n").is_err(), "missing the equals");
    }

    #[test]
    fn zero_is_allowed_because_it_is_a_choice() {
        let rates = Rates::parse("encounter = 0\n").unwrap();
        assert_eq!(rates.encounter, 0.0);
    }
}
