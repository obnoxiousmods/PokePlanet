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
    ///
    /// `mode` selects which world this connection plays: "normal" or "deadman". An account holds a
    /// separate character per mode, so the same token can enter either; the server resolves the
    /// character for (account, mode). An unknown value is treated as "normal".
    Hello {
        protocol_version: u16,
        token: Option<String>,
        client_version: String,
        mode: String,
    },
    /// Ask the server to mint a login ticket so the sidecar can open the browser.
    BeginLogin,
    /// Poll whether the browser half of the Discord flow has completed.
    PollLogin {
        ticket: String,
    },
    /// The player walked onto a different map; the server rescopes their snapshot feed.
    EnterMap {
        map: MapId,
    },
    Chat {
        target: ChatTarget,
        text: String,
    },
    /// Ask another player for a battle. They answer with `RespondToBattle`.
    RequestBattle {
        target: PlayerId,
    },
    /// Answer an outstanding invitation.
    RespondToBattle {
        from: PlayerId,
        accepted: bool,
    },
    Goodbye,
    /// One slice of this character's save. Sent in pieces for the same reason as over the
    /// IPC link: the whole image is 128KB and nothing should sit in a single huge write.
    /// `total` lets the server know when it has all of it without a separate end marker.
    ///
    /// Appended after Goodbye rather than inserted next to the other gameplay messages so
    /// the existing variant numbering does not shift under a client that has not updated.
    SaveUpload {
        offset: u32,
        total: u32,
        bytes: Vec<u8>,
    },
    /// Send this character's profile and stored save again, as they are now.
    ///
    /// The sidecar outlives the game deliberately -- that is what lets a restart skip the
    /// browser -- so the sign-in data it holds is only accurate at the moment it signed in.
    /// A game attaching later needs what is true now, not then.
    ///
    /// Appended so the existing variant numbering does not shift.
    Resync,
    /// One block of link-battle traffic, for whoever this player is battling.
    ///
    /// The battle engine exchanges fixed-size blocks -- party data, chosen moves, the
    /// handshake -- and this carries them verbatim. The server does not interpret them
    /// here; it knows who is battling whom and forwards. `BLOCK_BUFFER_SIZE` in the game
    /// is 256 bytes, which is the ceiling this must respect.
    LinkBlock {
        bytes: Vec<u8>,
    },
    /// This player's battle is over.
    ///
    /// Without it the server only ever unseats someone when they disconnect, so blocks from
    /// a finished battle keep being forwarded to an opponent who has walked away from it.
    BattleEnded,
    /// This character's money is now this.
    ///
    /// Appended so the existing variant numbering does not shift: bincode discriminants are
    /// positional, so inserting one anywhere else silently changes what every older client's
    /// messages decode as.
    MoneyChanged {
        amount: u32,
    },
    /// This character now holds `quantity` of `item`, in `pocket`.
    ///
    /// Appended so the existing variant numbering does not shift.
    ItemChanged {
        pocket: u8,
        item: u16,
        quantity: u16,
    },
    /// The whole party, as the game's own bytes.
    ///
    /// Appended so the existing variant numbering does not shift.
    PartyChanged {
        count: u8,
        mons: Vec<u8>,
    },
    /// One allowlisted region of SaveBlock1.
    ///
    /// Appended so the existing variant numbering does not shift.
    RegionChanged {
        offset: u32,
        bytes: Vec<u8>,
    },
    /// One chunk of a whole save block.
    ///
    /// Appended so the existing variant numbering does not shift.
    BlockChunk {
        block: u8,
        offset: u32,
        total: u32,
        bytes: Vec<u8>,
    },
    /// Key state for a run of consecutive frames.
    ///
    /// Appended so the existing variant numbering does not shift.
    Keys {
        frames: Vec<u16>,
    },
    /// Deadman hard reset: the character has no living Pokemon left anywhere, so it loses
    /// everything and starts over. The server wipes the character's save, boxes, bank and progress.
    ///
    /// Trusted without verification because it can only hurt the sender -- a hard reset destroys
    /// everything the character owns and gains nothing, so no honest or dishonest client benefits
    /// from sending it spuriously.
    ///
    /// Appended so the existing variant numbering does not shift.
    HardReset,
    /// Deposit the whole carried wallet into the PC bank. The server moves the money in its own
    /// copy of the save and answers with `BankState`, so the client never authors the change.
    ///
    /// Appended so the existing variant numbering does not shift.
    BankDeposit,
    /// Withdraw from the PC bank into the carried wallet, up to the money cap. The server answers
    /// with `BankState`.
    ///
    /// Appended so the existing variant numbering does not shift.
    BankWithdraw,
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
    /// Where this character stands, as the server has it. The client adopts this after
    /// loading the save rather than trusting the position inside the save image, so the
    /// two cannot drift apart and argue about it afterwards.
    pub map_group: u8,
    pub map_num: u8,
    pub x: i16,
    pub y: i16,
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
    AuthRequired {
        ticket: String,
        login_url: String,
    },
    /// Response to `PollLogin` while the browser flow is still outstanding.
    LoginPending,
    PlayerJoined {
        player_id: PlayerId,
        name: String,
        graphics_id: u8,
    },
    PlayerLeft {
        player_id: PlayerId,
    },
    Chat {
        from: String,
        target: ChatTarget,
        text: String,
    },
    /// Someone wants to battle you. Answer with `ClientControl::RespondToBattle`.
    BattleInvitation {
        from: PlayerId,
        from_name: String,
    },
    /// The outcome of an invitation you sent.
    BattleInvitationAnswered {
        from: PlayerId,
        from_name: String,
        accepted: bool,
    },
    /// An invitation could not be delivered -- they left, or are already busy.
    BattleInvitationFailed {
        reason: String,
    },
    /// Terminal error; the sidecar drops to offline and reports `reason` to the game.
    Rejected {
        reason: String,
    },
    /// This character signed in somewhere else and that connection now owns it. Distinct
    /// from `Rejected` because retrying cannot help and the token is perfectly good: the
    /// only correct response is for this client to stop.
    ///
    /// Appended rather than inserted so the existing variant numbering does not shift.
    Superseded {
        reason: String,
    },
    /// One slice of the character's stored save, sent at sign-in so the client plays the
    /// server's copy rather than whatever is on this machine.
    SaveImage {
        offset: u32,
        total: u32,
        bytes: Vec<u8>,
    },
    /// Both players agreed to battle. Sent to each of them.
    ///
    /// `link_id` is this player's slot in the battle, and it is the server's job to assign
    /// it rather than the clients'. The game works out who runs the battle engine from
    /// GetMultiplayerId, which on this port reads a register nothing ever writes and so
    /// returns 0 on both machines -- leaving both convinced they are the master. An
    /// externally assigned id is what makes exactly one of them right.
    BattleStarting {
        opponent: PlayerId,
        opponent_name: String,
        link_id: u8,
    },
    /// The client is somewhere the server does not agree with, and this is where it really
    /// is. Sent only when a reported step was refused, so an honest client never sees one.
    ///
    /// Appended, like every variant after Rejected, so numbering never shifts.
    Correction {
        pose: Pose,
    },
    /// One block of link-battle traffic from the player this one is battling.
    ///
    /// `from_slot` is the sender's link id, which is the index the game files the block
    /// under in gBlockRecvBuffer.
    LinkBlock {
        from_slot: u8,
        bytes: Vec<u8>,
    },
    /// The gameplay rates this server runs, sent at sign-in.
    ///
    /// Multipliers on the original game: 1.0 is Emerald exactly. The client applies them so
    /// play feels right; the server keeps them so it can refuse a save that gained more than
    /// they allow. Sending them rather than baking them into the client is what lets a server
    /// be retuned without shipping a new build to everyone.
    ///
    /// Per-species encounter rates are deliberately not here: there are hundreds and the
    /// client needs one at a time, so it asks when it needs to rather than being handed a
    /// table at sign-in.
    Rates {
        experience: f32,
        encounter: f32,
        money: f32,
        items: f32,
        catch: f32,
        shop_price: f32,
        /// Per-species encounter multipliers, `(species, multiplier)`, applied on top of the
        /// global `encounter` rate. Only the species a server actually tunes are sent -- a handful,
        /// not the whole dex -- so a rare Deadman find can be made genuinely rare. Appended to the
        /// struct so an older sidecar's decode of the six scalars is unaffected; server and sidecar
        /// deploy together, so the field is always present in practice.
        #[serde(default)]
        species_encounter: Vec<(u16, f32)>,
    },
    /// The PC bank balance and the authoritative carried money after a deposit/withdraw (and once
    /// at sign-in). The client adopts `carried` as its wallet, so the money move stays entirely on
    /// the server -- a client can never mint money by claiming a withdrawal.
    ///
    /// Appended so the existing variant numbering does not shift.
    BankState {
        bank: u64,
        carried: u32,
    },
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
    /// How many Pokémon this player carries, for their name tag. Appended last so an older field
    /// order still decodes the rest.
    #[serde(default)]
    pub party_count: u8,
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
