//! Messages exchanged between the client sidecar and the server over QUIC.
//!
//! Control traffic ([`ClientControl`] / [`ServerControl`]) rides a reliable bidirectional
//! stream. Movement ([`ClientMovement`] / [`ServerSnapshot`]) rides unreliable datagrams,
//! because a dropped position is superseded by the next one ~100ms later and retransmitting
//! it would only add latency.

use crate::{MapId, PlayerId, Pose, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};

/// How the player is currently authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthState {
    Offline,
    Connecting,
    /// The server issued a login ticket; the player must complete the Discord flow.
    NeedsLogin,
    AwaitingBrowser,
    Online,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientControl {
    /// First message on the control stream. `token` is a previously issued session token.
    Hello {
        protocol_version: u16,
        token: Option<String>,
        client_version: String,
    },
    /// Ask the server to mint a login ticket so the sidecar can open the browser.
    BeginLogin,
    /// Poll whether the browser half of the Discord flow has completed.
    PollLogin { ticket: String },
    /// The player walked onto a different map; the server rescopes their snapshot feed.
    EnterMap { map: MapId },
    Chat { target: ChatTarget, text: String },
    Goodbye,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatTarget {
    /// The default in-game channel, bridged to IRC `#pokeplanet`.
    Global,
    /// Everyone standing on the same map.
    Local,
    /// A private message, bridged to an IRC PM.
    Private(String),
}

/// The save-file summary shown on the sign-in screen, straight from the server.
///
/// This is the authoritative record of a character's progress; the client displays it
/// rather than reading anything locally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CharacterProfile {
    pub name: String,
    /// Overworld sprite assigned to this character at creation.
    pub graphics_id: u8,
    pub play_time_seconds: u32,
    pub badges: u8,
    pub pokedex_caught: u16,
    pub pokedex_seen: u16,
    pub money: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerControl {
    /// Authentication succeeded; the character is live.
    Welcome {
        player_id: PlayerId,
        profile: CharacterProfile,
        /// Persist this and send it in `Hello` next launch to skip the browser.
        token: String,
    },
    /// No usable token. The player must visit `login_url` to finish the Discord flow.
    AuthRequired { ticket: String, login_url: String },
    /// Response to `PollLogin` while the browser flow is still outstanding.
    LoginPending,
    PlayerJoined { player_id: PlayerId, name: String, graphics_id: u8 },
    PlayerLeft { player_id: PlayerId },
    Chat { from: String, target: ChatTarget, text: String },
    /// Terminal error; the sidecar drops to offline and reports `reason` to the game.
    Rejected { reason: String },
}

/// Position report, sent by the client at roughly 10Hz on a datagram.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClientMovement {
    pub pose: Pose,
}

/// Positions of every other character sharing the sender's map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSnapshot {
    pub players: Vec<RemotePlayer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePlayer {
    pub player_id: PlayerId,
    pub name: String,
    pub graphics_id: u8,
    pub pose: Pose,
}

/// bincode configuration used for every QUIC payload.
fn codec() -> impl bincode::Options {
    use bincode::Options;
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_varint_encoding()
        // A hostile peer must not be able to make us allocate arbitrarily.
        .with_limit(64 * 1024)
}

pub fn encode<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    use bincode::Options;
    Ok(codec().serialize(value)?)
}

pub fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> anyhow::Result<T> {
    use bincode::Options;
    Ok(codec().deserialize(bytes)?)
}

/// Client and server must agree exactly; there is no compatibility window yet.
pub fn version_is_compatible(client: u16) -> bool {
    client == PROTOCOL_VERSION
}
