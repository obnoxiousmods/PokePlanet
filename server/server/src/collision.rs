//! Where a player is allowed to stand.
//!
//! Built from the game's own map data by `tools/export_collision.py`, which reads the same
//! layout files the client builds from: each entry there is a u16 with collision packed into
//! bits 10-11, so nothing is inferred or re-derived and the server cannot quietly disagree
//! with the game about what a wall is. The exported table was checked against the client's
//! own MapGridGetCollisionAt before being trusted.
//!
//! One bit per tile is small: all 518 maps come to about 42KB, so this lives in memory and a
//! lookup is an array index rather than anything that touches the database.

use std::collections::HashMap;
use std::path::Path;

/// Runtime coordinates include a border the layout does not: the client reports a position
/// with this already added, so it comes back off before indexing the grid.
const MAP_OFFSET: i16 = 7;

const MAGIC: &[u8; 4] = b"PPCL";

struct MapCollision {
    width: u16,
    height: u16,
    /// One bit per tile, set when solid.
    bits: Vec<u8>,
}

impl MapCollision {
    /// Whether a runtime coordinate is actually on this map.
    ///
    /// Distinct from `blocked`, and the distinction is the whole point. `blocked` treats
    /// off-the-edge as walkable so map connections work -- you are briefly off one layout while
    /// stepping onto the next. That is right for a step, and wrong for deciding whether a
    /// position is a place at all: an interior map has no connections, so off its edge is
    /// nowhere, and a position nowhere must never be stored or handed back at sign-in.
    fn in_bounds(&self, x: i16, y: i16) -> bool {
        x >= 0 && y >= 0 && x < self.width as i16 && y < self.height as i16
    }

    fn blocked(&self, x: i16, y: i16) -> bool {
        if x < 0 || y < 0 || x >= self.width as i16 || y >= self.height as i16 {
            // Off the edge of the layout. Connections mean this is a normal thing to see for
            // a step or two while crossing between maps, so it is not treated as a wall --
            // refusing here would stop players using the routes.
            return false;
        }
        let index = y as usize * self.width as usize + x as usize;
        self.bits[index / 8] >> (index % 8) & 1 == 1
    }
}

#[derive(Default)]
pub struct Collision {
    maps: HashMap<(u8, u8), MapCollision>,
}

impl Collision {
    /// Read the exported table. A missing file is not fatal: the server still runs and still
    /// refuses teleports, it just cannot tell a wall from a path.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read(path)?;
        anyhow::ensure!(
            data.len() >= 8,
            "collision table is too short to hold a header"
        );
        anyhow::ensure!(&data[..4] == MAGIC, "collision table has the wrong magic");

        let count = u16::from_le_bytes([data[6], data[7]]) as usize;
        let mut maps = HashMap::with_capacity(count);
        let mut at = 8usize;

        for _ in 0..count {
            anyhow::ensure!(at + 6 <= data.len(), "collision table ends mid-record");
            let group = data[at];
            let num = data[at + 1];
            let width = u16::from_le_bytes([data[at + 2], data[at + 3]]);
            let height = u16::from_le_bytes([data[at + 4], data[at + 5]]);
            at += 6;

            let bytes = (width as usize * height as usize).div_ceil(8);
            anyhow::ensure!(at + bytes <= data.len(), "collision table ends mid-map");
            maps.insert(
                (group, num),
                MapCollision {
                    width,
                    height,
                    bits: data[at..at + bytes].to_vec(),
                },
            );
            at += bytes;
        }

        Ok(Self { maps })
    }

    pub fn len(&self) -> usize {
        self.maps.len()
    }

    /// A collision table of all-walkable maps of the given sizes, for tests in other modules.
    /// Each tuple is (group, num, width, height); runtime coords 7..width+7 are on the map.
    #[cfg(test)]
    pub(crate) fn for_test(maps: &[(u8, u8, u16, u16)]) -> Self {
        let mut m = std::collections::HashMap::new();
        for &(group, num, width, height) in maps {
            let bytes = (width as usize * height as usize).div_ceil(8);
            m.insert(
                (group, num),
                MapCollision {
                    width,
                    height,
                    bits: vec![0u8; bytes],
                },
            );
        }
        Self { maps: m }
    }

    /// True when this position is a real place on this map.
    ///
    /// Unknown maps pass, for the same reason `walkable` lets them: a map the table does not
    /// cover must not become a cage. What this does catch is a position on a map the table
    /// *does* know, that is off the edge of it -- which is not a tile, however it was arrived at.
    ///
    /// This exists because nothing checked it. A pose is accepted verbatim on a map change, and
    /// persisted fifteen seconds later, so town coordinates recorded against an interior map
    /// were stored and handed back at the next sign-in. Professor Birch's lab is 13x13; its door
    /// in Littleroot is at runtime (14, 23), an ordinary town position four tiles below the
    /// bottom of the lab. That is the invalid spawn.
    pub fn in_bounds(&self, group: u8, num: u8, x: i16, y: i16) -> bool {
        match self.maps.get(&(group, num)) {
            Some(map) => map.in_bounds(x - MAP_OFFSET, y - MAP_OFFSET),
            None => true,
        }
    }

    /// True when a player may stand here. Unknown maps are allowed rather than refused: a
    /// map the table does not cover should not become an invisible cage.
    pub fn walkable(&self, group: u8, num: u8, x: i16, y: i16) -> bool {
        match self.maps.get(&(group, num)) {
            Some(map) => !map.blocked(x - MAP_OFFSET, y - MAP_OFFSET),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_map(width: u16, height: u16, solid: &[(i16, i16)]) -> Collision {
        let mut bits = vec![0u8; (width as usize * height as usize).div_ceil(8)];
        for &(x, y) in solid {
            let i = y as usize * width as usize + x as usize;
            bits[i / 8] |= 1 << (i % 8);
        }
        let mut maps = HashMap::new();
        maps.insert(
            (0, 9),
            MapCollision {
                width,
                height,
                bits,
            },
        );
        Collision { maps }
    }

    #[test]
    fn a_solid_tile_is_not_walkable() {
        // Solid at layout (3,4), which the client would report as (10,11).
        let c = one_map(20, 20, &[(3, 4)]);
        assert!(!c.walkable(0, 9, 3 + MAP_OFFSET, 4 + MAP_OFFSET));
        assert!(c.walkable(0, 9, 4 + MAP_OFFSET, 4 + MAP_OFFSET));
    }

    #[test]
    fn off_the_layout_is_allowed_so_connections_still_work() {
        let c = one_map(20, 20, &[]);
        assert!(c.walkable(0, 9, -5, 3));
        assert!(c.walkable(0, 9, 500, 3));
    }

    #[test]
    fn an_unknown_map_is_not_a_cage() {
        let c = one_map(20, 20, &[]);
        assert!(c.walkable(3, 3, 10, 10));
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    fn one_map(width: u16, height: u16) -> Collision {
        let bytes = (width as usize * height as usize).div_ceil(8);
        let mut maps = HashMap::new();
        maps.insert(
            (1u8, 4u8),
            MapCollision {
                width,
                height,
                bits: vec![0u8; bytes],
            },
        );
        Collision { maps }
    }

    /// A position off the edge of a known map is not a place, and one on it still is.
    ///
    /// The negative control is the point: refusing everything would "fix" the invalid spawn by
    /// making every position invalid, which would strand every player instead of one.
    #[test]
    fn off_the_edge_is_refused_and_ordinary_positions_are_not() {
        // Birch's lab: 13x13 layout, so runtime 7..=19.
        let c = one_map(13, 13);

        assert!(c.in_bounds(1, 4, 7, 7), "the top-left tile is on the map");
        assert!(
            c.in_bounds(1, 4, 19, 19),
            "the bottom-right tile is on the map"
        );
        assert!(
            c.in_bounds(1, 4, 13, 14),
            "somewhere in the middle is on the map"
        );

        // The reported failure: the lab's door in Littleroot is runtime (14, 23). Fine in a
        // 20x20 town, four tiles past the bottom of a 13x13 lab.
        assert!(
            !c.in_bounds(1, 4, 14, 23),
            "town coordinates are not on the lab"
        );
        assert!(
            !c.in_bounds(1, 4, 20, 19),
            "one tile past the right edge is off the map"
        );
        assert!(!c.in_bounds(1, 4, 6, 7), "above the border is off the map");

        // A map the table does not know must not become a cage.
        assert!(
            c.in_bounds(9, 9, 1000, 1000),
            "unknown maps are not policed"
        );
    }
}
