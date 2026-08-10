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

#[cfg(test)]
mod tests {
    use super::*;

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
