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

    /// The anti-cheat ceilings, so the enforcement headroom is tunable rather than baked in.
    ///
    /// A server that runs unusually generous events, or an unusually strict one, can move these
    /// without a rebuild. They are the *ceiling* on how fast money and experience may arrive,
    /// scaled further by the money/experience multipliers above, over a burst window.
    pub ceiling_money_per_second: f32,
    pub ceiling_experience_per_second: f32,
    pub ceiling_burst_seconds: f32,
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
            ceiling_money_per_second: DEFAULT_MONEY_PER_SECOND,
            ceiling_experience_per_second: DEFAULT_EXPERIENCE_PER_SECOND,
            ceiling_burst_seconds: DEFAULT_BURST_SECONDS,
        }
    }
}

impl Rates {
    /// The multiplier for encountering one species: the global rate and its own, together.
    #[allow(dead_code)] // per-species rates: parsed and tested, wiring pending
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
                "ceiling_money_per_second" => rates.ceiling_money_per_second = rate,
                "ceiling_experience_per_second" => rates.ceiling_experience_per_second = rate,
                "ceiling_burst_seconds" => rates.ceiling_burst_seconds = rate,
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
                    experience = rates.experience,
                    encounter = rates.encounter,
                    money = rates.money,
                    items = rates.items,
                    catch = rates.catch,
                    species = rates.species_encounter.len(),
                    "rates loaded"
                );
                Ok(rates)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no rates file; using the original game's rates");
                Ok(Self::default())
            }
            Err(e) => Err(anyhow::Error::from(e)),
        }
    }
}

/// The rate sets for each world.
///
/// Deadman runs a deliberately harsher economy than Normal -- lower experience, a low catch rate,
/// scarce money -- so each mode gets its own `Rates`, and a character's mode picks which applies.
/// A server that has not written a deadman config simply runs the normal rates in both worlds.
pub struct ModeRates {
    pub normal: Rates,
    pub deadman: Rates,
}

impl ModeRates {
    /// The rates for a character's mode. An unrecognised mode falls back to the normal set, so a
    /// bad value can never hand out unbounded gains.
    pub fn for_mode(&self, mode: &str) -> &Rates {
        match mode {
            "deadman" => &self.deadman,
            _ => &self.normal,
        }
    }
}

/// The most money the game can hand a player in a second, before the server's rate.
///
/// Deliberately far above anything real play produces. The job is catching a client that
/// awards itself a fortune between two saves, not policing an efficient player, and a rule
/// that refuses an honest save is worse than the cheating it prevents. A whole bag of Nuggets
/// sold at once is a few tens of thousands; this allows that every second, forever.
const DEFAULT_MONEY_PER_SECOND: f32 = 50_000.0;

/// The most experience one Pokemon can earn in a second, before the server's rate.
///
/// A level 100 needs at most 1,640,000 in total, so this allows a Pokemon to go from nothing
/// to fully levelled in under a minute of continuous battling. Nothing legitimate comes close.
const DEFAULT_EXPERIENCE_PER_SECOND: f32 = 30_000.0;

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

/// How much unspent allowance can build up, in seconds' worth.
///
/// Without a cap, a character idle for an hour could spend an hour of headroom in one report,
/// which is the same as having no ceiling for anyone patient. A minute is generous enough that
/// ordinary bursts -- a long battle, a shop trip -- never touch it.
const DEFAULT_BURST_SECONDS: f32 = 60.0;

/// A running allowance for one character, spent by gains and refilled by time.
///
/// Replaces passing a bare `elapsed` to `gained_too_fast`. That took the time since the *last
/// report* and floored it at one second, which was fine when a report meant a whole save upload
/// arriving at most every ninety frames. Now that money, items and the party each report as they
/// change, a client sending ten reports a second collected ten separate one-second allowances --
/// so reporting more often bought more headroom, and the ceiling measured message frequency
/// rather than rate of gain.
///
/// A bucket cannot be gamed that way: time refills it and gains empty it, so the sustained limit
/// is the same whether progress arrives in one report or a hundred.
pub struct Allowance {
    last: std::time::Instant,
    money: f32,
    experience: f32,
}

impl Allowance {
    /// Starts with a second's worth rather than a full bucket.
    ///
    /// A full bucket on connect would hand out a minute of headroom to anyone who reconnects,
    /// which turns reconnecting into the cheat. A second is enough that the first honest report
    /// after signing in is never refused.
    pub fn new(rates: &Rates) -> Self {
        Self {
            last: std::time::Instant::now(),
            money: rates.ceiling_money_per_second * ceiling_scale(rates.money),
            experience: rates.ceiling_experience_per_second * ceiling_scale(rates.experience),
        }
    }

    fn refill(&mut self, rates: &Rates) {
        let seconds = self.last.elapsed().as_secs_f32();
        self.last = std::time::Instant::now();

        let money_rate = rates.ceiling_money_per_second * ceiling_scale(rates.money);
        let exp_rate = rates.ceiling_experience_per_second * ceiling_scale(rates.experience);
        let burst = rates.ceiling_burst_seconds;

        self.money = (self.money + money_rate * seconds).min(money_rate * burst);
        self.experience = (self.experience + exp_rate * seconds).min(exp_rate * burst);
    }

    /// Judge a change, spending the allowance it costs.
    ///
    /// Nothing is spent when the change is refused, so a rejected report does not make the next
    /// honest one more likely to be refused too.
    pub fn check(
        &mut self,
        before: &crate::save_parse::SaveState,
        after: &crate::save_parse::SaveState,
        rates: &Rates,
    ) -> Option<String> {
        self.refill(rates);

        let money_gained = after.money().saturating_sub(before.money()) as f32;
        if money_gained > self.money {
            return Some(format!(
                "gained {money_gained:.0} money, above the {:.0} this character has built up",
                self.money
            ));
        }

        let mut exp_gained = 0.0f32;
        for old in &before.party {
            let Some(new) = after
                .party
                .iter()
                .find(|m| m.personality == old.personality && m.ot_id == old.ot_id)
            else {
                continue;
            };
            if !old.checksum_ok || !new.checksum_ok {
                continue;
            }
            if new.experience > old.experience {
                exp_gained += (new.experience - old.experience) as f32;
            }
        }
        if exp_gained > self.experience {
            return Some(format!(
                "gained {exp_gained:.0} experience, above the {:.0} this character has built up",
                self.experience
            ));
        }

        self.money -= money_gained;
        self.experience -= exp_gained;
        None
    }
}

/// How many whole-save operations a connection may trigger per second once its burst is spent.
const DEFAULT_HEAVY_PER_SECOND: f32 = 10.0;
/// The burst a connection may spend at once. Sized above a full save upload (a 128KB block arrives
/// as ~32 chunks) so an honest first-save or resync never waits, while a flood still settles to the
/// sustained rate afterwards.
const DEFAULT_HEAVY_BURST: f32 = 64.0;

/// A token bucket pacing how often one connection may make the server do heavy save work.
///
/// Every typed report and every resync costs a full 128KB load, parse, rebuild and store (or a
/// fresh copy handed back). The authenticated control stream had no limit on how often a client
/// could ask for that, so a 4-byte `Resync` or a tiny `MoneyChanged` spammed as fast as the stream
/// drained turned into thousands of full save round-trips a second from a single connection --
/// enough to exhaust the database pool and saturate upload. This bounds that.
///
/// Refilled by time, spent one token per heavy message. Generous enough that an honest client --
/// which resyncs rarely and reports only on real progress -- never spends its way empty; tight
/// enough that a flood is throttled to the refill rate. Cheap messages (keys, chat, movement) are
/// not gated by it.
pub struct RequestBudget {
    tokens: f32,
    per_second: f32,
    burst: f32,
    last: std::time::Instant,
}

impl RequestBudget {
    /// A budget with the default heavy-operation rate and burst.
    pub fn new() -> Self {
        Self::with_rate(DEFAULT_HEAVY_PER_SECOND, DEFAULT_HEAVY_BURST)
    }

    /// A budget with an explicit rate and burst, for tests.
    pub fn with_rate(per_second: f32, burst: f32) -> Self {
        Self {
            // Start full: the burst exists precisely so the operations a client legitimately does
            // right after signing in (its first resync, a first-save upload) are never throttled.
            tokens: burst,
            per_second,
            burst,
            last: std::time::Instant::now(),
        }
    }

    /// Take one token for a heavy operation, returning whether one was available. Refills by the
    /// time elapsed since the last call first, so a quiet connection is always allowed a full burst.
    pub fn take(&mut self) -> bool {
        let seconds = self.last.elapsed().as_secs_f32();
        self.last = std::time::Instant::now();
        self.tokens = (self.tokens + self.per_second * seconds).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for RequestBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // superseded by Allowance; retained for its ceiling-math tests
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
        let allowed = DEFAULT_MONEY_PER_SECOND * ceiling_scale(rates.money) * seconds;
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
            let allowed = DEFAULT_EXPERIENCE_PER_SECOND * ceiling_scale(rates.experience) * seconds;
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
                held_item: 0,
                moves: [0; 4],
                evs: [0; 6],
                checksum_ok: true,
            }],
        }
    }

    fn secs(n: u64) -> std::time::Duration {
        std::time::Duration::from_secs(n)
    }

    /// A configured ceiling is honoured, and the default is the historical value.
    ///
    /// Negative control: a gain that the default ceiling allows is refused once the ceiling is
    /// configured low -- otherwise the config key would parse but do nothing.
    #[test]
    fn the_money_ceiling_is_configurable() {
        // Default: 50,000/s of headroom scales money reports fine.
        let def = Rates::default();
        assert_eq!(def.ceiling_money_per_second, 50_000.0);
        let mut b = Allowance::new(&def);
        assert!(
            b.check(&state(0, 0), &state(40_000, 0), &def).is_none(),
            "40k is within the default ceiling"
        );

        // Configure a strict ceiling and the same gain is now refused.
        let strict = Rates::parse("ceiling_money_per_second = 1000").expect("parses");
        assert_eq!(strict.ceiling_money_per_second, 1000.0);
        let mut sb = Allowance::new(&strict);
        assert!(
            sb.check(&state(0, 0), &state(40_000, 0), &strict).is_some(),
            "40k must exceed a 1000/s ceiling"
        );
        // ...but a small gain within the strict ceiling still passes.
        let mut sb2 = Allowance::new(&strict);
        assert!(
            sb2.check(&state(0, 0), &state(500, 0), &strict).is_none(),
            "a gain under the strict ceiling is fine"
        );
    }

    /// A character's mode selects its rate set, and an unknown mode falls back to normal so a bad
    /// value can never hand out the wrong (or unbounded) economy.
    #[test]
    fn for_mode_selects_the_right_world() {
        let rates = ModeRates {
            normal: Rates {
                experience: 2.5,
                ..Rates::default()
            },
            deadman: Rates {
                experience: 0.8,
                ..Rates::default()
            },
        };
        assert_eq!(rates.for_mode("normal").experience, 2.5);
        assert_eq!(rates.for_mode("deadman").experience, 0.8);
        assert_eq!(
            rates.for_mode("nonsense").experience,
            2.5,
            "an unknown mode must fall back to the normal set"
        );
        assert_eq!(rates.for_mode("").experience, 2.5);
    }

    /// The shipped Deadman config is the high-stakes economy the design calls for: slow experience,
    /// a low catch rate, and scarce money. Guards the actual numbers so a careless edit to the
    /// example config is caught, not silently shipped.
    #[test]
    fn the_deadman_config_is_the_high_stakes_economy() {
        let text = include_str!("../../rates.deadman.conf.example");
        let r = Rates::parse(text).expect("the deadman example config parses");
        assert_eq!(r.experience, 0.8, "slow leveling");
        assert_eq!(r.catch, 0.35, "captures are an investment");
        assert_eq!(r.money, 0.6, "money is scarce");
        assert_eq!(r.encounter, 0.8, "encounters are less relentless");
        assert!(
            r.experience < 1.0 && r.money < 1.0 && r.catch < 1.0,
            "Deadman must be harsher than the base game on every stakes dial"
        );
    }

    /// A stingy server actually tightens the ceiling.
    #[test]
    fn a_rate_below_one_lowers_the_ceiling() {
        let r = Rates {
            experience: 0.1,
            ..Default::default()
        };

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

    /// Reporting more often does not buy more headroom.
    ///
    /// This is the property the old per-call floor did not have, and the reason for the change:
    /// with a floor, ten reports in a second collected ten separate one-second allowances.
    #[test]
    fn many_small_reports_cannot_outrun_one_large_one() {
        let r = Rates::default();
        let mut bucket = Allowance::new(&r);

        // Drain the initial second's worth, then keep asking. Time barely advances across this
        // loop, so almost nothing refills -- the total permitted stays near one second's worth
        // however many reports it is split across.
        let mut allowed_total = 0u32;
        let step = 5_000u32;
        for _ in 0..50 {
            if bucket.check(&state(0, 0), &state(0, step), &r).is_none() {
                allowed_total += step;
            }
        }

        assert!(
            allowed_total <= (DEFAULT_EXPERIENCE_PER_SECOND as u32) * 2,
            "fifty rapid reports let through {allowed_total} experience, which is more than \
             a couple of seconds' worth -- splitting a gain up is buying headroom"
        );

        // Negative control: the bucket is not simply refusing everything. A fresh one accepts
        // an ordinary gain.
        let mut fresh = Allowance::new(&r);
        assert!(
            fresh.check(&state(0, 0), &state(0, 1_000), &r).is_none(),
            "an ordinary gain must be accepted, or this is a ceiling of zero"
        );
    }

    /// Time refills the allowance.
    #[test]
    fn waiting_restores_headroom() {
        let r = Rates::default();
        let mut bucket = Allowance::new(&r);

        // Spend it all.
        while bucket.check(&state(0, 0), &state(0, 5_000), &r).is_none() {}

        // Rewind the clock to simulate a wait, which is the only way to test this without
        // sleeping in a unit test.
        bucket.last = std::time::Instant::now() - std::time::Duration::from_secs(10);

        assert!(
            bucket.check(&state(0, 0), &state(0, 5_000), &r).is_none(),
            "ten seconds of waiting must restore enough headroom for an ordinary gain"
        );
    }

    /// Ordinary play is never accused.
    #[test]
    fn a_normal_session_is_allowed() {
        let r = Rates::default();
        assert_eq!(
            gained_too_fast(&state(1000, 0), &state(50_000, 20_000), &r, secs(60)),
            None
        );
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
        assert!(
            out.is_some(),
            "a full experience bar in a second is not play"
        );
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
        assert_eq!(
            gained_too_fast(&state(900_000, 0), &state(10, 0), &r, secs(1)),
            None
        );
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
        assert!(
            Rates::parse("expreience = 2\n").is_err(),
            "a misspelt key is a typo"
        );
        assert!(
            Rates::parse("experience 2\n").is_err(),
            "missing the equals"
        );
    }

    #[test]
    fn zero_is_allowed_because_it_is_a_choice() {
        let rates = Rates::parse("encounter = 0\n").unwrap();
        assert_eq!(rates.encounter, 0.0);
    }

    /// A flood is throttled: a connection may spend its burst, then is refused until time refills.
    #[test]
    fn a_burst_of_heavy_operations_is_throttled() {
        // No refill happens across these calls (real time barely moves), so this isolates the burst.
        let mut budget = RequestBudget::with_rate(10.0, 5.0);
        for i in 0..5 {
            assert!(
                budget.take(),
                "token {i} within the burst should be granted"
            );
        }
        assert!(
            !budget.take(),
            "the sixth exceeds the burst and must be refused"
        );
        assert!(
            !budget.take(),
            "and it stays refused while no time has passed"
        );
    }

    /// The negative control: an honest connection's pace is never throttled. A client that makes a
    /// heavy request about once a second, far above any real report rate, is always granted -- so
    /// the limiter cannot disconnect or drop reports from a laggy but honest player.
    #[test]
    fn an_honest_pace_is_never_throttled() {
        let mut budget = RequestBudget::with_rate(10.0, 5.0);
        // Spend the whole burst first, so this proves the *refill* keeps an honest pace flowing
        // rather than merely riding the initial burst.
        for _ in 0..5 {
            assert!(budget.take());
        }
        // Ten heavy requests, each after a tenth of a second -- 10/s, the sustained rate, which is
        // already generous next to a real client that reports only on progress.
        for i in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(110));
            assert!(
                budget.take(),
                "request {i} at the sustained rate must be granted"
            );
        }
    }

    /// Waiting quietly restores a full burst, so a connection that goes idle is not left short.
    #[test]
    fn waiting_refills_the_budget() {
        let mut budget = RequestBudget::with_rate(100.0, 4.0);
        for _ in 0..4 {
            assert!(budget.take());
        }
        assert!(!budget.take(), "burst is spent");
        std::thread::sleep(std::time::Duration::from_millis(60)); // 100/s * 0.06s = 6 tokens, capped at 4
        assert!(budget.take(), "time refilled the bucket");
    }
}
