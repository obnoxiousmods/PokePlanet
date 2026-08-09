//! Wire types shared by `pymerald-server` and the `pymerald-net` client sidecar.
//!
//! Two distinct encodings live here on purpose:
//!
//! * [`quic`] — Rust-to-Rust over QUIC. bincode, free to evolve, versioned by
//!   [`PROTOCOL_VERSION`].
//! * [`ipc`] — sidecar-to-game over loopback. Fixed-layout little-endian records so the
//!   32-bit C client can parse them with a struct cast and no allocator.

pub mod ipc;
pub mod quic;

/// Bumped on any incompatible change to [`quic`]. The server refuses mismatched clients.
pub const PROTOCOL_VERSION: u16 = 1;

/// Maximum remote players the server will describe in a single snapshot.
///
/// The game can only render a handful: `OBJECT_EVENTS_COUNT` bounds live object events and
/// `MAX_SPRITES` (64) bounds OAM entries, both shared with the map's own NPCs.
pub const MAX_VISIBLE_PLAYERS: usize = 8;

/// Identifies one map instance. Mirrors the game's `gSaveBlock1Ptr->location`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct MapId {
    pub group: u8,
    pub num: u8,
}

impl MapId {
    pub fn new(group: u8, num: u8) -> Self {
        Self { group, num }
    }
}

/// Where a character is standing, in map tile coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Pose {
    pub map: MapId,
    pub x: i16,
    pub y: i16,
    /// Game direction constant: 1=south, 2=north, 3=west, 4=east.
    pub facing: u8,
    pub elevation: u8,
    /// True while the avatar is mid-step, so observers animate a walk instead of a warp.
    pub moving: bool,
}

/// Server-assigned identity for a connected character.
pub type PlayerId = u32;
