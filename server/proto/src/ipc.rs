//! Loopback framing between the `pokeplanet-net` sidecar and the game client.
//!
//! The game side is 32-bit C compiled through the decomp's preprocessor, so this format is
//! deliberately dumb: every record is fixed size, little-endian, with no varints, no
//! pointers and no length-prefixed strings. Text fields are NUL-padded byte arrays the C
//! side can hand straight to its own string routines.
//!
//! Frame layout: `u32 length` (little-endian, counts everything after itself) followed by
//! `u8 msg_type` and a payload whose size is fixed per type.
//!
//! Keep this in sync with `include/net_client.h`.

use crate::{MapId, PlayerId, Pose};

pub const NAME_LEN: usize = 16;
pub const SENDER_LEN: usize = 24;
pub const TEXT_LEN: usize = 200;
pub const URL_LEN: usize = 192;

/// Refuse frames larger than this; the sidecar and game are both trusted-local, but a
/// desynced stream must fail loudly rather than allocate wildly.
pub const MAX_FRAME: usize = 16 * 1024;

// Sidecar -> game
pub const MSG_STATUS: u8 = 0x01;
pub const MSG_SNAPSHOT: u8 = 0x02;
pub const MSG_CHAT: u8 = 0x03;
pub const MSG_PROFILE: u8 = 0x04;
/// Someone challenged this player. Without these three the client can send a challenge but
/// can never learn it has received one, which is why answering was impossible.
pub const MSG_BATTLE_INVITE: u8 = 0x05;
pub const MSG_BATTLE_ANSWERED: u8 = 0x06;
pub const MSG_BATTLE_FAILED: u8 = 0x07;
/// A slice of the server's copy of the save, handed over at sign-in. Layout matches the
/// upload: u32 offset, u32 total, u16 len, bytes.
pub const MSG_SAVE_IMAGE: u8 = 0x08;
/// Both players agreed to battle. Carries the opponent and the slot the server assigned
/// this player, which is what decides who runs the battle engine.
pub const MSG_BATTLE_STARTING: u8 = 0x09;
/// The server refused a step and this is where the player really is.
pub const MSG_CORRECTION: u8 = 0x0A;
/// One block of link-battle traffic from the opponent: u8 from_slot, u16 len, bytes.
pub const MSG_LINK_BLOCK: u8 = 0x0B;

// Game -> sidecar
pub const MSG_SELF_STATE: u8 = 0x81;
pub const MSG_BEGIN_LOGIN: u8 = 0x82;
pub const MSG_CANCEL_LOGIN: u8 = 0x83;
pub const MSG_CHAT_SEND: u8 = 0x84;
pub const MSG_LOGOUT: u8 = 0x85;
/// Challenge another player: u32 player id.
pub const MSG_BATTLE_REQUEST: u8 = 0x86;
/// Answer a challenge: u32 player id, u8 accepted.
pub const MSG_BATTLE_RESPOND: u8 = 0x87;
/// A slice of the save. Sent in pieces because the whole flash image is 128KB and the game
/// must not sit in a single blocking write. Layout: u32 offset, u32 total, u16 len, bytes.
pub const MSG_SAVE_CHUNK: u8 = 0x88;
/// One block of link-battle traffic for the opponent: u16 len, bytes.
pub const MSG_LINK_BLOCK_SEND: u8 = 0x89;
/// The battle this player was in has finished. No payload.
pub const MSG_BATTLE_ENDED: u8 = 0x8A;

/// Mirrors `enum NetAuthState` in the C header.
pub const AUTH_OFFLINE: u8 = 0;
pub const AUTH_CONNECTING: u8 = 1;
pub const AUTH_NEEDS_LOGIN: u8 = 2;
pub const AUTH_AWAITING_BROWSER: u8 = 3;
pub const AUTH_ONLINE: u8 = 4;
/// This character signed in elsewhere. The game shuts itself down rather than sitting
/// there looking connected while the world has moved on without it.
pub const AUTH_SUPERSEDED: u8 = 5;

pub const CHAT_GLOBAL: u8 = 0;
pub const CHAT_LOCAL: u8 = 1;
pub const CHAT_PRIVATE: u8 = 2;

/// Serialized size of one entry in a snapshot frame.
pub const REMOTE_PLAYER_SIZE: usize = 32;

/// Copy `s` into a fixed NUL-padded field, truncating on a char boundary.
fn put_str(out: &mut Vec<u8>, s: &str, width: usize) {
    let mut bytes = s.as_bytes();
    if bytes.len() >= width {
        // Never split a multi-byte character; walk back to a boundary.
        let mut cut = width - 1;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        bytes = &s.as_bytes()[..cut];
    }
    out.extend_from_slice(bytes);
    out.resize(out.len() + (width - bytes.len()), 0);
}

fn take_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Wrap a message body in its length prefix.
pub fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Split one complete frame off the front of `buf`, if there is one.
///
/// Returns the message body (type byte first). Leaves `buf` untouched when the frame is
/// still partial, so the caller can just append more bytes and retry.
pub fn take_frame(buf: &mut Vec<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len == 0 || len > MAX_FRAME {
        anyhow::bail!("implausible IPC frame length {len}");
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let body = buf[4..4 + len].to_vec();
    buf.drain(..4 + len);
    Ok(Some(body))
}

/// Someone has challenged this player. Carries who, so the answer can name them and be
/// routed back to the right person.
pub fn encode_battle_invite(from: PlayerId, from_name: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(1 + 4 + NAME_LEN);
    b.push(MSG_BATTLE_INVITE);
    b.extend_from_slice(&from.to_le_bytes());
    put_str(&mut b, from_name, NAME_LEN);
    frame(b)
}

/// The outcome of a challenge this player sent.
pub fn encode_battle_answered(from: PlayerId, from_name: &str, accepted: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + 4 + NAME_LEN);
    b.push(MSG_BATTLE_ANSWERED);
    b.push(u8::from(accepted));
    b.extend_from_slice(&from.to_le_bytes());
    put_str(&mut b, from_name, NAME_LEN);
    frame(b)
}

/// A challenge could not be delivered: they left, or are already busy.
pub fn encode_battle_failed(reason: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(1 + TEXT_LEN);
    b.push(MSG_BATTLE_FAILED);
    put_str(&mut b, reason, TEXT_LEN);
    frame(b)
}

/// A slice of the server's save, on its way to the game.
pub fn encode_save_image(offset: u32, total: u32, bytes: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(11 + bytes.len());
    b.push(MSG_SAVE_IMAGE);
    b.extend_from_slice(&offset.to_le_bytes());
    b.extend_from_slice(&total.to_le_bytes());
    b.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    b.extend_from_slice(bytes);
    frame(b)
}

pub fn encode_battle_starting(opponent: PlayerId, opponent_name: &str, link_id: u8) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + 4 + NAME_LEN);
    b.push(MSG_BATTLE_STARTING);
    b.push(link_id);
    b.extend_from_slice(&opponent.to_le_bytes());
    put_str(&mut b, opponent_name, NAME_LEN);
    frame(b)
}

/// Put the player back where the server says they are.
pub fn encode_correction(pose: &Pose) -> Vec<u8> {
    let mut b = Vec::with_capacity(8);
    b.push(MSG_CORRECTION);
    b.push(pose.map.group);
    b.push(pose.map.num);
    b.extend_from_slice(&pose.x.to_le_bytes());
    b.extend_from_slice(&pose.y.to_le_bytes());
    b.push(pose.facing);
    b.push(pose.elevation);
    frame(b)
}

pub fn encode_status(state: u8, name: &str, login_url: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + NAME_LEN + URL_LEN);
    b.push(MSG_STATUS);
    b.push(state);
    put_str(&mut b, name, NAME_LEN);
    put_str(&mut b, login_url, URL_LEN);
    frame(b)
}

pub struct SnapshotEntry {
    pub player_id: PlayerId,
    pub name: String,
    pub graphics_id: u8,
    pub pose: Pose,
}

pub fn encode_snapshot(players: &[SnapshotEntry]) -> Vec<u8> {
    let mut b = Vec::with_capacity(3 + players.len() * REMOTE_PLAYER_SIZE);
    b.push(MSG_SNAPSHOT);
    b.extend_from_slice(&(players.len() as u16).to_le_bytes());
    for p in players {
        let start = b.len();
        b.extend_from_slice(&p.player_id.to_le_bytes());
        b.push(p.pose.map.group);
        b.push(p.pose.map.num);
        b.extend_from_slice(&p.pose.x.to_le_bytes());
        b.extend_from_slice(&p.pose.y.to_le_bytes());
        b.push(p.pose.facing);
        b.push(p.graphics_id);
        b.push(p.pose.elevation);
        b.push(u8::from(p.pose.moving));
        put_str(&mut b, &p.name, NAME_LEN);
        // Pad to the fixed stride so the C side can index without parsing.
        b.resize(start + REMOTE_PLAYER_SIZE, 0);
    }
    frame(b)
}

/// The save summary the sign-in screen displays. Fixed layout, so the game reads it
/// field by field with no parser.
pub fn encode_profile(profile: &crate::quic::CharacterProfile) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + 13 + NAME_LEN);
    b.push(MSG_PROFILE);
    b.push(profile.graphics_id);
    b.push(profile.badges);
    b.extend_from_slice(&profile.pokedex_caught.to_le_bytes());
    b.extend_from_slice(&profile.pokedex_seen.to_le_bytes());
    b.extend_from_slice(&profile.play_time_seconds.to_le_bytes());
    b.extend_from_slice(&profile.money.to_le_bytes());
    b.push(profile.map_group);
    b.push(profile.map_num);
    b.extend_from_slice(&profile.x.to_le_bytes());
    b.extend_from_slice(&profile.y.to_le_bytes());
    put_str(&mut b, &profile.name, NAME_LEN);
    frame(b)
}

pub fn encode_chat(kind: u8, from: &str, text: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + SENDER_LEN + TEXT_LEN);
    b.push(MSG_CHAT);
    b.push(kind);
    put_str(&mut b, from, SENDER_LEN);
    put_str(&mut b, text, TEXT_LEN);
    frame(b)
}

/// One block of battle traffic for the game, filed under the sender's link slot.
pub fn encode_link_block(from_slot: u8, bytes: &[u8]) -> Vec<u8> {
    let mut b = vec![MSG_LINK_BLOCK, from_slot];
    b.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    b.extend_from_slice(bytes);
    frame(b)
}

/// A message the game sent us.
#[derive(Debug, Clone)]
pub enum GameMessage {
    SelfState { pose: Pose, graphics_id: u8 },
    BeginLogin,
    CancelLogin,
    ChatSend { kind: u8, target: String, text: String },
    Logout,
    RequestBattle { target: PlayerId },
    RespondToBattle { from: PlayerId, accepted: bool },
    /// One slice of the save. `total` is the whole image, so the receiver knows when it
    /// has all of it without a separate end marker.
    SaveChunk { offset: u32, total: u32, bytes: Vec<u8> },
    /// One block of link-battle traffic, bound for whoever this player is battling.
    LinkBlock { bytes: Vec<u8> },
    /// The battle this player was in has finished.
    BattleEnded,
    /// A game process connected to the sidecar.
    ///
    /// Synthesised locally rather than decoded from a frame: the game cannot send this,
    /// because the point of it is that the game has only just arrived and has said nothing
    /// yet. It carries no wire encoding for the same reason.
    Attached,
}

pub fn decode_game_message(body: &[u8]) -> anyhow::Result<GameMessage> {
    let (&kind, rest) = body
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty IPC frame"))?;
    match kind {
        MSG_BATTLE_ENDED => Ok(GameMessage::BattleEnded),
        MSG_LINK_BLOCK_SEND => {
            if rest.len() < 2 {
                anyhow::bail!("short link block");
            }
            let len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
            if rest.len() < 2 + len {
                anyhow::bail!("truncated link block");
            }
            Ok(GameMessage::LinkBlock {
                bytes: rest[2..2 + len].to_vec(),
            })
        }
        MSG_SELF_STATE => {
            if rest.len() < 10 {
                anyhow::bail!("short SELF_STATE frame ({} bytes)", rest.len());
            }
            Ok(GameMessage::SelfState {
                pose: Pose {
                    map: MapId::new(rest[0], rest[1]),
                    x: i16::from_le_bytes([rest[2], rest[3]]),
                    y: i16::from_le_bytes([rest[4], rest[5]]),
                    facing: rest[6],
                    moving: rest[7] != 0,
                    elevation: rest[9],
                },
                graphics_id: rest[8],
            })
        }
        MSG_BATTLE_REQUEST => {
            if rest.len() < 4 {
                anyhow::bail!("short BATTLE_REQUEST frame ({} bytes)", rest.len());
            }
            Ok(GameMessage::RequestBattle {
                target: u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]),
            })
        }
        MSG_BATTLE_RESPOND => {
            if rest.len() < 5 {
                anyhow::bail!("short BATTLE_RESPOND frame ({} bytes)", rest.len());
            }
            Ok(GameMessage::RespondToBattle {
                from: u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]),
                accepted: rest[4] != 0,
            })
        }
        MSG_SAVE_CHUNK => {
            if rest.len() < 10 {
                anyhow::bail!("short SAVE_CHUNK header ({} bytes)", rest.len());
            }
            let offset = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
            let total = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
            let len = u16::from_le_bytes([rest[8], rest[9]]) as usize;
            if rest.len() < 10 + len {
                anyhow::bail!("SAVE_CHUNK claims {len} bytes but carries {}", rest.len() - 10);
            }
            // A chunk that runs past the end it declares is malformed, and trusting it
            // would size an allocation from the wire.
            if offset as usize + len > total as usize {
                anyhow::bail!("SAVE_CHUNK at {offset}+{len} exceeds its total of {total}");
            }
            Ok(GameMessage::SaveChunk {
                offset,
                total,
                bytes: rest[10..10 + len].to_vec(),
            })
        }
        MSG_BEGIN_LOGIN => Ok(GameMessage::BeginLogin),
        MSG_CANCEL_LOGIN => Ok(GameMessage::CancelLogin),
        MSG_LOGOUT => Ok(GameMessage::Logout),
        MSG_CHAT_SEND => {
            if rest.len() < 1 + SENDER_LEN + TEXT_LEN {
                anyhow::bail!("short CHAT_SEND frame ({} bytes)", rest.len());
            }
            Ok(GameMessage::ChatSend {
                kind: rest[0],
                target: take_str(&rest[1..1 + SENDER_LEN]),
                text: take_str(&rest[1 + SENDER_LEN..1 + SENDER_LEN + TEXT_LEN]),
            })
        }
        other => anyhow::bail!("unknown IPC message type 0x{other:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_a_split_stream() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&encode_status(AUTH_ONLINE, "Ash", "https://x/y"));
        stream.extend_from_slice(&encode_chat(CHAT_GLOBAL, "Brock", "hi"));

        // Feed it a byte at a time; nothing should surface until a frame is whole.
        let mut buf = Vec::new();
        let mut frames = Vec::new();
        for byte in stream {
            buf.push(byte);
            while let Some(f) = take_frame(&mut buf).unwrap() {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][0], MSG_STATUS);
        assert_eq!(frames[1][0], MSG_CHAT);
    }

    #[test]
    fn self_state_survives_the_round_trip() {
        let pose = Pose {
            map: MapId::new(3, 7),
            x: -12,
            y: 340,
            facing: 2,
            elevation: 3,
            moving: true,
        };
        let mut b = vec![MSG_SELF_STATE, pose.map.group, pose.map.num];
        b.extend_from_slice(&pose.x.to_le_bytes());
        b.extend_from_slice(&pose.y.to_le_bytes());
        b.push(pose.facing);
        b.push(1);
        b.push(42);
        b.push(pose.elevation);

        match decode_game_message(&b).unwrap() {
            GameMessage::SelfState { pose: got, graphics_id } => {
                assert_eq!(got, pose);
                assert_eq!(graphics_id, 42);
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    #[test]
    fn snapshot_entries_use_the_fixed_stride() {
        let entry = SnapshotEntry {
            player_id: 9,
            name: "Nurse Joy".into(),
            graphics_id: 5,
            pose: Pose::default(),
        };
        let f = encode_snapshot(std::slice::from_ref(&entry));
        // 4 length + 1 type + 2 count + one padded entry
        assert_eq!(f.len(), 4 + 1 + 2 + REMOTE_PLAYER_SIZE);
    }

    #[test]
    fn oversized_names_are_truncated_not_overflowed() {
        let entry = SnapshotEntry {
            player_id: 1,
            name: "A".repeat(200),
            graphics_id: 0,
            pose: Pose::default(),
        };
        let f = encode_snapshot(std::slice::from_ref(&entry));
        assert_eq!(f.len(), 4 + 1 + 2 + REMOTE_PLAYER_SIZE);
    }

    #[test]
    fn a_nonsense_length_is_rejected_rather_than_allocated() {
        let mut buf = u32::MAX.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert!(take_frame(&mut buf).is_err());
    }
}
