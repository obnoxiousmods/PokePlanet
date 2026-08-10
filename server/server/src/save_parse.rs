//! Reading the game's flash image, so the server understands progress rather than storing
//! 128KB it cannot see into.
//!
//! The client already uploads its whole save. Parsing it here rather than asking the client
//! for a summary is the point: a summary is one more thing the client would be trusted to
//! tell the truth about, whereas the image is what the game itself will read back. It is
//! also the only way the server can ever notice progress that could not have happened,
//! which is what validation needs.
//!
//! Layout, from the game's own save.c and global.h:
//!
//! - 32 sectors of 4096 bytes. Sectors 0-13 are one save slot, 14-27 the other.
//! - Each sector ends in a footer: its id, a checksum, a signature and a counter. The id
//!   says which part of the save it holds, and it is *not* the same as its position -- the
//!   game rotates sectors on every write to spread flash wear -- so a slot has to be
//!   indexed by footer id rather than read in order.
//! - The slot with the higher counter is the newer one, which is the one the game will load.
//! - SaveBlock1 is spread across sector ids 1 to 4, in order, 3968 bytes at a time.

const SECTOR_SIZE: usize = 4096;
const SECTOR_DATA_SIZE: usize = 3968;
const SECTORS_PER_SLOT: usize = 14;
const NUM_SECTORS: usize = 32;
const SECTOR_SIGNATURE: u32 = 0x0801_2025;

/// Sector ids holding SaveBlock1, in order.
const SAVEBLOCK1_SECTORS: [u16; 4] = [1, 2, 3, 4];

/// Sector id holding SaveBlock2, which carries the key the rest is obfuscated with.
const SAVEBLOCK2_SECTOR: u16 = 0;

/// Offset of the obfuscation key within SaveBlock2.
const OFFSET_ENCRYPTION_KEY: usize = 0xAC;

/// Caps the game itself enforces, from src/money.c and include/constants/coins.h.
///
/// These are the only rules worth checking at this stage, precisely because they cannot
/// produce a false accusation: the game clamps to them, so no amount of ordinary play can
/// exceed them. Rules about how fast money may be earned would need every legitimate source
/// enumerated first, and getting that wrong refuses an honest player -- which is a worse
/// bug than the cheating it would catch.
pub const MAX_MONEY: u32 = 999_999;
pub const MAX_COINS: u16 = 9_999;

/// Caps the game enforces on a Pokemon, from src/pokemon.c and include/constants/pokemon.h.
pub const MAX_LEVEL: u8 = 100;
pub const MAX_EV_PER_STAT: u16 = 255;
pub const MAX_EV_TOTAL: u16 = 510;

/// Offsets within SaveBlock1. From the annotated struct in include/global.h.
const OFFSET_PARTY: usize = 0x238;
const PARTY_SIZE: usize = 6;
const MON_SIZE: usize = 100;
/// Within a `struct Pokemon`: the box data, then status, then the level.
const MON_OFFSET_LEVEL: usize = 0x54;
/// Within a `struct BoxPokemon`: where the four obfuscated substructs begin.
const BOX_OFFSET_SECURE: usize = 32;
/// And where the checksum over those substructs is stored.
const BOX_OFFSET_CHECKSUM: usize = 28;

/// Which physical slot each substruct type occupies, chosen by personality % 24.
///
/// Straight from GetSubstruct in src/pokemon.c: row[type] is the slot that type sits in.
/// The shuffle exists to make naive save editing harder, and reading it wrong yields
/// plausible-looking nonsense rather than an obvious failure -- which is why this is copied
/// rather than reasoned about.
const SUBSTRUCT_ORDER: [[usize; 4]; 24] = [
    [0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 1, 3], [0, 3, 1, 2],
    [0, 2, 3, 1], [0, 3, 2, 1], [1, 0, 2, 3], [1, 0, 3, 2],
    [2, 0, 1, 3], [3, 0, 1, 2], [2, 0, 3, 1], [3, 0, 2, 1],
    [1, 2, 0, 3], [1, 3, 0, 2], [2, 1, 0, 3], [3, 1, 0, 2],
    [2, 3, 0, 1], [3, 2, 0, 1], [1, 2, 3, 0], [1, 3, 2, 0],
    [2, 1, 3, 0], [3, 1, 2, 0], [2, 3, 1, 0], [3, 2, 1, 0],
];

const OFFSET_MONEY: usize = 0x490;
const OFFSET_COINS: usize = 0x494;

/// The bag, pocket by pocket: offset and slot count, from the annotated SaveBlock1 struct
/// and BAG_*_COUNT in include/constants/global.h. The two agree -- each pocket's offset plus
/// four bytes a slot lands exactly on the next -- which is what makes them trustworthy.
const BAG_POCKETS: [(usize, usize); 5] = [
    (0x560, 30), // Items
    (0x5D8, 30), // Key Items
    (0x650, 16), // Poke Balls
    (0x690, 64), // TMs and HMs
    (0x790, 46), // Berries
];

/// What one slot can hold, from MAX_BAG_ITEM_CAPACITY in include/constants/items.h.
pub const MAX_ITEM_QUANTITY: u16 = 99;
const OFFSET_FLAGS: usize = 0x1270;
const OFFSET_VARS: usize = 0x139C;

pub const FLAG_BYTES: usize = OFFSET_VARS - OFFSET_FLAGS; // 300
pub const VAR_COUNT: usize = 256;

/// One Pokemon, as much of it as validation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyMon {
    pub species: u16,
    pub level: u8,
    pub experience: u32,
    /// HP, Attack, Defence, Speed, Sp. Attack, Sp. Defence.
    pub evs: [u8; 6],
    /// Whether the decrypted substructs add up to the checksum stored alongside them.
    ///
    /// This is what says the decode worked. The game writes the sum of the plain substruct
    /// bytes into the record, so an exclusive-or with the wrong key produces bytes that sum
    /// to something else with overwhelming probability -- there is no reading of a wrong
    /// key that quietly agrees. The game itself treats a mismatch as a bad egg.
    pub checksum_ok: bool,
}

/// Pull one `struct Pokemon` apart.
///
/// Returns None for an empty slot. The four substructs are exclusive-ored with
/// personality ^ otId and shuffled by personality % 24; both have to be undone before any
/// of it means anything.
fn read_mon(bytes: &[u8]) -> Option<PartyMon> {
    let personality = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
    let ot_id = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
    let key = personality ^ ot_id;

    let secure = bytes.get(BOX_OFFSET_SECURE..BOX_OFFSET_SECURE + 48)?;
    let mut plain = [0u8; 48];
    for (i, chunk) in secure.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes(chunk.try_into().ok()?) ^ key;
        plain[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }

    // The game's own sum: every decrypted substruct byte, as 16-bit words.
    let computed: u16 = plain
        .chunks_exact(2)
        .fold(0u16, |acc, c| acc.wrapping_add(u16::from_le_bytes([c[0], c[1]])));
    let stored = u16::from_le_bytes(
        bytes.get(BOX_OFFSET_CHECKSUM..BOX_OFFSET_CHECKSUM + 2)?.try_into().ok()?,
    );

    let order = SUBSTRUCT_ORDER[(personality % 24) as usize];
    let growth = order[0] * 12;
    let evs_at = order[2] * 12;

    let species = u16::from_le_bytes([plain[growth], plain[growth + 1]]);
    if species == 0 {
        return None; // an empty slot
    }
    let experience = u32::from_le_bytes([
        plain[growth + 4], plain[growth + 5], plain[growth + 6], plain[growth + 7],
    ]);

    let mut evs = [0u8; 6];
    evs.copy_from_slice(&plain[evs_at..evs_at + 6]);

    Some(PartyMon {
        species,
        level: *bytes.get(MON_OFFSET_LEVEL)?,
        experience,
        evs,
        checksum_ok: computed == stored,
    })
}

/// What the server takes from a save image.
///
/// Flags and vars are kept as the game's own bitfield and array rather than interpreted.
/// Reinterpreting them would mean encoding what every script in the game means, and the
/// useful questions -- did this move, could it have moved that way -- do not need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveState {
    pub flags: Vec<u8>,
    pub vars: Vec<u16>,
    /// Still obfuscated. See `money`.
    pub money_raw: u32,
    /// Still obfuscated. See `coins`.
    pub coins_raw: u16,
    /// The key the save was written with, from SaveBlock2.
    pub encryption_key: u32,
    /// The party, empty slots omitted.
    pub party: Vec<PartyMon>,
    /// The bag: (pocket, item id, quantity), empty slots omitted.
    pub bag: Vec<(u8, u16, u16)>,
}

impl SaveState {
    /// The player's money.
    ///
    /// Stored exclusive-ored with a key held in SaveBlock2, which the game re-rolls on every
    /// battle -- so the stored bytes differ between two saves of the same amount, and only
    /// the decoded value is comparable.
    pub fn money(&self) -> u32 {
        self.money_raw ^ self.encryption_key
    }

    /// Game Corner coins, obfuscated with the low half of the same key.
    pub fn coins(&self) -> u16 {
        self.coins_raw ^ (self.encryption_key as u16)
    }

    /// What in this save could not have come from playing the game.
    ///
    /// None means only that nothing here is provably impossible -- it is not a statement
    /// that the save is honest. This is the floor of validation, not the ceiling.
    pub fn impossible(&self) -> Option<String> {
        if self.money() > MAX_MONEY {
            return Some(format!(
                "money is {}, above the {} the game clamps to",
                self.money(),
                MAX_MONEY
            ));
        }
        if self.coins() > MAX_COINS {
            return Some(format!(
                "coins are {}, above the {} the game clamps to",
                self.coins(),
                MAX_COINS
            ));
        }

        // A slot holds at most ninety-nine of anything -- the game starts a new slot rather
        // than putting a hundredth in -- and never zero, since it clears a slot instead of
        // leaving one empty.
        //
        // Enforced because the decode behind it was finally checked against real data rather
        // than argued for: a known item written into a running game, obfuscated by the game
        // itself, saved, uploaded and read back here as the same item and the same count.
        // Until that evidence existed this was left unenforced, because both saves to hand
        // had empty bags and a wrong decode would have refused every honest save carrying an
        // item.
        for (pocket, item, quantity) in &self.bag {
            if *quantity == 0 || *quantity > MAX_ITEM_QUANTITY {
                return Some(format!(
                    "bag pocket {} holds {} of item {}, which the game cannot store",
                    pocket + 1, quantity, item
                ));
            }
        }

        for (i, mon) in self.party.iter().enumerate() {
            // Level is read straight out of `struct Pokemon`, at an offset confirmed against
            // the running game: it reported level 5 for a save this parser also reads as 5.
            if mon.level > MAX_LEVEL {
                return Some(format!(
                    "party slot {} is level {}, above the maximum of {}",
                    i + 1, mon.level, MAX_LEVEL
                ));
            }

            // Everything below came through the substruct decode, so it is only trusted on
            // a record whose decrypted bytes add up to the checksum stored beside them. A
            // wrong decode yields plausible-looking numbers rather than an obvious failure,
            // and refusing a save on the strength of one would lock an honest player out
            // over the server's own arithmetic. The checksum is what rules that out.
            //
            // A record that does not verify is left alone rather than refused: the game
            // treats one as a bad egg, which is already its own punishment, and a save can
            // hold one through corruption rather than cheating.
            if !mon.checksum_ok {
                continue;
            }

            // Both the per-stat cap and the total, because a save can break one without the
            // other: six stats of 85 is a legal total made of legal parts, and one stat of
            // 510 is a legal total made of an illegal part.
            let total: u16 = mon.evs.iter().map(|e| *e as u16).sum();
            if total > MAX_EV_TOTAL {
                return Some(format!(
                    "party slot {} has {} effort points, above the maximum of {}",
                    i + 1, total, MAX_EV_TOTAL
                ));
            }
            if let Some(ev) = mon.evs.iter().find(|e| **e as u16 > MAX_EV_PER_STAT) {
                return Some(format!(
                    "party slot {} has {} effort points in one stat, above the maximum of {}",
                    i + 1, ev, MAX_EV_PER_STAT
                ));
            }
        }

        None
    }
}

struct Sector<'a> {
    id: u16,
    counter: u32,
    signature: u32,
    data: &'a [u8],
}

fn read_sector(image: &[u8], index: usize) -> Option<Sector<'_>> {
    let start = index * SECTOR_SIZE;
    let raw = image.get(start..start + SECTOR_SIZE)?;
    // The footer is 128 bytes, of which the first 116 are unused padding.
    let footer = &raw[SECTOR_DATA_SIZE + 116..];
    Some(Sector {
        id: u16::from_le_bytes([footer[0], footer[1]]),
        // footer[2..4] is the checksum, deliberately not verified here: it is taken over
        // each sector's *declared* size, which differs per sector and depends on the exact
        // sizeof(SaveBlock1) this build was compiled with. Guessing that wrong would reject
        // a perfectly good save and lock a player out of their character, which is a far
        // worse failure than not checking it.
        signature: u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]),
        counter: u32::from_le_bytes([footer[8], footer[9], footer[10], footer[11]]),
        data: &raw[..SECTOR_DATA_SIZE],
    })
}

/// The save slot the game would load: the one whose sectors carry the higher counter.
fn newest_slot(image: &[u8]) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;

    for slot in 0..(NUM_SECTORS / SECTORS_PER_SLOT) {
        let mut counter = None;
        let mut valid = 0;
        for i in 0..SECTORS_PER_SLOT {
            let Some(sector) = read_sector(image, slot * SECTORS_PER_SLOT + i) else {
                continue;
            };
            if sector.signature != SECTOR_SIGNATURE {
                continue;
            }
            valid += 1;
            // Every sector in a slot carries the same counter; the first is enough.
            counter.get_or_insert(sector.counter);
        }
        // A slot the game never finished writing is not a slot to read.
        if valid < SECTORS_PER_SLOT {
            continue;
        }
        if let Some(counter) = counter {
            if best.is_none_or(|(_, b)| counter > b) {
                best = Some((slot, counter));
            }
        }
    }

    best.map(|(slot, _)| slot)
}

/// Reassemble SaveBlock1 from the sectors that hold it, indexed by footer id.
fn saveblock1(image: &[u8], slot: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(SAVEBLOCK1_SECTORS.len() * SECTOR_DATA_SIZE);

    for want in SAVEBLOCK1_SECTORS {
        let mut found = None;
        for i in 0..SECTORS_PER_SLOT {
            let sector = read_sector(image, slot * SECTORS_PER_SLOT + i)?;
            if sector.signature == SECTOR_SIGNATURE && sector.id == want {
                found = Some(sector.data);
                break;
            }
        }
        out.extend_from_slice(found?);
    }

    Some(out)
}

/// Read what the server cares about out of a whole flash image.
///
/// Returns None for an image that is not a save at all -- the wrong size, or with no slot
/// the game would load. A save that parses is not thereby trusted; it is merely legible.
pub fn parse(image: &[u8]) -> Option<SaveState> {
    if image.len() < NUM_SECTORS * SECTOR_SIZE {
        return None;
    }

    let slot = newest_slot(image)?;
    let block = saveblock1(image, slot)?;

    let mut encryption_key = 0;
    for i in 0..SECTORS_PER_SLOT {
        let sector = read_sector(image, slot * SECTORS_PER_SLOT + i)?;
        if sector.signature == SECTOR_SIGNATURE && sector.id == SAVEBLOCK2_SECTOR {
            let at = sector.data.get(OFFSET_ENCRYPTION_KEY..OFFSET_ENCRYPTION_KEY + 4)?;
            encryption_key = u32::from_le_bytes(at.try_into().ok()?);
            break;
        }
    }

    let flags = block.get(OFFSET_FLAGS..OFFSET_FLAGS + FLAG_BYTES)?.to_vec();
    let raw_vars = block.get(OFFSET_VARS..OFFSET_VARS + VAR_COUNT * 2)?;
    let vars = raw_vars
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let money_bytes = block.get(OFFSET_MONEY..OFFSET_MONEY + 4)?;
    let money_raw = u32::from_le_bytes(money_bytes.try_into().ok()?);
    let coins_bytes = block.get(OFFSET_COINS..OFFSET_COINS + 2)?;
    let coins_raw = u16::from_le_bytes(coins_bytes.try_into().ok()?);

    // Quantities are obfuscated with the low half of the same key as money, per
    // GetBagItemQuantity in src/item.c.
    let mut bag = Vec::new();
    for (pocket, (at, slots)) in BAG_POCKETS.iter().enumerate() {
        for slot in 0..*slots {
            let off = at + slot * 4;
            let Some(raw) = block.get(off..off + 4) else {
                continue;
            };
            let item = u16::from_le_bytes([raw[0], raw[1]]);
            if item == 0 {
                continue;
            }
            let quantity =
                u16::from_le_bytes([raw[2], raw[3]]) ^ (encryption_key as u16);
            bag.push((pocket as u8, item, quantity));
        }
    }

    let mut party = Vec::new();
    for i in 0..PARTY_SIZE {
        let at = OFFSET_PARTY + i * MON_SIZE;
        if let Some(bytes) = block.get(at..at + MON_SIZE) {
            if let Some(mon) = read_mon(bytes) {
                party.push(mon);
            }
        }
    }

    Some(SaveState { flags, vars, money_raw, coins_raw, encryption_key, party, bag })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an image with one readable slot, so the layout logic is exercised without
    /// needing a real save on disk.
    fn image_with(slot: usize, counter: u32, flags: &[u8], vars: &[u16], money: u32) -> Vec<u8> {
        let mut image = vec![0u8; NUM_SECTORS * SECTOR_SIZE];

        // SaveBlock1 laid out contiguously, then cut into sector-sized pieces.
        let mut block = vec![0u8; SAVEBLOCK1_SECTORS.len() * SECTOR_DATA_SIZE];
        block[OFFSET_MONEY..OFFSET_MONEY + 4].copy_from_slice(&money.to_le_bytes());
        block[OFFSET_FLAGS..OFFSET_FLAGS + flags.len()].copy_from_slice(flags);
        for (i, v) in vars.iter().enumerate() {
            let at = OFFSET_VARS + i * 2;
            block[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }

        for i in 0..SECTORS_PER_SLOT {
            let index = slot * SECTORS_PER_SLOT + i;
            let start = index * SECTOR_SIZE;
            // Deliberately not in id order: the real game rotates them, and reading a slot
            // in position order rather than by id is exactly the bug this guards against.
            let id = ((i + 3) % SECTORS_PER_SLOT) as u16;
            if let Some(n) = SAVEBLOCK1_SECTORS.iter().position(|s| *s == id) {
                let from = n * SECTOR_DATA_SIZE;
                image[start..start + SECTOR_DATA_SIZE]
                    .copy_from_slice(&block[from..from + SECTOR_DATA_SIZE]);
            }
            let footer = start + SECTOR_DATA_SIZE + 116;
            image[footer..footer + 2].copy_from_slice(&id.to_le_bytes());
            image[footer + 4..footer + 8].copy_from_slice(&SECTOR_SIGNATURE.to_le_bytes());
            image[footer + 8..footer + 12].copy_from_slice(&counter.to_le_bytes());
        }

        image
    }

    #[test]
    fn reads_flags_vars_and_money() {
        let mut flags = vec![0u8; FLAG_BYTES];
        flags[0] = 0b1010_1010;
        flags[299] = 0xFF;
        let mut vars = vec![0u16; VAR_COUNT];
        vars[0] = 0x1234;
        vars[255] = 0xBEEF;

        let image = image_with(0, 7, &flags, &vars, 0xDEAD_BEEF);
        let state = parse(&image).expect("should parse");

        assert_eq!(state.flags, flags);
        assert_eq!(state.vars, vars);
        assert_eq!(state.money_raw, 0xDEAD_BEEF);
        // No SaveBlock2 written in this fixture, so the key is zero and money reads through.
        assert_eq!(state.money(), 0xDEAD_BEEF);
    }

    /// The game alternates slots, and reading the older one silently reverts progress.
    #[test]
    fn prefers_the_slot_with_the_higher_counter() {
        let mut old_flags = vec![0u8; FLAG_BYTES];
        old_flags[0] = 0x01;
        let mut new_flags = vec![0u8; FLAG_BYTES];
        new_flags[0] = 0x02;

        let older = image_with(0, 4, &old_flags, &vec![0; VAR_COUNT], 0);
        let newer = image_with(1, 9, &new_flags, &vec![0; VAR_COUNT], 0);
        let mut image = older;
        // Splice the newer slot in alongside the older one.
        let at = SECTORS_PER_SLOT * SECTOR_SIZE;
        image[at..].copy_from_slice(&newer[at..]);

        let state = parse(&image).expect("should parse");
        assert_eq!(state.flags[0], 0x02, "the newer slot should win");
    }

    /// The synthetic fixtures above only prove this file agrees with itself. Point
    /// POKEPLANET_SAVE_FIXTURE at an image the game actually wrote to prove the offsets are
    /// the game's, and POKEPLANET_SAVE_MONEY at what that character's money should be.
    #[test]
    fn reads_a_real_save_the_game_wrote() {
        let Ok(path) = std::env::var("POKEPLANET_SAVE_FIXTURE") else {
            return;
        };
        let image = std::fs::read(&path).expect("fixture should be readable");
        let state = parse(&image).expect("a real save should parse");

        assert_eq!(state.flags.len(), FLAG_BYTES);
        assert_eq!(state.vars.len(), VAR_COUNT);

        // A real save's bag must decode to quantities the game could actually store. This
        // is the check that the quantity obfuscation was undone correctly: a wrong key
        // gives values scattered across the whole 16-bit range, which this catches at once.
        for (pocket, item, quantity) in &state.bag {
            assert!(
                *quantity > 0 && *quantity <= MAX_ITEM_QUANTITY,
                "pocket {} item {} decoded to {}, so the quantity key is wrong",
                pocket, item, quantity
            );
        }

        // Every Pokemon in a save the game wrote should decode to bytes that agree with
        // its own checksum. This is what proves the exclusive-or and the personality shuffle
        // are right, without needing to ask the game for a second opinion.
        for (i, mon) in state.party.iter().enumerate() {
            assert!(
                mon.checksum_ok,
                "party slot {} did not verify: the substruct decode is wrong (species {}, level {})",
                i + 1, mon.species, mon.level
            );
        }

        if let Ok(expected) = std::env::var("POKEPLANET_SAVE_MONEY") {
            let expected: u32 = expected.parse().expect("money should be a number");
            assert_eq!(
                state.money(),
                expected,
                "decoded money should match what the game reports"
            );
        }
    }

    /// An ordinary save is not accused of anything.
    #[test]
    fn an_honest_save_is_not_impossible() {
        let image = image_with(0, 1, &vec![0; FLAG_BYTES], &vec![0; VAR_COUNT], 3000);
        let state = parse(&image).expect("should parse");
        assert_eq!(state.money(), 3000);
        assert_eq!(state.impossible(), None);
    }

    /// Exactly at the cap is reachable by playing, so it must not be refused.
    #[test]
    fn the_cap_itself_is_allowed() {
        let image = image_with(0, 1, &vec![0; FLAG_BYTES], &vec![0; VAR_COUNT], MAX_MONEY);
        let state = parse(&image).expect("should parse");
        assert_eq!(state.impossible(), None, "the cap is reachable, not a cheat");
    }

    /// Above the cap the game clamps to, which no amount of play can produce.
    #[test]
    fn money_above_the_cap_is_impossible() {
        let image = image_with(0, 1, &vec![0; FLAG_BYTES], &vec![0; VAR_COUNT], MAX_MONEY + 1);
        let state = parse(&image).expect("should parse");
        assert!(state.impossible().is_some(), "above the cap should be caught");
    }

    #[test]
    fn refuses_something_that_is_not_a_save() {
        assert!(parse(&[]).is_none());
        assert!(parse(&vec![0u8; 1024]).is_none());
        // Right size, but no signatures anywhere.
        assert!(parse(&vec![0u8; NUM_SECTORS * SECTOR_SIZE]).is_none());
    }

    /// A slot half-written when the power went out must not be read as complete.
    #[test]
    fn refuses_a_slot_with_missing_sectors() {
        let image = image_with(0, 3, &vec![0; FLAG_BYTES], &vec![0; VAR_COUNT], 0);
        let mut broken = image.clone();
        // Wipe one sector's signature.
        let footer = 5 * SECTOR_SIZE + SECTOR_DATA_SIZE + 116;
        broken[footer + 4..footer + 8].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse(&broken).is_none(), "an incomplete slot is not loadable");
    }
}



