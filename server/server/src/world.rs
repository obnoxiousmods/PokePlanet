//! Live world state: who is online, where they are standing, and chat routing.
//!
//! This is deliberately in-memory and rebuilt on restart. Durable progress lives in
//! Postgres; presence is ephemeral and cheap to reconstruct when clients reconnect.

use pokeplanet_proto::quic::{ChatTarget, RemotePlayer, ServerControl};
use pokeplanet_proto::{MapId, PlayerId, Pose, MAX_VISIBLE_PLAYERS};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Distinguishes two connections for the same character.
///
/// A player can be connected twice at once -- a reconnect that races its own teardown, or
/// a second copy of the game. Without this, the older connection's cleanup would evict the
/// live one from the world and silently stop its snapshots.
pub type SessionId = u64;

static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_session_id() -> SessionId {
    NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct Presence {
    pub session: SessionId,
    pub character_id: i64,
    pub name: String,
    pub graphics_id: u8,
    pub pose: Pose,
    /// How many Pokémon this player carries, shown on their name tag. Seeded from the save at
    /// join and kept current by party reports.
    pub party_count: u8,
    /// How many gym badges this character has, seeded at join. Deadman PvP is gated to within two
    /// badges of each other; slightly stale (a badge earned mid-session is not reflected until the
    /// next connect) but only ever an approximate matchmaking bound, never a correctness rule.
    pub badges: u8,
    /// Which world this character plays: "normal" or "deadman". PvP badge gating only applies
    /// between two deadman characters.
    pub mode: String,
    /// Control-stream sink for this connection.
    pub control: mpsc::Sender<ServerControl>,
    /// Who has challenged this player and is awaiting an answer.
    pub pending_invite: Option<PlayerId>,
    /// The battle this player is in, if any.
    ///
    /// Battle traffic is forwarded rather than broadcast, and this is the only thing that
    /// says where to. Recorded on both sides when an invitation is accepted, so a block
    /// can never reach someone who is not in the battle it belongs to.
    pub battle: Option<BattleSeat>,
    /// The next reported position is taken as given rather than checked.
    ///
    /// True on joining and after a map change, because a warp legitimately puts the
    /// player anywhere on the new map: a door leads to a specific tile, not to the one
    /// adjacent to where they were standing. Without this, walking into a building is
    /// a map change followed by a report from wherever the door leads, which looks
    /// exactly like a teleport and gets refused.
    pub position_unknown: bool,
}

/// A player's place in a battle.
///
/// One value rather than two fields, because a peer without a slot (or the wrong slot) is
/// not a state that should be representable: the slot decides which machine runs the battle
/// engine, and getting it wrong means either both do or neither does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BattleSeat {
    /// The other player.
    pub peer: PlayerId,
    /// This player's link id. Slot 0 runs the engine; slot 1 follows it.
    pub slot: u8,
}

#[derive(Default)]
pub struct World {
    players: RwLock<HashMap<PlayerId, Presence>>,
    /// Who is on which map.
    ///
    /// Snapshots go out to every player ten times a second, and each one only ever
    /// concerns the handful of people sharing that map. Without this every snapshot
    /// walks the entire player table, which is fine for five players and quadratic for
    /// a thousand: at that size it is ten million comparisons a second, all of them
    /// holding the same lock.
    by_map: RwLock<HashMap<MapId, HashSet<PlayerId>>>,
    /// Where players may stand. Empty when the table could not be loaded, in which case
    /// steps are still checked for being single steps, just not for hitting walls.
    collision: crate::collision::Collision,
}

pub type SharedWorld = Arc<World>;

impl World {
    pub fn new() -> SharedWorld {
        Arc::new(World::default())
    }

    pub fn with_collision(collision: crate::collision::Collision) -> SharedWorld {
        Arc::new(World {
            collision,
            ..Default::default()
        })
    }

    pub async fn join(&self, id: PlayerId, presence: Presence) {
        let announcement = ServerControl::PlayerJoined {
            player_id: id,
            name: presence.name.clone(),
            graphics_id: presence.graphics_id,
        };
        let map = presence.pose.map;
        let displaced = {
            let mut players = self.players.write().await;
            players.insert(id, presence)
        };
        {
            let mut by_map = self.by_map.write().await;
            if let Some(old) = displaced.as_ref() {
                if let Some(set) = by_map.get_mut(&old.pose.map) {
                    set.remove(&id);
                }
            }
            by_map.entry(map).or_default().insert(id);
        }

        // One character cannot be in two places. The old connection was already being
        // ignored from here on -- update_pose and session_is_current both check the
        // session -- but nothing ever told it, so it sat there looking online while the
        // world moved on without it. Say so, and let it close itself.
        if let Some(old) = displaced {
            let _ = old
                .control
                .send(ServerControl::Superseded {
                    reason: "signed in from somewhere else".to_string(),
                })
                .await;
        }

        self.broadcast_except(id, announcement).await;
    }

    /// Remove a player, but only if the presence still belongs to `session`.
    ///
    /// A reconnecting client briefly has two connections. The older one tearing down must
    /// not evict the newer one, or the live session stops receiving snapshots while still
    /// believing it is online.
    pub async fn leave(&self, id: PlayerId, session: SessionId) {
        let removed = {
            let mut players = self.players.write().await;
            match players.get(&id) {
                Some(p) if p.session == session => {
                    let map = p.pose.map;
                    // Read who they were battling before removing them, or their opponent
                    // is left seated in a battle against somebody who no longer exists.
                    let peer = p.battle.map(|s| s.peer);
                    let gone = players.remove(&id).is_some();
                    if gone {
                        if let Some(set) = self.by_map.write().await.get_mut(&map) {
                            set.remove(&id);
                        }
                        if let Some(peer) = peer {
                            if let Some(other) = players.get_mut(&peer) {
                                // Only if they still name this player, since they may
                                // already have started another battle.
                                if other.battle.map(|s| s.peer) == Some(id) {
                                    other.battle = None;
                                }
                            }
                        }
                    }
                    gone
                }
                _ => false,
            }
        };
        if removed {
            self.broadcast_except(id, ServerControl::PlayerLeft { player_id: id })
                .await;
        }
    }

    /// Update a pose, ignoring reports from a connection that has been superseded.
    ///
    /// Returns the pose the server considers real. When that differs from what was
    /// reported, the client is out of step and has to be corrected.
    ///
    /// The client used to be believed outright, which made position the easiest thing in
    /// the game to cheat: a patched client could stand anywhere, on any map, instantly.
    /// A step is now only accepted if it continues from the position the server already
    /// accepted, which needs no knowledge of the map at all and still rules out teleporting
    /// and moving faster than the game can walk.
    ///
    /// A step also has to land somewhere the map allows, using collision exported from
    /// the game's own layout data, so walking through a wall is refused as well.
    pub async fn update_pose(&self, id: PlayerId, session: SessionId, pose: Pose) -> Option<Pose> {
        let mut players = self.players.write().await;
        let p = players.get_mut(&id)?;
        if p.session != session {
            return None;
        }

        // Nothing to continue from yet, so there is nothing to check against.
        if p.position_unknown {
            // Read the old map before overwriting it, or the reindex below moves the player
            // from the map they are already on to itself and leaves them listed on the one
            // they left -- a ghost standing somewhere nobody is.
            let was = p.pose.map;
            p.position_unknown = false;
            p.pose = pose;
            drop(players);
            if was != pose.map {
                self.reindex(id, was, pose.map).await;
            }
            return Some(pose);
        }

        // A map change is the one legitimate way to appear somewhere unrelated. Warps are
        // still the client's call for now, so this trusts it; once warps are server-side
        // this becomes the check that they were adjacent to a real door.
        if pose.map != p.pose.map {
            // On the map change -- and ONLY here, not on every step -- refuse a coordinate that
            // is not on the destination map. This is the one place a pose carrying the previous
            // map's coordinates can enter; checking it on every step instead flooded corrections
            // and refused legitimate tiles whenever a map's runtime layout differs from the
            // exported one (secret bases, the Battle Pyramid). Refusing keeps the old pose and
            // re-arms position_unknown so the next honest report settles it, rather than
            // persisting a bad coordinate.
            if !self
                .collision
                .in_bounds(pose.map.group, pose.map.num, pose.x, pose.y)
            {
                tracing::warn!(
                    player = id,
                    group = pose.map.group,
                    num = pose.map.num,
                    x = pose.x,
                    y = pose.y,
                    "refusing a map-change onto a coordinate that is not on that map"
                );
                p.position_unknown = true;
                return Some(p.pose);
            }
            let was = p.pose.map;
            p.pose = pose;
            // The tile they arrive on is decided by the door, and the report carrying it
            // may not be this one, so take the next report as given too.
            p.position_unknown = true;
            drop(players);
            self.reindex(id, was, pose.map).await;
            return Some(pose);
        }

        // Widen before subtracting/abs: both operands are client-controlled i16, and abs() on
        // i16::MIN panics just as the subtraction can overflow. i32 makes every reported pose safe.
        let dx = (pose.x as i32 - p.pose.x as i32).abs();
        let dy = (pose.y as i32 - p.pose.y as i32).abs();

        // Standing still, turning, or a single step along one axis. Diagonals are not a
        // thing the player avatar can do.
        let legal = (dx == 0 && dy == 0)
            || (dx + dy == 1
                && self
                    .collision
                    .walkable(pose.map.group, pose.map.num, pose.x, pose.y));
        if legal {
            p.pose = pose;
            Some(pose)
        } else {
            // Refused. Hand back where they actually are so the client can put them there.
            Some(p.pose)
        }
    }

    /// Move a player between the per-map sets.
    async fn reindex(&self, id: PlayerId, from: MapId, to: MapId) {
        let mut by_map = self.by_map.write().await;
        if let Some(set) = by_map.get_mut(&from) {
            set.remove(&id);
        }
        by_map.entry(to).or_default().insert(id);
    }

    /// True while this session is still the live one for its character.
    pub async fn session_is_current(&self, id: PlayerId, session: SessionId) -> bool {
        self.players
            .read()
            .await
            .get(&id)
            .is_some_and(|p| p.session == session)
    }

    pub async fn pose_of(&self, id: PlayerId) -> Option<Pose> {
        self.players.read().await.get(&id).map(|p| p.pose)
    }

    pub async fn online_count(&self) -> usize {
        self.players.read().await.len()
    }

    /// Everyone else standing on `map`, capped to what the client can actually render.
    ///
    /// When more players share a map than the game has object-event slots, the nearest
    /// ones win — a crowd should thin out at the edges rather than pop in and out at
    /// random, which is what an arbitrary hash-order truncation would look like.
    pub async fn snapshot(&self, viewer: PlayerId, map: MapId, from: Pose) -> Vec<RemotePlayer> {
        let here = match self.by_map.read().await.get(&map) {
            Some(set) => set.clone(),
            None => return Vec::new(),
        };

        let players = self.players.read().await;
        let mut visible: Vec<&Presence> = here
            .iter()
            .filter(|id| **id != viewer)
            .filter_map(|id| players.get(id))
            .collect();

        if visible.len() > MAX_VISIBLE_PLAYERS {
            visible.sort_by_key(|p| {
                // Widen before subtracting: pose.x/y are client-controlled i16 and a hostile
                // client can plant -32768, so an i16 subtraction here would overflow (panic under
                // overflow-checks, wrap in release) on a value another player never chose.
                let dx = p.pose.x as i32 - from.x as i32;
                let dy = p.pose.y as i32 - from.y as i32;
                dx * dx + dy * dy
            });
            visible.truncate(MAX_VISIBLE_PLAYERS);
        }

        visible
            .into_iter()
            .map(|p| RemotePlayer {
                player_id: p.character_id as PlayerId,
                name: p.name.clone(),
                graphics_id: p.graphics_id,
                pose: p.pose,
                party_count: p.party_count,
            })
            .collect()
    }

    /// Update a player's carried-Pokémon count so their name tag reflects a party change. A no-op
    /// if they are not online, which is the same as any other update to a departed player.
    pub async fn set_party_count(&self, id: PlayerId, count: u8) {
        if let Some(p) = self.players.write().await.get_mut(&id) {
            p.party_count = count;
        }
    }

    /// Deliver a chat message according to its target. Returns false if a private
    /// message had no recipient online.
    /// Deliver a line to whoever should see it. `from_id` identifies the sender; `from` is
    /// only what recipients are shown.
    pub async fn route_chat(
        &self,
        from_id: PlayerId,
        from: &str,
        target: &ChatTarget,
        text: &str,
    ) -> bool {
        let msg = ServerControl::Chat {
            from: from.to_string(),
            target: target.clone(),
            text: text.to_string(),
        };
        let players = self.players.read().await;

        match target {
            ChatTarget::Global => {
                for p in players.values() {
                    let _ = p.control.try_send(msg.clone());
                }
                true
            }
            ChatTarget::Local => {
                // Scope to the sender's own map, found by id rather than by name: names are
                // unique, but looking one up to answer a question the caller already knows
                // the answer to is both slower and a way for the two to disagree.
                let Some(origin) = players.get(&from_id).map(|p| p.pose.map) else {
                    return false;
                };
                for p in players.values().filter(|p| p.pose.map == origin) {
                    let _ = p.control.try_send(msg.clone());
                }
                true
            }
            ChatTarget::Private(to) => {
                // Case-insensitively, matching how names are kept unique, so addressing
                // someone does not require reproducing their capitalisation.
                let mut delivered = false;
                for (id, p) in players.iter() {
                    let is_recipient = p.name.eq_ignore_ascii_case(to);
                    if !is_recipient && *id != from_id {
                        continue;
                    }
                    // The sender gets a copy so their own whisper appears in their log.
                    let _ = p.control.try_send(msg.clone());
                    if is_recipient {
                        delivered = true;
                    }
                }
                delivered
            }
        }
    }

    /// Push a control message to everyone except one player.
    async fn broadcast_except(&self, skip: PlayerId, msg: ServerControl) {
        for (id, p) in self.players.read().await.iter() {
            if *id != skip {
                // try_send: a client too backed up to accept an announcement is already
                // being torn down; blocking the whole world on it would be worse.
                let _ = p.control.try_send(msg.clone());
            }
        }
    }

    /// Offer a battle to another player.
    ///
    /// Returns Err with a player-facing reason when it cannot be delivered. Invitations
    /// are refused across maps because the two avatars must be standing together for the
    /// battle to make sense in the overworld, and refused when either side already has one
    /// outstanding so a player cannot be spammed into a battle they did not choose.
    pub async fn invite_to_battle(&self, from: PlayerId, target: PlayerId) -> Result<(), String> {
        if from == target {
            return Err("You can't battle yourself.".into());
        }
        let players = self.players.read().await;
        let inviter = players.get(&from).ok_or("You are not online.")?;
        let invitee = players.get(&target).ok_or("They are no longer online.")?;

        if inviter.pose.map != invitee.pose.map {
            return Err("They have left this area.".into());
        }
        // Deadman PvP is gated to within two badges of each other, so a veteran cannot farm a
        // newcomer. Only applies when both are in the deadman world; normal-mode battles are open.
        if inviter.mode == "deadman"
            && invitee.mode == "deadman"
            && !crate::deadman::pvp_in_badge_range(inviter.badges, invitee.badges)
        {
            return Err("They are too far from your badge rank to battle.".into());
        }
        if invitee.pending_invite.is_some() {
            return Err("They are already being challenged.".into());
        }

        invitee
            .control
            .try_send(ServerControl::BattleInvitation {
                from,
                from_name: inviter.name.clone(),
            })
            .map_err(|_| "They aren't responding.".to_string())?;
        drop(players);

        if let Some(p) = self.players.write().await.get_mut(&target) {
            p.pending_invite = Some(from);
        }
        Ok(())
    }

    /// Answer an invitation. Returns Err if it is no longer outstanding.
    pub async fn answer_battle(
        &self,
        responder: PlayerId,
        from: PlayerId,
        accepted: bool,
    ) -> Result<(), String> {
        {
            let mut players = self.players.write().await;
            let me = players.get_mut(&responder).ok_or("You are not online.")?;
            // Only clear an invitation that is actually the one being answered, so a
            // stale reply cannot cancel a newer challenge from someone else.
            if me.pending_invite != Some(from) {
                return Err("That invitation has expired.".into());
            }
            me.pending_invite = None;
        }

        let players = self.players.read().await;
        let responder_name = players
            .get(&responder)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let inviter = players.get(&from).ok_or("They are no longer online.")?;
        let inviter_name = inviter.name.clone();
        let _ = inviter
            .control
            .try_send(ServerControl::BattleInvitationAnswered {
                from: responder,
                from_name: responder_name.clone(),
                accepted,
            });

        if accepted {
            // Assign the slots here rather than letting the clients decide. The one who
            // issued the challenge takes slot 0, which is the slot the game treats as the
            // master; left to themselves both machines would claim it.
            let _ = inviter.control.try_send(ServerControl::BattleStarting {
                opponent: responder,
                opponent_name: responder_name,
                link_id: 0,
            });
            if let Some(me) = players.get(&responder) {
                let _ = me.control.try_send(ServerControl::BattleStarting {
                    opponent: from,
                    opponent_name: inviter_name,
                    link_id: 1,
                });
            }
        }
        drop(players);

        // Seat them only after both have been told, and with the same slots they were told,
        // so what the server forwards by can never disagree with what the clients believe.
        if accepted {
            let mut players = self.players.write().await;
            if let Some(p) = players.get_mut(&from) {
                p.battle = Some(BattleSeat {
                    peer: responder,
                    slot: 0,
                });
            }
            if let Some(p) = players.get_mut(&responder) {
                p.battle = Some(BattleSeat {
                    peer: from,
                    slot: 1,
                });
            }
        }
        Ok(())
    }

    /// End the battle this player is in, on both sides.
    ///
    /// Taken on the player's word, which is safe because the only thing a seat grants is
    /// being able to send blocks to one specific person who agreed to receive them. Ending
    /// a battle early is a thing a player can already do by walking out of the game, and it
    /// costs the other side nothing they had.
    pub async fn clear_battle(&self, id: PlayerId) {
        let mut players = self.players.write().await;
        let peer = players
            .get_mut(&id)
            .and_then(|p| p.battle.take())
            .map(|s| s.peer);
        if let Some(peer) = peer {
            if let Some(p) = players.get_mut(&peer) {
                // Only if they still name this player; they may already have moved on to
                // another battle, and unseating them from that one would break it.
                if p.battle.map(|s| s.peer) == Some(id) {
                    p.battle = None;
                }
            }
        }
    }

    /// Forward one block of battle traffic to whoever the sender is battling.
    ///
    /// Returns false when there is nobody to forward to, which is the normal answer for a
    /// client that keeps sending after its opponent has gone. The bytes are not interpreted:
    /// the battle engine's blocks mean nothing to the server, and it only has to know who
    /// they belong to.
    pub async fn route_link_block(&self, from: PlayerId, bytes: Vec<u8>) -> bool {
        let players = self.players.read().await;
        let Some(me) = players.get(&from) else {
            return false;
        };
        let Some(seat) = me.battle else {
            return false;
        };
        let Some(peer) = players.get(&seat.peer) else {
            return false;
        };
        // Peers must agree they are in the same battle. Without this a stale seat left by a
        // player who has already started another battle would leak blocks into it.
        if peer.battle.map(|s| s.peer) != Some(from) {
            return false;
        }
        peer.control
            .try_send(ServerControl::LinkBlock {
                from_slot: seat.slot,
                bytes,
            })
            .is_ok()
    }

    /// Send one control message to a single player.
    pub async fn tell(&self, id: PlayerId, msg: ServerControl) {
        if let Some(p) = self.players.read().await.get(&id) {
            let _ = p.control.try_send(msg);
        }
    }

    /// Deliver a message that originated outside the game, e.g. from IRC.
    pub async fn inject_chat(&self, from: &str, text: &str) {
        // Text arriving from IRC is as untrusted as text from a game client, and until now it
        // skipped the cleaning every game message gets: an IRC user could inject control
        // characters or an over-long line straight into everyone's chat, and a crafted nick
        // could carry the same. Run both through the same sanitizer, and drop a message that
        // is empty once cleaned.
        let from = crate::quic::sanitize_chat(from);
        let text = crate::quic::sanitize_chat(text);
        if from.is_empty() || text.is_empty() {
            return;
        }
        let msg = ServerControl::Chat {
            from,
            target: ChatTarget::Global,
            text,
        };
        for p in self.players.read().await.values() {
            let _ = p.control.try_send(msg.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(
        character_id: i64,
        name: &str,
        x: i16,
        y: i16,
    ) -> (Presence, mpsc::Receiver<ServerControl>) {
        let (tx, rx) = mpsc::channel(16);
        (
            Presence {
                session: next_session_id(),
                character_id,
                name: name.to_string(),
                graphics_id: 7,
                pose: Pose {
                    map: MapId::new(1, 4),
                    x,
                    y,
                    ..Default::default()
                },
                party_count: 0,
                badges: 0,
                mode: "normal".to_string(),
                control: tx,
                pending_invite: None,
                battle: None,
                position_unknown: true,
            },
            rx,
        )
    }

    /// A map change onto a coordinate that is not on the destination map is refused; onto a
    /// real one it is accepted. Ordinary steps never touch this path.
    ///
    /// The negative control is the second half: if the refusal fired on a valid tile too it
    /// would strand every player at every door, which is the live rubber-band this replaced.
    #[tokio::test]
    async fn a_map_change_onto_an_off_map_tile_is_refused() {
        // (0,9) is 13x13 -> runtime 7..=19; (0,10) is 20x20 -> runtime 7..=26.
        let world = World::with_collision(crate::collision::Collision::for_test(&[
            (0, 9, 13, 13),
            (0, 10, 20, 20),
        ]));
        let (a, _ra) = presence(1, "Ash", 10, 10);
        world.join(1, a).await;
        let session = world.players.read().await[&1].session;

        // Settle onto (0,10) at a valid tile.
        let start = Pose {
            map: MapId::new(0, 10),
            x: 10,
            y: 10,
            ..Default::default()
        };
        assert_eq!(world.update_pose(1, session, start).await, Some(start));

        // Change to (0,9) carrying a town coordinate four tiles past its bottom edge: refused,
        // and the player keeps the pose they had rather than being flung off the map.
        let off = Pose {
            map: MapId::new(0, 9),
            x: 14,
            y: 23,
            ..Default::default()
        };
        assert_eq!(
            world.update_pose(1, session, off).await,
            Some(start),
            "an off-map map-change must be refused, keeping the old pose"
        );

        // The same change onto a real tile of (0,9) is accepted.
        let ok = Pose {
            map: MapId::new(0, 9),
            x: 12,
            y: 12,
            ..Default::default()
        };
        assert_eq!(
            world.update_pose(1, session, ok).await,
            Some(ok),
            "an on-map map-change must be accepted"
        );
    }

    #[tokio::test]
    async fn arriving_through_a_door_is_not_mistaken_for_a_teleport() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 5, 5);
        world.join(1, a).await;
        let session = world.players.read().await[&1].session;

        // Settle in: the join grace is used up by the first report.
        let start = Pose {
            map: MapId::new(1, 4),
            x: 5,
            y: 5,
            ..Default::default()
        };
        world.update_pose(1, session, start).await;

        // Walk into a building. The map changes, and the tile arrived on is wherever the
        // door leads -- nowhere near the tile outside it.
        let doorway = Pose {
            map: MapId::new(9, 1),
            x: 4,
            y: 8,
            ..Default::default()
        };
        assert_eq!(world.update_pose(1, session, doorway).await, Some(doorway));

        // The report that actually carries the arrival tile can be the next one, and it
        // must not be read as a teleport across the new map.
        let inside = Pose {
            map: MapId::new(9, 1),
            x: 12,
            y: 2,
            ..Default::default()
        };
        assert_eq!(
            world.update_pose(1, session, inside).await,
            Some(inside),
            "arriving through a door was refused as a teleport"
        );

        // Once settled, the rules apply again on the new map.
        let jump = Pose {
            map: MapId::new(9, 1),
            x: 40,
            y: 40,
            ..Default::default()
        };
        let answer = world.update_pose(1, session, jump).await.unwrap();
        assert_ne!(answer, jump, "a teleport inside the building was accepted");
    }

    #[tokio::test]
    async fn walking_to_another_map_moves_you_in_the_index() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 5, 5);
        let (b, _rb) = presence(2, "Misty", 6, 5);
        world.join(1, a).await;
        world.join(2, b).await;
        let session = world.players.read().await[&1].session;

        let here = MapId::new(1, 4);
        let there = MapId::new(2, 7);

        // They start together and can see each other.
        assert_eq!(world.snapshot(2, here, Pose::default()).await.len(), 1);

        let moved = Pose {
            map: there,
            x: 5,
            y: 5,
            ..Default::default()
        };
        world.update_pose(1, session, moved).await;

        // Gone from the old map, present on the new one. An index left un-updated shows
        // this as a ghost standing on a map nobody is on.
        assert!(world.snapshot(2, here, Pose::default()).await.is_empty());
        assert_eq!(world.snapshot(2, there, Pose::default()).await.len(), 1);
    }

    #[tokio::test]
    async fn a_single_step_is_accepted_and_a_teleport_is_not() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 10, 10);
        world.join(1, a).await;
        let session = world.players.read().await[&1].session;

        let step = Pose {
            map: MapId::new(1, 4),
            x: 10,
            y: 11,
            ..Default::default()
        };
        assert_eq!(
            world.update_pose(1, session, step).await,
            Some(step),
            "a one-tile step should be accepted"
        );

        // Across the map in one report: the classic teleport.
        let jump = Pose {
            map: MapId::new(1, 4),
            x: 40,
            y: 60,
            ..Default::default()
        };
        let answer = world.update_pose(1, session, jump).await.unwrap();
        assert_ne!(answer, jump, "a teleport was accepted");
        assert_eq!(
            (answer.x, answer.y),
            (10, 11),
            "the refusal should hand back the last position the server accepted"
        );

        // And the refusal must not have moved them either.
        assert_eq!(world.pose_of(1).await.map(|p| (p.x, p.y)), Some((10, 11)));
    }

    #[tokio::test]
    async fn a_diagonal_step_is_refused() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 5, 5);
        world.join(1, a).await;
        let session = world.players.read().await[&1].session;

        // Settle first: the report right after joining is taken as given, because that is
        // the client telling the server where its save puts it.
        let start = Pose {
            map: MapId::new(1, 4),
            x: 5,
            y: 5,
            ..Default::default()
        };
        world.update_pose(1, session, start).await;

        // The player avatar cannot move diagonally, so one report covering both axes is
        // two steps' worth of distance in one tick.
        let diagonal = Pose {
            map: MapId::new(1, 4),
            x: 6,
            y: 6,
            ..Default::default()
        };
        let answer = world.update_pose(1, session, diagonal).await.unwrap();
        assert_ne!(answer, diagonal, "a diagonal step was accepted");
    }

    #[tokio::test]
    async fn accepting_gives_each_side_a_different_battle_slot() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 1, 0);
        world.join(1, a).await;
        world.join(2, b).await;

        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, true).await.unwrap();

        let slot_of = |rx: &mut mpsc::Receiver<ServerControl>| {
            let mut found = None;
            while let Ok(msg) = rx.try_recv() {
                if let ServerControl::BattleStarting { link_id, .. } = msg {
                    found = Some(link_id);
                }
            }
            found
        };

        let challenger = slot_of(&mut ra).expect("the challenger was not told to battle");
        let accepter = slot_of(&mut rb).expect("the accepter was not told to battle");

        // The whole point: exactly one of them runs the battle engine.
        assert_eq!(challenger, 0, "the challenger should hold the master slot");
        assert_ne!(
            challenger, accepter,
            "both sides were given the same slot, so both would claim to be master"
        );
    }

    #[tokio::test]
    async fn declining_starts_no_battle() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 1, 0);
        world.join(1, a).await;
        world.join(2, b).await;

        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, false).await.unwrap();

        while let Ok(msg) = ra.try_recv() {
            assert!(
                !matches!(msg, ServerControl::BattleStarting { .. }),
                "a declined challenge still started a battle"
            );
        }
    }

    #[tokio::test]
    async fn signing_in_again_tells_the_old_connection_to_stop() {
        let world = World::new();
        let (first, mut first_rx) = presence(1, "Ash", 0, 0);
        let (second, _second_rx) = presence(1, "Ash", 0, 0);

        world.join(1, first).await;
        world.join(1, second).await;

        match first_rx.try_recv() {
            Ok(ServerControl::Superseded { .. }) => {}
            other => panic!("the displaced connection was not told to stop: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_surviving_connection_is_not_told_to_stop() {
        let world = World::new();
        let (first, _first_rx) = presence(1, "Ash", 0, 0);
        let (second, mut second_rx) = presence(1, "Ash", 0, 0);

        world.join(1, first).await;
        world.join(1, second).await;

        // Whatever the newcomer hears, it must not be an instruction to close itself.
        while let Ok(msg) = second_rx.try_recv() {
            assert!(
                !matches!(msg, ServerControl::Superseded { .. }),
                "the live connection was told to stop"
            );
        }
    }

    #[tokio::test]
    async fn a_snapshot_excludes_the_viewer() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 3, 3);
        world.join(1, a).await;
        world.join(2, b).await;

        let seen = world
            .snapshot(
                1,
                MapId::new(1, 4),
                Pose {
                    map: MapId::new(1, 4),
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].name, "Misty");
    }

    #[tokio::test]
    async fn players_on_another_map_are_not_visible() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (mut b, _rb) = presence(2, "Misty", 0, 0);
        b.pose.map = MapId::new(9, 9);
        world.join(1, a).await;
        world.join(2, b).await;

        let seen = world.snapshot(1, MapId::new(1, 4), Pose::default()).await;
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn an_overfull_map_keeps_the_nearest_players() {
        let world = World::new();
        let (me, _rm) = presence(0, "Me", 0, 0);
        world.join(0, me).await;
        // Twenty players strung out along the x axis, furthest inserted first.
        for i in 1..=20i16 {
            let (p, rx) = presence(i as i64, &format!("P{i}"), 100 - i, 0);
            std::mem::forget(rx); // keep the channel open for the test's lifetime
            world.join(i as PlayerId, p).await;
        }

        let seen = world.snapshot(0, MapId::new(1, 4), Pose::default()).await;
        assert_eq!(seen.len(), MAX_VISIBLE_PLAYERS);
        // The closest is at x = 100 - 20 = 80.
        let nearest = seen.iter().map(|p| p.pose.x).min().unwrap();
        assert_eq!(nearest, 80);
    }

    #[tokio::test]
    async fn a_private_message_reaches_sender_and_recipient_only() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 0, 0);
        let (c, mut rc) = presence(3, "Brock", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        world.join(3, c).await;
        // Drain the join announcements.
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}
        while rc.try_recv().is_ok() {}

        let ok = world
            .route_chat(1, "Ash", &ChatTarget::Private("Misty".into()), "hey")
            .await;
        assert!(ok);
        assert!(ra.try_recv().is_ok(), "sender should see their own PM");
        assert!(rb.try_recv().is_ok(), "recipient should receive the PM");
        assert!(
            rc.try_recv().is_err(),
            "third party must not receive the PM"
        );
    }

    /// Names are matched without regard to case, so addressing someone does not depend on
    /// reproducing their capitalisation.
    #[tokio::test]
    async fn a_private_message_finds_its_recipient_whatever_the_case() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}

        let ok = world
            .route_chat(1, "Ash", &ChatTarget::Private("mIsTy".into()), "hey")
            .await;
        assert!(ok);
        assert!(rb.try_recv().is_ok(), "recipient should receive the PM");
    }

    /// Local chat reaches the sender's own map and stops there. The sender is identified by
    /// id, so this holds even if someone else were somehow carrying the same name.
    #[tokio::test]
    async fn local_chat_stays_on_the_senders_map() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 0, 0);
        let (mut c, mut rc) = presence(3, "Brock", 0, 0);
        c.pose.map = MapId::new(0, 9); // somewhere else entirely
        world.join(1, a).await;
        world.join(2, b).await;
        world.join(3, c).await;
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}
        while rc.try_recv().is_ok() {}

        let ok = world
            .route_chat(1, "Ash", &ChatTarget::Local, "hello")
            .await;
        assert!(ok);
        assert!(ra.try_recv().is_ok(), "sender should see their own line");
        assert!(
            rb.try_recv().is_ok(),
            "someone on the same map should hear it"
        );
        assert!(rc.try_recv().is_err(), "someone on another map must not");
    }

    /// Global reaches everyone, wherever they are standing.
    #[tokio::test]
    async fn global_chat_reaches_other_maps() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (mut b, mut rb) = presence(2, "Misty", 0, 0);
        b.pose.map = MapId::new(0, 9);
        world.join(1, a).await;
        world.join(2, b).await;
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}

        let ok = world
            .route_chat(1, "Ash", &ChatTarget::Global, "hello")
            .await;
        assert!(ok);
        assert!(ra.try_recv().is_ok());
        assert!(rb.try_recv().is_ok(), "global must cross maps");
    }

    #[tokio::test]
    async fn a_private_message_to_an_offline_player_reports_failure() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        world.join(1, a).await;
        let ok = world
            .route_chat(1, "Ash", &ChatTarget::Private("Nobody".into()), "hey")
            .await;
        assert!(!ok);
    }

    /// Accepting seats both players, and a block reaches the opponent tagged with the
    /// sender's slot -- which is what the game files it under.
    #[tokio::test]
    async fn a_block_reaches_the_opponent_with_the_senders_slot() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, true).await.unwrap();
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}

        assert!(world.route_link_block(1, vec![7, 8, 9]).await);
        match rb.try_recv() {
            Ok(ServerControl::LinkBlock { from_slot, bytes }) => {
                assert_eq!(from_slot, 0, "the challenger is slot 0");
                assert_eq!(bytes, vec![7, 8, 9]);
            }
            other => panic!("expected a block, got {other:?}"),
        }

        // And the other way, from slot 1.
        assert!(world.route_link_block(2, vec![1]).await);
        match ra.try_recv() {
            Ok(ServerControl::LinkBlock { from_slot, .. }) => assert_eq!(from_slot, 1),
            other => panic!("expected a block, got {other:?}"),
        }
    }

    /// Blocks from someone who is not in a battle go nowhere. Without this a client could
    /// push traffic at anyone by inventing it.
    #[tokio::test]
    async fn a_block_from_nobodys_battle_is_dropped() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        while rb.try_recv().is_ok() {}

        assert!(!world.route_link_block(1, vec![1, 2, 3]).await);
        assert!(
            rb.try_recv().is_err(),
            "an unseated player must not reach anyone"
        );
    }

    /// Declining seats nobody, so the battle traffic has nowhere to go.
    #[tokio::test]
    async fn declining_leaves_both_players_unseated() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, false).await.unwrap();

        assert!(!world.route_link_block(1, vec![1]).await);
        assert!(!world.route_link_block(2, vec![1]).await);
    }

    /// Ending a battle unseats both sides, so blocks stop being forwarded to an opponent
    /// who has already walked away from it.
    #[tokio::test]
    async fn ending_a_battle_unseats_both() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 0, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, true).await.unwrap();
        assert!(
            world.route_link_block(1, vec![1]).await,
            "seated to begin with"
        );

        world.clear_battle(1).await;
        assert!(
            !world.route_link_block(1, vec![1]).await,
            "the ender is unseated"
        );
        assert!(
            !world.route_link_block(2, vec![1]).await,
            "and so is the opponent"
        );
    }

    /// When one player drops, the other must not be left seated against a ghost.
    #[tokio::test]
    async fn leaving_unseats_the_opponent() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 0, 0);
        let session_a = a.session;
        world.join(1, a).await;
        world.join(2, b).await;
        world.invite_to_battle(1, 2).await.unwrap();
        world.answer_battle(2, 1, true).await.unwrap();

        world.leave(1, session_a).await;
        assert!(
            !world.route_link_block(2, vec![1]).await,
            "the survivor should no longer be in a battle"
        );
    }

    #[tokio::test]
    async fn leaving_announces_departure_to_others() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 0, 0);
        let b_session = b.session;
        world.join(1, a).await;
        world.join(2, b).await;
        while ra.try_recv().is_ok() {}

        world.leave(2, b_session).await;
        match ra.try_recv() {
            Ok(ServerControl::PlayerLeft { player_id }) => assert_eq!(player_id, 2),
            other => panic!("expected PlayerLeft, got {other:?}"),
        }
        assert_eq!(world.online_count().await, 1);
    }

    #[tokio::test]
    async fn a_stale_connection_cannot_evict_the_reconnected_one() {
        // The exact failure seen live: a second sidecar connects as the same character,
        // then the first one's teardown removed the live presence and snapshots stopped.
        let world = World::new();
        let (first, _r1) = presence(1, "Ash", 0, 0);
        let stale_session = first.session;
        world.join(1, first).await;

        let (second, _r2) = presence(1, "Ash", 5, 5);
        let live_session = second.session;
        world.join(1, second).await;
        assert_ne!(stale_session, live_session);

        world.leave(1, stale_session).await;

        assert_eq!(world.online_count().await, 1, "live session must survive");
        assert!(world.session_is_current(1, live_session).await);
        assert!(!world.session_is_current(1, stale_session).await);
    }

    #[tokio::test]
    async fn a_battle_invitation_reaches_the_target_and_can_be_accepted() {
        let world = World::new();
        let (a, mut ra) = presence(1, "Ash", 0, 0);
        let (b, mut rb) = presence(2, "Misty", 1, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        while ra.try_recv().is_ok() {}
        while rb.try_recv().is_ok() {}

        world.invite_to_battle(1, 2).await.unwrap();
        match rb.try_recv() {
            Ok(ServerControl::BattleInvitation { from, from_name }) => {
                assert_eq!(from, 1);
                assert_eq!(from_name, "Ash");
            }
            other => panic!("expected BattleInvitation, got {other:?}"),
        }

        world.answer_battle(2, 1, true).await.unwrap();
        match ra.try_recv() {
            Ok(ServerControl::BattleInvitationAnswered {
                accepted,
                from_name,
                ..
            }) => {
                assert!(accepted);
                assert_eq!(from_name, "Misty");
            }
            other => panic!("expected BattleInvitationAnswered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn battle_invitations_are_refused_across_maps_and_to_yourself() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (mut b, _rb) = presence(2, "Misty", 0, 0);
        b.pose.map = MapId::new(9, 9);
        world.join(1, a).await;
        world.join(2, b).await;

        assert!(
            world.invite_to_battle(1, 1).await.is_err(),
            "self-challenge"
        );
        assert!(world.invite_to_battle(1, 2).await.is_err(), "different map");
        assert!(world.invite_to_battle(1, 99).await.is_err(), "not online");
    }

    #[tokio::test]
    async fn a_player_cannot_be_challenged_twice_at_once() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 1, 0);
        let (c, _rc) = presence(3, "Brock", 2, 0);
        world.join(1, a).await;
        world.join(2, b).await;
        world.join(3, c).await;

        world.invite_to_battle(1, 2).await.unwrap();
        assert!(
            world.invite_to_battle(3, 2).await.is_err(),
            "a second challenger must be refused while one is outstanding"
        );
    }

    #[tokio::test]
    async fn a_stale_answer_cannot_cancel_a_newer_challenge() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 1, 0);
        world.join(1, a).await;
        world.join(2, b).await;

        world.invite_to_battle(1, 2).await.unwrap();
        // Answering an invitation that was never sent must not clear the real one.
        assert!(world.answer_battle(2, 99, true).await.is_err());
        assert!(
            world.answer_battle(2, 1, true).await.is_ok(),
            "real one survives"
        );
    }

    #[tokio::test]
    async fn a_superseded_connection_cannot_move_the_character() {
        let world = World::new();
        let (first, _r1) = presence(1, "Ash", 0, 0);
        let stale_session = first.session;
        world.join(1, first).await;
        let (second, _r2) = presence(1, "Ash", 5, 5);
        world.join(1, second).await;

        world
            .update_pose(
                1,
                stale_session,
                Pose {
                    map: MapId::new(1, 4),
                    x: 99,
                    y: 99,
                    ..Default::default()
                },
            )
            .await;

        assert_eq!(world.pose_of(1).await.unwrap().x, 5, "stale report ignored");
    }
}
