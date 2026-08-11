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

pub const SECTOR_SIZE: usize = 4096;
pub const SECTOR_DATA_SIZE: usize = 3968;
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

/// Experience bounds, read out of gExperienceTables in the compiled game rather than
/// transcribed from the macros that generate it.
///
/// The six growth rates need, at level 100: 600000 erratic, 800000 fast, 1000000 medium
/// fast, 1059860 medium slow, 1250000 slow, 1640000 fluctuating. Which curve a species uses
/// is not known here, so only the two bounds that hold whatever it is are used -- no species
/// can pass the highest, and none can reach level 100 below the lowest.
pub const MAX_EXPERIENCE: u32 = 1_640_000;
pub const MIN_EXPERIENCE_AT_MAX_LEVEL: u32 = 600_000;

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
/// Which species this character has seen. A bitfield, one bit per species.
const OFFSET_SEEN: usize = 0x988;
/// Sixty-four counters -- steps taken, battles won, Pokemon caught, and so on.
const OFFSET_GAME_STATS: usize = 0x159C;
const GAME_STAT_COUNT: usize = 64;
/// Berry trees: what is planted where and how far along it is. 0x169C to 0x1A9C.
const OFFSET_BERRY_TREES: usize = 0x169C;
const BERRY_TREE_BYTES: usize = 0x400;
/// Trainer rematch state and the step counter that drives it. 0x9C8 to 0xA2E.
const OFFSET_REMATCHES: usize = 0x9C8;
const REMATCH_BYTES: usize = 0x66;

/// ROUND_BITS_TO_BYTES(NUM_SPECIES) in the game.
const DEX_FLAG_BYTES: usize = 52;
const OFFSET_VARS: usize = 0x139C;

pub const FLAG_BYTES: usize = OFFSET_VARS - OFFSET_FLAGS; // 300
pub const VAR_COUNT: usize = 256;

/// One Pokemon, as much of it as validation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyMon {
    /// Immutable for the life of a Pokemon, and effectively unique, so it is what identifies
    /// the same Pokemon across two saves even after the party has been reordered.
    pub personality: u32,
    pub ot_id: u32,
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
        personality,
        ot_id,
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
    /// Which species have been seen, as the game's own bitfield.
    pub seen: Vec<u8>,
    /// The whole of SaveBlock1, exactly as the game wrote it.
    ///
    /// This is what makes retiring the save image safe. Every other field here exists so the
    /// server can *reason* about progress; this one exists so nothing is lost when it stops
    /// keeping the image. Roughly thirty fields are still unparsed -- mail, the daycare,
    /// secret bases, contest wins -- and rebuilding a save from parsed fields alone would
    /// return them all as zero, permanently and with no copy to restore from.
    ///
    /// Holding the block means the server can reconstruct a save it fully understands the
    /// wrapper of (sectors, counters, checksums) around contents it does not have to
    /// understand at all. Preserving something is a lower bar than interpreting it, and it is
    /// the bar that matters for not destroying somebody's game.
    pub block1: Vec<u8>,
    /// Berry trees and trainer rematch state, kept as the game's own bytes.
    ///
    /// Raw for the same reason flags and vars are: reproducing every structure in the save
    /// would be a great deal of code to answer questions nobody is asking yet, where keeping
    /// the bytes is enough both to notice a change and to put it back.
    pub berry_trees: Vec<u8>,
    pub rematches: Vec<u8>,
    /// The sixty-four game counters, in the game's own order.
    ///
    /// Kept as numbers rather than named, for the same reason flags and vars are kept raw:
    /// naming them would mean encoding what each one counts, and the useful questions -- has
    /// this moved, could it have moved that way -- do not need it.
    pub game_stats: Vec<u32>,
}

/// What could not have happened between two saves of the same character.
///
/// Rules about how *fast* things may be gained need every legitimate source enumerated
/// first, and refusing an honest player is worse than the cheating it would catch. This is
/// the one direction that needs no such list: in this game experience is never taken away,
/// so a Pokemon that has gone backwards is not a Pokemon that was played.
///
/// Matched by personality and trainer id rather than by party position, because reordering,
/// depositing and withdrawing all move Pokemon between slots quite legitimately. A Pokemon
/// present in one save and not the other is not compared at all -- it may have been traded,
/// released or boxed.
pub fn regressed(before: &SaveState, after: &SaveState) -> Option<String> {
    for old in &before.party {
        let Some(new) = after
            .party
            .iter()
            .find(|m| m.personality == old.personality && m.ot_id == old.ot_id)
        else {
            continue;
        };
        // Only compare records both sides could actually decode.
        //
        // `gained_too_fast` and `impossible` already gate on this; this function did not, which
        // made it the strictest check running on the least reliable data. A substruct that fails
        // to decode yields an invented experience number, and an invented number that happens to
        // be low reads as "a Pokemon lost experience" -- rejecting the whole report and, with the
        // save upload retired, discarding real progress over a decode this could not trust
        // anyway. An undecodable record is not evidence of regression.
        if !old.checksum_ok || !new.checksum_ok {
            continue;
        }
        if new.experience < old.experience {
            return Some(format!(
                "a Pokemon lost experience, going from {} to {}",
                old.experience, new.experience
            ));
        }
        if new.level < old.level {
            return Some(format!(
                "a Pokemon lost levels, going from {} to {}",
                old.level, new.level
            ));
        }
    }
    None
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

            // Experience past what the slowest curve asks for level 100 belongs to no
            // species in the game.
            if mon.experience > MAX_EXPERIENCE {
                return Some(format!(
                    "party slot {} has {} experience, above the {} any species can hold",
                    i + 1, mon.experience, MAX_EXPERIENCE
                ));
            }
            // And a Pokemon at the maximum level must have earned at least what the fastest
            // curve asks for it. This is what a level set by hand looks like: the level says
            // one thing and the experience behind it says another.
            if mon.level == MAX_LEVEL && mon.experience < MIN_EXPERIENCE_AT_MAX_LEVEL {
                return Some(format!(
                    "party slot {} is level {} on {} experience, below the {} it would need",
                    i + 1, mon.level, mon.experience, MIN_EXPERIENCE_AT_MAX_LEVEL
                ));
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

/// Reassemble any block from the sectors that hold it, indexed by footer id.
pub fn read_block(image: &[u8], sectors: &[u16]) -> Option<Vec<u8>> {
    let slot = newest_slot(image)?;
    let mut out = Vec::with_capacity(sectors.len() * SECTOR_DATA_SIZE);

    for want in sectors {
        let mut found = None;
        for i in 0..SECTORS_PER_SLOT {
            let sector = read_sector(image, slot * SECTORS_PER_SLOT + i)?;
            if sector.signature == SECTOR_SIGNATURE && sector.id == *want {
                found = Some(sector.data);
                break;
            }
        }
        out.extend_from_slice(found?);
    }

    Some(out)
}

/// Rebuild an image with a replacement for any block, proving authoring is faithful first.
///
/// The same identity check `reauthor` makes, generalised: rewrite the block unchanged and
/// require byte-identical output before trusting a real write. Declining costs the player one
/// update; a wrong write costs them the block.
pub fn reauthor_block(image: &[u8], sectors: &[u16], data: &[u8]) -> Option<Vec<u8>> {
    let original = read_block(image, sectors)?;
    if write_block(image, sectors, &original)? != image {
        return None;
    }
    write_block(image, sectors, data)
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

/// What the client says, against what the server's own run of the game produced.
///
/// This is the point of running the game at all. Every other rule here judges a *result* --
/// a level that is too high, money gained too fast -- and those catch a careless forgery while
/// a careful one simply reports figures that are individually plausible. Replaying the session
/// and comparing removes that freedom: the server is no longer asking whether a number looks
/// reasonable, it has computed its own.
///
/// Returns the first material disagreement, or None when the two accounts match.
///
/// What is deliberately *not* compared matters as much as what is. Playtime, the RNG state, step
/// counters and anything else that turns on wall-clock timing or the exact frame an input landed
/// will differ between two honest runs, and treating those as evidence would accuse everybody.
/// The comparison is restricted to things a player cares about and cannot honestly disagree on:
/// money, the party, and the bag.
pub fn diverged(claimed: &SaveState, computed: &SaveState) -> Option<String> {
    if claimed.money() != computed.money() {
        return Some(format!(
            "money: client says {}, the server's own run produced {}",
            claimed.money(),
            computed.money()
        ));
    }

    for want in &computed.party {
        // Matched by identity, not by slot: reordering a party is not a disagreement.
        let Some(got) = claimed
            .party
            .iter()
            .find(|m| m.personality == want.personality && m.ot_id == want.ot_id)
        else {
            return Some(format!(
                "a Pokemon the server's run has (personality {}) is missing from the client's",
                want.personality
            ));
        };

        // Only records both sides decoded. An undecodable substruct invents numbers, and
        // accusing somebody on the strength of an invented number is worse than missing a cheat.
        if !want.checksum_ok || !got.checksum_ok {
            continue;
        }

        if got.level != want.level {
            return Some(format!(
                "level: client says {}, the server's own run produced {}",
                got.level, want.level
            ));
        }
        if got.experience != want.experience {
            return Some(format!(
                "experience: client says {}, the server's own run produced {}",
                got.experience, want.experience
            ));
        }
    }

    for (pocket, item, quantity) in &computed.bag {
        let got = claimed
            .bag
            .iter()
            .find(|(p, i, _)| p == pocket && i == item)
            .map(|(_, _, q)| *q)
            .unwrap_or(0);
        if got != *quantity {
            return Some(format!(
                "item {item}: client says {got}, the server's own run produced {quantity}"
            ));
        }
    }

    None
}

/// A copy of this save's SaveBlock1 with money set to `amount`.
///
/// Money is stored XOR'd with the save's own key, so setting it means encoding it the way the
/// game would rather than writing the number down. Returned as a block for the caller to hand
/// to `reauthor`, so that producing a candidate and accepting it stay separate steps -- the
/// value still has to survive the same checks an uploaded save does.
pub fn with_money(state: &SaveState, amount: u32) -> Vec<u8> {
    let mut block1 = state.block1.clone();
    if block1.len() >= OFFSET_MONEY + 4 {
        let raw = amount ^ state.encryption_key;
        block1[OFFSET_MONEY..OFFSET_MONEY + 4].copy_from_slice(&raw.to_le_bytes());
    }
    block1
}

/// A copy of this save's SaveBlock1 with one item set to `quantity` in `pocket`.
///
/// A quantity of zero clears the slot, which is how the game empties one -- an item at zero
/// still occupying a slot would show up in the bag as a ghost entry.
///
/// Returns None when the pocket does not exist or is full, rather than dropping the change
/// silently. A player whose bag is full should be told no by the game before it ever reports,
/// so reaching that here means the two disagree, and guessing which is right is how saves get
/// quietly wrong.
pub fn with_item(
    state: &SaveState,
    pocket: u8,
    item: u16,
    quantity: u16,
) -> Option<Vec<u8>> {
    let (at, slots) = *BAG_POCKETS.get(pocket as usize)?;
    let mut block1 = state.block1.clone();

    let mut empty = None;
    for slot in 0..slots {
        let off = at + slot * 4;
        let raw = block1.get(off..off + 4)?;
        let here = u16::from_le_bytes([raw[0], raw[1]]);

        if here == item {
            return Some(write_slot(block1, off, item, quantity, state.encryption_key));
        }
        if here == 0 && empty.is_none() {
            empty = Some(off);
        }
    }

    // Nothing to do: removing an item that is not there is already the state asked for.
    if quantity == 0 {
        return Some(block1);
    }

    let off = empty?;
    block1 = write_slot(block1, off, item, quantity, state.encryption_key);
    Some(block1)
}

fn write_slot(mut block1: Vec<u8>, off: usize, item: u16, quantity: u16, key: u32) -> Vec<u8> {
    let (item, quantity) = if quantity == 0 { (0, 0) } else { (item, quantity) };
    block1[off..off + 2].copy_from_slice(&item.to_le_bytes());
    // Quantities carry the low half of the same key as money, per GetBagItemQuantity.
    let hidden = quantity ^ (key as u16);
    block1[off + 2..off + 4].copy_from_slice(&hidden.to_le_bytes());
    block1
}

/// Where the game keeps how many Pokemon are in the party, immediately before the party.
const OFFSET_PARTY_COUNT: usize = 0x234;

/// A copy of this save's SaveBlock1 with the party replaced.
///
/// The party arrives as the game's own bytes rather than as fields, and that is a deliberate
/// choice rather than laziness. Each Pokemon carries four substructures encrypted with
/// `personality ^ ot_id` and *ordered* by `personality % 24`; re-encoding them server-side means
/// reimplementing both, and that exact decode has already produced a confidently wrong answer
/// once in this codebase -- one that read correctly, passed its test, and would have cost
/// players their Pokemon.
///
/// Carrying the bytes cannot get that wrong, and gives up nothing that matters: this is strictly
/// less than the whole save, and it faces the same level, experience and EV checks an uploaded
/// party does. The server validates what it decodes; it does not have to author what it cannot.
pub fn with_party(state: &SaveState, count: u8, mons: &[u8]) -> Option<Vec<u8>> {
    if count as usize > PARTY_SIZE || mons.len() != PARTY_SIZE * MON_SIZE {
        return None;
    }

    let mut block1 = state.block1.clone();
    block1
        .get_mut(OFFSET_PARTY..OFFSET_PARTY + PARTY_SIZE * MON_SIZE)?
        .copy_from_slice(mons);
    block1
        .get_mut(OFFSET_PARTY_COUNT..OFFSET_PARTY_COUNT + 4)?
        .copy_from_slice(&(count as u32).to_le_bytes());
    Some(block1)
}

/// The parts of SaveBlock1 a client may report directly, as (offset, length).
///
/// This is all of SaveBlock1 in kilobyte chunks, except one protected span (0x234..0x848)
/// holding the party count, the party, money, coins and the bag. Those have their own messages
/// carrying caps, rate ceilings and level consistency checks, and a raw region write would skip
/// every one of them -- so covering them here would quietly become the way to cheat.
///
/// Chunked rather than one entry per field because the fields are not the point: secret bases
/// alone are 4360 bytes, over the wire limit, and enumerating thirty structures by hand is
/// thirty chances to get an offset wrong. Chunks are generated, so the only thing that has to
/// be right is the boundary of the protected span.
///
/// An allowlist rather than an offset the client picks, because "write these bytes at this
/// offset" with no constraint is not a save protocol, it is an arbitrary write into the
/// player's save. Money and the party have their own messages and are deliberately absent
/// here: they carry checks -- caps, rate ceilings, level consistency -- that a raw region
/// write would walk straight past.
pub const REPORTABLE: &[(usize, usize)] = &[
    (0x0, 0x234),
    (0x848, 0x400),
    (0xC48, 0x400),
    (0x1048, 0x400),
    (0x1448, 0x400),
    (0x1848, 0x400),
    (0x1C48, 0x400),
    (0x2048, 0x400),
    (0x2448, 0x400),
    (0x2848, 0x400),
    (0x2C48, 0x400),
    (0x3048, 0x400),
    (0x3448, 0x400),
    (0x3848, 0x400),
    (0x3C48, 0x1B8),
];

/// A copy of this save's SaveBlock1 with one allowlisted region replaced.
///
/// Refuses any (offset, length) not on the list exactly. Not "within" a listed region --
/// exactly, because accepting a subrange lets a caller write one byte at a time at an offset
/// of its choosing, which is the same arbitrary write with extra steps.
pub fn with_region(state: &SaveState, offset: usize, bytes: &[u8]) -> Option<Vec<u8>> {
    if !REPORTABLE.iter().any(|&(at, len)| at == offset && len == bytes.len()) {
        return None;
    }

    let mut block1 = state.block1.clone();
    block1.get_mut(offset..offset + bytes.len())?.copy_from_slice(bytes);
    Some(block1)
}

/// The sectors after the two save slots: Hall of Fame (28, 29), Trainer Hill (30) and the
/// recorded battle (31).
///
/// These sit outside the slot rotation entirely, so none of the block machinery reaches them --
/// it all resolves through `newest_slot`. They were the last thing the save image carried that
/// nothing else did, which is why the upload could not simply be switched off: a player's Hall
/// of Fame would have stopped reaching the server, gone at the next sign-in rather than
/// degraded.
pub const TAIL_SECTORS: std::ops::Range<usize> = 28..32;

/// Replace the tail sectors with what the client reported, verbatim.
///
/// Verbatim including footers, and deliberately so. These sectors are not a struct the server
/// models -- there is no field here it could check -- and their checksums were written by the
/// game that produced them. Recomputing what is not understood is how a save gets confidently
/// corrupted; copying it cannot be wrong in a way that reading it right would have caught.
///
/// Safe to take at face value because nothing here feeds the rules that matter: no money, no
/// party, no bag. A forged Hall of Fame is a vanity lie, not an economic one.
pub fn with_tail(image: &[u8], tail: &[u8]) -> Option<Vec<u8>> {
    let at = TAIL_SECTORS.start * SECTOR_SIZE;
    let len = TAIL_SECTORS.len() * SECTOR_SIZE;
    if image.len() != NUM_SECTORS * SECTOR_SIZE || tail.len() != len {
        return None;
    }

    let mut out = image.to_vec();
    out[at..at + len].copy_from_slice(tail);
    Some(out)
}

/// The blocks a client may report wholesale, by id.
///
/// Ids rather than sector lists on the wire: the client naming which sectors to write is the
/// same arbitrary write the region allowlist exists to prevent, one level up.
///
/// SaveBlock1 is absent. It holds money, the bag and the party, which have their own messages
/// carrying caps and rate ceilings -- accepting it wholesale here would be a way to set them
/// without meeting any of that.
pub fn reportable_block(id: u8) -> Option<&'static [u16]> {
    match id {
        0 => Some(&SAVEBLOCK2_SECTORS),
        1 => Some(&STORAGE_SECTORS),
        // 2 is the tail; it is not a slot block and is handled by with_tail.
        _ => None,
    }
}

/// The game's own checksum: sum of the data as little-endian u32s, folded to sixteen bits.
///
/// Mirrors CalculateChecksum in src/save.c.
fn checksum(data: &[u8], size: usize) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data[..size].chunks_exact(4) {
        sum = sum.wrapping_add(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    ((sum >> 16) as u16).wrapping_add(sum as u16)
}

/// Work out how many bytes of a sector the checksum was taken over, by finding the size that
/// reproduces the checksum already stored in it.
///
/// The game checksums each sector over its *declared* size, which comes from
/// `sizeof(struct SaveBlock1)` in the build that wrote it -- a number that changes whenever the
/// struct changes. Hardcoding it would mean every future change to the game silently producing
/// saves this refuses, so the size is recovered from the image instead of assumed about it.
///
/// Sizes are tried largest first because the full-sector case is overwhelmingly the common one.
fn declared_size(data: &[u8], stored: u16) -> Option<usize> {
    (0..=SECTOR_DATA_SIZE)
        .rev()
        .filter(|n| n % 4 == 0)
        .find(|&n| checksum(data, n) == stored)
}

/// Write a modified SaveBlock1 back into an image, returning the new image.
///
/// This is what lets the server *author* a save rather than only read one, which is the whole
/// prerequisite for the save image stopping being something the client is the origin of.
///
/// Returns None rather than a wrong image whenever anything does not line up. A save that fails
/// to rebuild leaves the player exactly where they were; a save rebuilt wrongly corrupts a
/// character permanently, so every uncertain case takes the first outcome.
pub fn write_block1(image: &[u8], block1: &[u8]) -> Option<Vec<u8>> {
    write_block(image, &SAVEBLOCK1_SECTORS, block1)
}

/// The sectors holding SaveBlock2: the player's name, gender, playtime and encryption key.
pub const SAVEBLOCK2_SECTORS: [u16; 1] = [0];

/// The sectors holding PokemonStorage -- the PC boxes.
///
/// Nine sectors, and by far the largest thing in the save. Until this existed the boxes could
/// not be written at all, which is why the save upload could not be retired: switching it off
/// would have meant every Pokemon in a player's PC silently ceasing to reach the server.
pub const STORAGE_SECTORS: [u16; 9] = [5, 6, 7, 8, 9, 10, 11, 12, 13];

/// Write any run of sectors back into an image.
///
/// The same work `write_block1` was doing, with the sector list as a parameter. Nothing about
/// recovering the checksum size was ever specific to SaveBlock1 -- it reads the size out of the
/// sector in front of it -- so the restriction to one block was an accident of how this grew
/// rather than anything the format requires.
pub fn write_block(image: &[u8], sectors: &[u16], data: &[u8]) -> Option<Vec<u8>> {
    if image.len() != NUM_SECTORS * SECTOR_SIZE
        || data.len() != sectors.len() * SECTOR_DATA_SIZE
    {
        return None;
    }

    let slot = newest_slot(image)?;
    let mut out = image.to_vec();

    for (n, want) in sectors.iter().enumerate() {
        let mut wrote = false;

        for i in 0..SECTORS_PER_SLOT {
            let index = slot * SECTORS_PER_SLOT + i;
            let sector = read_sector(image, index)?;
            if sector.signature != SECTOR_SIGNATURE || sector.id != *want {
                continue;
            }

            let start = index * SECTOR_SIZE;
            let footer = start + SECTOR_DATA_SIZE + 116;
            let stored = u16::from_le_bytes([image[footer + 2], image[footer + 3]]);
            // Recovered from the sector as it stands, before anything is changed in it.
            let size = declared_size(sector.data, stored)?;

            let from = n * SECTOR_DATA_SIZE;
            out[start..start + SECTOR_DATA_SIZE]
                .copy_from_slice(&data[from..from + SECTOR_DATA_SIZE]);

            let fresh = checksum(&out[start..start + SECTOR_DATA_SIZE], size);
            out[footer + 2..footer + 4].copy_from_slice(&fresh.to_le_bytes());
            wrote = true;
            break;
        }

        if !wrote {
            return None;
        }
    }

    Some(out)
}

/// Rebuild an image with the server's own SaveBlock1, refusing unless authoring is provably
/// faithful on this exact image first.
///
/// `declared_size` recovers a size by matching a checksum, and a checksum is sixteen bits, so in
/// principle a wrong size could match by luck. Rather than argue about how unlikely that is on
/// any given save, this rewrites the *unchanged* block first and requires the result to be
/// byte-identical to what came in. If the recovered sizes were wrong, the checksums they produce
/// differ and the identity fails -- on this image, not on average.
///
/// So the guarantee is not "the derivation is sound in theory" but "it was just demonstrated on
/// the save actually in hand", which is the one that protects the player holding it.
pub fn reauthor(image: &[u8], block1: &[u8]) -> Option<Vec<u8>> {
    let original = saveblock1(image, newest_slot(image)?)?;
    let identity = write_block1(image, &original)?;
    if identity != image {
        return None;
    }
    write_block1(image, block1)
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

    let seen = block
        .get(OFFSET_SEEN..OFFSET_SEEN + DEX_FLAG_BYTES)
        .map(|b| b.to_vec())
        .unwrap_or_default();

    let game_stats = block
        .get(OFFSET_GAME_STATS..OFFSET_GAME_STATS + GAME_STAT_COUNT * 4)
        .map(|b| {
            b.chunks_exact(4)
                // Obfuscated with the same key as money, per GetGameStat in the game. Read
                // raw they come out as near-identical nine-digit numbers -- which is what
                // gave this away, since nothing counting steps or battles looks like that.
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ encryption_key)
                .collect()
        })
        .unwrap_or_default();

    let berry_trees = block
        .get(OFFSET_BERRY_TREES..OFFSET_BERRY_TREES + BERRY_TREE_BYTES)
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let rematches = block
        .get(OFFSET_REMATCHES..OFFSET_REMATCHES + REMATCH_BYTES)
        .map(|b| b.to_vec())
        .unwrap_or_default();

    Some(SaveState {
        block1: block.clone(),
        flags,
        vars,
        money_raw,
        coins_raw,
        encryption_key,
        party,
        bag,
        seen,
        game_stats,
        berry_trees,
        rematches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a byte at a SaveBlock1 offset the parser knows nothing about, by finding the
    /// sector that carries it rather than assuming where it sits.
    fn poke(image: &mut [u8], slot: usize, offset: usize, byte: u8) {
        let n = offset / SECTOR_DATA_SIZE;
        let within = offset % SECTOR_DATA_SIZE;
        let want = SAVEBLOCK1_SECTORS[n];
        for i in 0..SECTORS_PER_SLOT {
            let start = (slot * SECTORS_PER_SLOT + i) * SECTOR_SIZE;
            let footer = start + SECTOR_DATA_SIZE + 116;
            let id = u16::from_le_bytes([image[footer], image[footer + 1]]);
            if id == want {
                image[start + within] = byte;
                return;
            }
        }
        panic!("no sector carries offset {offset:#X}");
    }

    /// The whole block is kept, including the parts nothing understands.
    ///
    /// This is the guarantee that lets the save image be retired without losing anything. It
    /// deliberately probes the *mail* offset, which no code here parses and none is planned to:
    /// if preservation only worked for fields that happened to have a parser, it would not be
    /// preservation, it would be a coincidence.
    #[test]
    fn the_whole_block_survives_including_what_is_not_parsed() {
        const OFFSET_MAIL: usize = 0x2BE0;

        let mut image = image_with(0, 1, &[], &[], 0);
        poke(&mut image, 0, OFFSET_MAIL, 0xA7);

        let state = parse(&image).expect("readable save");

        assert_eq!(
            state.block1.len(),
            SAVEBLOCK1_SECTORS.len() * SECTOR_DATA_SIZE,
            "the block should be kept whole, not truncated to the parsed region"
        );
        assert_eq!(
            state.block1[OFFSET_MAIL], 0xA7,
            "a byte no field parses still has to come back"
        );

        // Negative control. Without it this test passes on a block of the right length full of
        // zeroes, or on one that happens to contain 0xA7 everywhere -- both of which would lose
        // the player's mail while reporting success.
        let mut clean = image_with(0, 1, &[], &[], 0);
        poke(&mut clean, 0, OFFSET_MAIL, 0x00);
        let unmarked = parse(&clean).expect("readable save");
        assert_eq!(
            unmarked.block1[OFFSET_MAIL], 0x00,
            "the byte must track the save, not be a constant this test wrote"
        );
    }

    /// Stamp real checksums into a fixture, the way the game would.
    ///
    /// `last` is the size the final SaveBlock1 chunk declares. The real game checksums that one
    /// over only part of the sector, because sizeof(SaveBlock1) is not a multiple of the sector
    /// size, so a fixture where every sector is full would never exercise the case that actually
    /// ships.
    fn sign(image: &mut [u8], slot: usize, last: usize) {
        for i in 0..SECTORS_PER_SLOT {
            let start = (slot * SECTORS_PER_SLOT + i) * SECTOR_SIZE;
            let footer = start + SECTOR_DATA_SIZE + 116;
            let id = u16::from_le_bytes([image[footer], image[footer + 1]]);
            let size = if id == *SAVEBLOCK1_SECTORS.last().unwrap() {
                last
            } else {
                SECTOR_DATA_SIZE
            };
            let sum = checksum(&image[start..start + SECTOR_DATA_SIZE], size);
            image[footer + 2..footer + 4].copy_from_slice(&sum.to_le_bytes());
        }
    }

    /// The server can write a save the game would accept, and change only what it meant to.
    #[test]
    fn authoring_changes_the_one_field_and_leaves_the_rest() {
        let mut image = image_with(0, 1, &[0xFF, 0x0F], &[7, 9], 1234);
        poke(&mut image, 0, 0x2BE0, 0xA7);
        sign(&mut image, 0, 2000);

        let before = parse(&image).expect("readable");

        // Change money the way the server would: in the block, not in the image.
        let mut block1 = before.block1.clone();
        let new_money = 4321u32 ^ before.encryption_key;
        block1[OFFSET_MONEY..OFFSET_MONEY + 4].copy_from_slice(&new_money.to_le_bytes());

        let rebuilt = reauthor(&image, &block1).expect("authoring should succeed");
        let after = parse(&rebuilt).expect("the rebuilt save must still parse");

        assert_eq!(after.money(), 4321, "the field asked for should change");
        assert_eq!(before.money(), 1234, "and it should not have been that already");
        assert_eq!(after.flags, before.flags, "flags must survive authoring");
        assert_eq!(after.vars, before.vars, "vars must survive authoring");
        assert_eq!(
            after.block1[0x2BE0], 0xA7,
            "a field nothing parses must survive being rewritten around"
        );
        assert_eq!(rebuilt.len(), image.len(), "the image must keep its shape");
    }

    /// The checksums written are the ones the game would have written.
    ///
    /// Recomputed here from the game's own algorithm rather than compared against whatever the
    /// code under test produced, so this fails if authoring and reading are wrong *together* --
    /// which is exactly what a self-consistent bug looks like.
    #[test]
    fn authored_checksums_match_the_games_own() {
        const LAST: usize = 2000;
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, LAST);

        let mut block1 = parse(&image).expect("readable").block1;
        block1[OFFSET_MONEY..OFFSET_MONEY + 4].copy_from_slice(&999u32.to_le_bytes());
        let rebuilt = reauthor(&image, &block1).expect("authoring should succeed");

        for i in 0..SECTORS_PER_SLOT {
            let start = i * SECTOR_SIZE;
            let footer = start + SECTOR_DATA_SIZE + 116;
            let id = u16::from_le_bytes([rebuilt[footer], rebuilt[footer + 1]]);
            if !SAVEBLOCK1_SECTORS.contains(&id) {
                continue;
            }
            let size = if id == *SAVEBLOCK1_SECTORS.last().unwrap() {
                LAST
            } else {
                SECTOR_DATA_SIZE
            };
            let want = checksum(&rebuilt[start..start + SECTOR_DATA_SIZE], size);
            let got = u16::from_le_bytes([rebuilt[footer + 2], rebuilt[footer + 3]]);
            assert_eq!(got, want, "sector {id} carries a checksum the game would reject");
        }
    }

    /// Authoring refuses rather than guesses.
    #[test]
    fn authoring_refuses_what_it_cannot_do_faithfully() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);
        let block1 = parse(&image).expect("readable").block1;

        assert!(
            reauthor(&image, &block1[..100]).is_none(),
            "a block of the wrong length must be refused, not padded"
        );
        assert!(
            reauthor(&image[..1000], &block1).is_none(),
            "an image of the wrong length must be refused, not extended"
        );

        // A sector whose checksum matches no size at all: the size cannot be recovered, so
        // authoring cannot know what the game would check, so it must decline.
        let mut broken = image.clone();
        for i in 0..SECTORS_PER_SLOT {
            let footer = i * SECTOR_SIZE + SECTOR_DATA_SIZE + 116;
            let id = u16::from_le_bytes([broken[footer], broken[footer + 1]]);
            if id == SAVEBLOCK1_SECTORS[0] {
                // Non-zero data with a checksum that cannot arise from any prefix of it.
                broken[i * SECTOR_SIZE] = 0x11;
                broken[footer + 2..footer + 4].copy_from_slice(&0xBEEFu16.to_le_bytes());
            }
        }
        assert!(
            reauthor(&broken, &block1).is_none(),
            "an unrecoverable size must stop authoring, not produce a corrupt save"
        );
    }

    /// A reported money value lands in the save as that value, and nothing else moves.
    ///
    /// This is the path that replaces the upload for money: the server builds the save itself
    /// from a number the client reported, instead of being handed an image to inspect.
    #[test]
    fn reported_money_is_written_and_still_checkable() {
        let mut image = image_with(0, 1, &[0xFF, 0x0F], &[7, 9], 1234);
        poke(&mut image, 0, 0x2BE0, 0xA7);
        sign(&mut image, 0, 2000);

        let old = parse(&image).expect("readable");
        assert_eq!(old.money(), 1234, "the starting point must not already be the answer");

        let candidate =
            reauthor(&image, &with_money(&old, 8000)).expect("authoring should succeed");
        let new = parse(&candidate).expect("the rebuilt save must parse");

        assert_eq!(new.money(), 8000, "the reported value should be what the save now holds");
        assert_eq!(new.flags, old.flags, "nothing else should move");
        assert_eq!(new.block1[0x2BE0], 0xA7, "including what nothing parses");

        // The point of building a candidate rather than trusting the number: it can still be
        // judged. A wildly high report is refused by the same rule an uploaded save would be.
        let absurd = reauthor(&image, &with_money(&old, 9_999_999)).expect("authoring");
        let absurd = parse(&absurd).expect("parses");
        assert!(
            absurd.impossible().is_some(),
            "a value above what the game clamps to must still be caught after being written"
        );

        // Negative control: the ordinary value must NOT trip that rule, or the check above
        // would be passing for the trivial reason that everything looks impossible.
        assert!(
            new.impossible().is_none(),
            "a normal amount must pass, otherwise the rule catches nothing in particular"
        );
    }

    /// A reported item lands in the pocket it was reported for, at the count reported.
    #[test]
    fn reported_items_are_written_where_they_belong() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);
        let old = parse(&image).expect("readable");
        assert!(old.bag.is_empty(), "the bag must start empty or this proves nothing");

        // Key items are second in the save even though POCKET_KEY_ITEMS is 5. Writing to
        // pocket 1 and reading it back as pocket 1 is what pins that down: if the write and
        // the read disagreed about the order, an item would surface in the wrong pocket.
        let block1 = with_item(&old, 1, 260, 1).expect("a free slot exists");
        let new = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert_eq!(new.bag, vec![(1u8, 260u16, 1u16)], "one key item, in the key item pocket");

        // The offsets are far enough apart that a pocket mix-up shows up as a different index.
        let block1 = with_item(&old, 4, 133, 7).expect("a free slot exists");
        let new = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert_eq!(new.bag, vec![(4u8, 133u16, 7u16)], "berries land in the berry pocket");

        // Adding to an existing entry sets the count rather than making a second slot.
        let block1 = with_item(&new, 4, 133, 12).expect("the slot is already there");
        let again = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert_eq!(again.bag, vec![(4u8, 133u16, 12u16)], "one slot, updated");

        // Zero clears the slot entirely: an item at zero still occupying one would show up in
        // the bag as an entry the player cannot use and cannot get rid of.
        let block1 = with_item(&again, 4, 133, 0).expect("clearing works");
        let gone = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert!(gone.bag.is_empty(), "a count of zero should leave no slot behind");

        // A pocket that does not exist is refused rather than written somewhere convenient.
        assert!(with_item(&old, 9, 1, 1).is_none(), "there is no ninth pocket");
    }

    /// An over-full slot is caught after being written, by the same rule that catches it in an
    /// uploaded save.
    #[test]
    fn a_reported_item_above_the_cap_is_still_refused() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);
        let old = parse(&image).expect("readable");

        let over = with_item(&old, 0, 13, MAX_ITEM_QUANTITY + 1).expect("writes");
        let over = parse(&reauthor(&image, &over).expect("authoring")).expect("parses");
        assert!(
            over.impossible().is_some(),
            "more than a slot can hold must be caught however it arrived"
        );

        // Negative control: the largest legitimate amount must pass, or the check above is
        // just rejecting everything and proving nothing about the cap.
        let at_cap = with_item(&old, 0, 13, MAX_ITEM_QUANTITY).expect("writes");
        let at_cap = parse(&reauthor(&image, &at_cap).expect("authoring")).expect("parses");
        assert!(
            at_cap.impossible().is_none(),
            "a full but legal slot must be accepted"
        );
    }

    /// A reported party is written, and is still judged by the rules an uploaded one meets.
    #[test]
    fn a_reported_party_is_written_and_still_checked() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);
        let old = parse(&image).expect("readable");
        assert!(old.party.is_empty(), "the party must start empty or this proves nothing");

        // One Pokemon at a legal level. Level lives at 0x54 within the hundred-byte record.
        let mut mons = vec![0u8; PARTY_SIZE * MON_SIZE];
        mons[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // personality
        mons[0x54] = 50;

        let block1 = with_party(&old, 1, &mons).expect("a party of one fits");
        let new = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert_eq!(new.party.len(), 1, "one Pokemon should have been written");
        assert_eq!(new.party[0].level, 50, "at the level reported");
        assert!(new.impossible().is_none(), "a level 50 Pokemon is ordinary");

        // Above the level cap: caught after being written, by the rule that catches it in an
        // uploaded save rather than by a second rule written for this path.
        let mut cheated = mons.clone();
        cheated[0x54] = 200;
        let block1 = with_party(&old, 1, &cheated).expect("writes");
        let bad = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert!(
            bad.impossible().is_some(),
            "a level the game cannot reach must be refused however it arrived"
        );

        // Refusals rather than guesses about size.
        assert!(with_party(&old, 1, &mons[..10]).is_none(), "a short party must be refused");
        assert!(with_party(&old, 9, &mons).is_none(), "more than six must be refused");
    }

    /// The allowlist, written out as literals.
    ///
    /// The game carries the same table in src/mmo_autosave.c, and the two are matched by value
    /// rather than shared, because one is C and the other Rust. A mismatch does not crash or
    /// warn -- the server simply refuses that region forever and the field silently stops being
    /// saved, which is the worst way for this to fail. Spelling the numbers out here means
    /// changing a constant breaks this test instead of quietly breaking the game.
    #[test]
    fn the_allowlist_is_what_the_game_was_told() {
        assert_eq!(
            REPORTABLE,
            &[
                (0x0, 0x234),
                (0x848, 0x400),
                (0xC48, 0x400),
                (0x1048, 0x400),
                (0x1448, 0x400),
                (0x1848, 0x400),
                (0x1C48, 0x400),
                (0x2048, 0x400),
                (0x2448, 0x400),
                (0x2848, 0x400),
                (0x2C48, 0x400),
                (0x3048, 0x400),
                (0x3448, 0x400),
                (0x3848, 0x400),
                (0x3C48, 0x1B8),
            ][..],
            "if this changed, src/mmo_autosave.c must change with it"
        );
    }

    /// Regions are written when listed, and refused otherwise.
    #[test]
    fn only_listed_regions_can_be_written() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);
        let old = parse(&image).expect("readable");

        // The chunk that happens to contain the story flags, found rather than assumed, so this
        // keeps working when the chunk boundaries move.
        let &(at, len) = REPORTABLE
            .iter()
            .find(|&&(at, len)| at <= OFFSET_FLAGS && OFFSET_FLAGS < at + len)
            .expect("the flags must be inside some reportable chunk");

        let mut chunk = old.block1[at..at + len].to_vec();
        chunk[OFFSET_FLAGS - at] = 0xAB;
        let block1 = with_region(&old, at, &chunk).expect("a listed chunk is writable");
        let new = parse(&reauthor(&image, &block1).expect("authoring")).expect("parses");
        assert_eq!(new.flags[0], 0xAB, "the reported chunk should be what the save now holds");
        assert_ne!(old.flags[0], 0xAB, "and must not have been that already");

        // An offset nobody listed. Without this check it is an arbitrary write into the
        // player's save at a position the client chooses.
        assert!(
            with_region(&old, at + 1, &chunk).is_none(),
            "an unlisted offset must be refused"
        );
        // A subrange of a listed chunk is still not a listed chunk: allowing it would let a
        // caller write one byte at a time wherever it liked, which is the same arbitrary write.
        assert!(
            with_region(&old, at, &chunk[..8]).is_none(),
            "a partial chunk must be refused, not padded or merged"
        );

        // The protected span: money, coins, the bag and the party have their own messages,
        // which carry caps and rate ceilings a raw region write would skip. No chunk may
        // overlap them, or this whole mechanism becomes the way around those checks.
        for &(at, len) in REPORTABLE {
            assert!(
                at + len <= 0x234 || at >= 0x848,
                "chunk at {at:#X} overlaps the protected span"
            );
        }
        assert!(
            with_region(&old, OFFSET_MONEY, &[0u8; 4]).is_none(),
            "money must not be writable as a raw region"
        );
    }

    /// The PC boxes and SaveBlock2 can be written, not only SaveBlock1.
    ///
    /// This is the capability whose absence blocked retiring the save upload. The boxes live in
    /// their own nine sectors, so authoring that only covered SaveBlock1 meant switching the
    /// upload off would have stopped every Pokemon in a player's PC from reaching the server --
    /// not degraded, gone at the next sign-in.
    #[test]
    fn any_block_can_be_authored_not_just_saveblock1() {
        let mut image = image_with(0, 1, &[0xFF], &[3], 100);
        sign(&mut image, 0, 2000);

        for sectors in [&STORAGE_SECTORS[..], &SAVEBLOCK2_SECTORS[..]] {
            let original = read_block(&image, sectors).expect("the block should be readable");
            assert_eq!(
                original.len(),
                sectors.len() * SECTOR_DATA_SIZE,
                "a block should come back whole"
            );

            let mut changed = original.clone();
            changed[0] = 0x5A;
            changed[original.len() - 1] = 0xC3;

            let rebuilt = reauthor_block(&image, sectors, &changed).expect("authoring");
            let back = read_block(&rebuilt, sectors).expect("readable after writing");
            assert_eq!(back, changed, "what was written should be what comes back");
            assert_eq!(rebuilt.len(), image.len(), "the image must keep its shape");

            // Negative control: without it this passes on a no-op that returns the input.
            assert_ne!(
                back, original,
                "the block must actually have changed, or this proves nothing"
            );

            // SaveBlock1 must be untouched by writing a different block -- the sectors are
            // interleaved in the slot, so a wrong index lands in someone else's data.
            assert_eq!(
                read_block(&rebuilt, &SAVEBLOCK1_SECTORS).unwrap(),
                read_block(&image, &SAVEBLOCK1_SECTORS).unwrap(),
                "writing one block must not disturb another"
            );

            assert!(
                reauthor_block(&image, sectors, &changed[..10]).is_none(),
                "a block of the wrong length must be refused"
            );
        }
    }

    /// A state carrying one Pokemon, for reasoning about regression directly.
    fn with_mon(level: u8, experience: u32, checksum_ok: bool) -> SaveState {
        SaveState {
            flags: vec![],
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
            party: vec![PartyMon {
                personality: 7,
                ot_id: 7,
                species: 1,
                level,
                experience,
                evs: [0; 6],
                checksum_ok,
            }],
        }
    }

    /// Regression is judged only on records that decoded, and is still judged.
    ///
    /// The negative control is the whole point of this test. Skipping undecodable records is a
    /// one-line change that could just as easily have disabled the check altogether, and a
    /// disabled no-going-backwards rule is invisible until somebody uses it to roll a character
    /// back. So this asserts both directions: a real regression is still caught, and only the
    /// undecodable case is waved through.
    #[test]
    fn regression_is_ignored_only_when_the_record_did_not_decode() {
        let good_high = with_mon(20, 8000, true);
        let good_low = with_mon(10, 2000, true);

        // Negative control: the check must still fire on decodable records.
        assert!(
            regressed(&good_high, &good_low).is_some(),
            "losing experience must still be caught -- if this passes, the rule is off"
        );
        // And it must not fire on ordinary forward progress.
        assert!(
            regressed(&good_low, &good_high).is_none(),
            "gaining experience is not a regression"
        );

        // The actual change: an undecodable record on either side is not evidence. Its level and
        // experience are whatever the failed decode invented, so comparing them is meaningless.
        assert!(
            regressed(&good_high, &with_mon(10, 2000, false)).is_none(),
            "an undecodable new record must not be read as a loss"
        );
        assert!(
            regressed(&with_mon(20, 8000, false), &good_low).is_none(),
            "an undecodable old record must not be read as a loss"
        );

        // Level, not just experience -- both comparisons needed gating.
        let mut level_drop = with_mon(10, 8000, true);
        level_drop.party[0].experience = good_high.party[0].experience;
        assert!(
            regressed(&good_high, &level_drop).is_some(),
            "losing levels must still be caught independently of experience"
        );
    }

    /// Divergence is reported when the two accounts differ, and not when they agree.
    #[test]
    fn divergence_is_found_only_where_it_exists() {
        let a = with_mon(20, 8000, true);

        // Negative control first: identical states must not be an accusation. Without this a
        // function that always reported divergence would look like it was catching cheats.
        assert!(diverged(&a, &a).is_none(), "identical accounts must agree");

        // A level the client claims but the server's run did not produce.
        let mut lying = a.clone();
        lying.party[0].level = 60;
        assert!(diverged(&lying, &a).is_some(), "an invented level must be caught");

        // Experience likewise.
        let mut richer = a.clone();
        richer.party[0].experience = 999_999;
        assert!(diverged(&richer, &a).is_some(), "invented experience must be caught");

        // An undecodable record is not evidence either way -- same rule as `regressed`.
        let mut undecodable = a.clone();
        undecodable.party[0].level = 60;
        undecodable.party[0].checksum_ok = false;
        assert!(
            diverged(&undecodable, &a).is_none(),
            "a record that did not decode must not become an accusation"
        );

        // Reordering is not divergence: a second Pokemon, then the same two the other way round.
        let mut two = a.clone();
        let mut second = two.party[0].clone();
        second.personality = 99;
        second.ot_id = 99;
        two.party.push(second);
        let mut swapped = two.clone();
        swapped.party.swap(0, 1);
        assert!(
            diverged(&swapped, &two).is_none(),
            "reordering a party is not a disagreement"
        );
    }

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




