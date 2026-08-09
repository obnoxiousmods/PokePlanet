//! Live world state: who is online, where they are standing, and chat routing.
//!
//! This is deliberately in-memory and rebuilt on restart. Durable progress lives in
//! Postgres; presence is ephemeral and cheap to reconstruct when clients reconnect.

use pokeplanet_proto::quic::{ChatTarget, RemotePlayer, ServerControl};
use pokeplanet_proto::{MapId, PlayerId, Pose, MAX_VISIBLE_PLAYERS};
use std::collections::HashMap;
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
    /// Control-stream sink for this connection.
    pub control: mpsc::Sender<ServerControl>,
    /// Who has challenged this player and is awaiting an answer.
    pub pending_invite: Option<PlayerId>,
}

#[derive(Default)]
pub struct World {
    players: RwLock<HashMap<PlayerId, Presence>>,
}

pub type SharedWorld = Arc<World>;

impl World {
    pub fn new() -> SharedWorld {
        Arc::new(World::default())
    }

    pub async fn join(&self, id: PlayerId, presence: Presence) {
        let announcement = ServerControl::PlayerJoined {
            player_id: id,
            name: presence.name.clone(),
            graphics_id: presence.graphics_id,
        };
        {
            let mut players = self.players.write().await;
            players.insert(id, presence);
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
                Some(p) if p.session == session => players.remove(&id).is_some(),
                _ => false,
            }
        };
        if removed {
            self.broadcast_except(id, ServerControl::PlayerLeft { player_id: id })
                .await;
        }
    }

    /// Update a pose, ignoring reports from a connection that has been superseded.
    pub async fn update_pose(&self, id: PlayerId, session: SessionId, pose: Pose) {
        if let Some(p) = self.players.write().await.get_mut(&id) {
            if p.session == session {
                p.pose = pose;
            }
        }
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
        let players = self.players.read().await;
        let mut visible: Vec<&Presence> = players
            .iter()
            .filter(|(id, p)| **id != viewer && p.pose.map == map)
            .map(|(_, p)| p)
            .collect();

        if visible.len() > MAX_VISIBLE_PLAYERS {
            visible.sort_by_key(|p| {
                let dx = (p.pose.x - from.x) as i32;
                let dy = (p.pose.y - from.y) as i32;
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
            })
            .collect()
    }

    /// Deliver a chat message according to its target. Returns false if a private
    /// message had no recipient online.
    pub async fn route_chat(&self, from: &str, target: &ChatTarget, text: &str) -> bool {
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
                // Scope to the sender's own map.
                let Some(origin) = players.values().find(|p| p.name == from).map(|p| p.pose.map)
                else {
                    return false;
                };
                for p in players.values().filter(|p| p.pose.map == origin) {
                    let _ = p.control.try_send(msg.clone());
                }
                true
            }
            ChatTarget::Private(to) => {
                let mut delivered = false;
                for p in players.values().filter(|p| &p.name == to || p.name == from) {
                    let _ = p.control.try_send(msg.clone());
                    if &p.name == to {
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
    pub async fn invite_to_battle(
        &self,
        from: PlayerId,
        target: PlayerId,
    ) -> Result<(), String> {
        if from == target {
            return Err("You can't battle yourself.".into());
        }
        let players = self.players.read().await;
        let inviter = players.get(&from).ok_or("You are not online.")?;
        let invitee = players.get(&target).ok_or("They are no longer online.")?;

        if inviter.pose.map != invitee.pose.map {
            return Err("They have left this area.".into());
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
        let _ = inviter.control.try_send(ServerControl::BattleInvitationAnswered {
            from: responder,
            from_name: responder_name,
            accepted,
        });
        Ok(())
    }

    /// Send one control message to a single player.
    pub async fn tell(&self, id: PlayerId, msg: ServerControl) {
        if let Some(p) = self.players.read().await.get(&id) {
            let _ = p.control.try_send(msg);
        }
    }

    /// Deliver a message that originated outside the game, e.g. from IRC.
    pub async fn inject_chat(&self, from: &str, text: &str) {
        let msg = ServerControl::Chat {
            from: from.to_string(),
            target: ChatTarget::Global,
            text: text.to_string(),
        };
        for p in self.players.read().await.values() {
            let _ = p.control.try_send(msg.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(character_id: i64, name: &str, x: i16, y: i16) -> (Presence, mpsc::Receiver<ServerControl>) {
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
                control: tx,
                pending_invite: None,
            },
            rx,
        )
    }

    #[tokio::test]
    async fn a_snapshot_excludes_the_viewer() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        let (b, _rb) = presence(2, "Misty", 3, 3);
        world.join(1, a).await;
        world.join(2, b).await;

        let seen = world
            .snapshot(1, MapId::new(1, 4), Pose { map: MapId::new(1, 4), ..Default::default() })
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
            .route_chat("Ash", &ChatTarget::Private("Misty".into()), "hey")
            .await;
        assert!(ok);
        assert!(ra.try_recv().is_ok(), "sender should see their own PM");
        assert!(rb.try_recv().is_ok(), "recipient should receive the PM");
        assert!(rc.try_recv().is_err(), "third party must not receive the PM");
    }

    #[tokio::test]
    async fn a_private_message_to_an_offline_player_reports_failure() {
        let world = World::new();
        let (a, _ra) = presence(1, "Ash", 0, 0);
        world.join(1, a).await;
        let ok = world
            .route_chat("Ash", &ChatTarget::Private("Nobody".into()), "hey")
            .await;
        assert!(!ok);
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
            Ok(ServerControl::BattleInvitationAnswered { accepted, from_name, .. }) => {
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

        assert!(world.invite_to_battle(1, 1).await.is_err(), "self-challenge");
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
        assert!(world.answer_battle(2, 1, true).await.is_ok(), "real one survives");
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
            .update_pose(1, stale_session, Pose { map: MapId::new(1, 4), x: 99, y: 99, ..Default::default() })
            .await;

        assert_eq!(world.pose_of(1).await.unwrap().x, 5, "stale report ignored");
    }
}
