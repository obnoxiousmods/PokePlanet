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

/// Offsets within SaveBlock1. From the annotated struct in include/global.h.
const OFFSET_MONEY: usize = 0x490;
const OFFSET_FLAGS: usize = 0x1270;
const OFFSET_VARS: usize = 0x139C;

pub const FLAG_BYTES: usize = OFFSET_VARS - OFFSET_FLAGS; // 300
pub const VAR_COUNT: usize = 256;

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
    /// The key the save was written with, from SaveBlock2.
    pub encryption_key: u32,
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

    Some(SaveState { flags, vars, money_raw, encryption_key })
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

        if let Ok(expected) = std::env::var("POKEPLANET_SAVE_MONEY") {
            let expected: u32 = expected.parse().expect("money should be a number");
            assert_eq!(
                state.money(),
                expected,
                "decoded money should match what the game reports"
            );
        }
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
