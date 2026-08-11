//! QUIC listener and per-connection handling.

use crate::auth;
use crate::config::Config;
use crate::db::{self, Db};
use crate::world::{Presence, SharedWorld};
use anyhow::Context;
use pokeplanet_proto::quic::{
    self, ClientControl, ClientMovement, ServerControl, ServerSnapshot,
};
use pokeplanet_proto::{PlayerId, Pose};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// How often each client is told where everyone else is.
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);
/// How often a player's position is written back to Postgres.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(15);
const MAX_CONTROL_FRAME: usize = 64 * 1024;
/// The game's flash image, and therefore the largest save that can be genuine.
const MAX_SAVE_BYTES: usize = 128 * 1024;
/// Slice size for handing a stored save back to a client.
const SAVE_STREAM_CHUNK: usize = 1024;

/// The game's BLOCK_BUFFER_SIZE. A block larger than the buffer it is destined for cannot
/// be anything the battle engine produced.
const MAX_LINK_BLOCK: usize = 256;

pub struct Server {
    pub cfg: Arc<Config>,
    /// The gameplay rates this server publishes. See rates.rs.
    pub rates: Arc<crate::rates::Rates>,
    pub db: Db,
    pub world: SharedWorld,
    pub http: reqwest::Client,
}

pub fn endpoint(cfg: &Config) -> anyhow::Result<Endpoint> {
    let certs = load_certs(&cfg.cert_chain)?;
    let key = load_key(&cfg.private_key)?;

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS config")?;
    // Identify our application protocol so this can share a port with other QUIC services.
    tls.alpn_protocols = vec![b"pokeplanet/1".to_vec()];

    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .context("QUIC requires a TLS 1.3-capable config")?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));

    let transport = Arc::get_mut(&mut server_config.transport).expect("fresh transport config");
    // Movement rides datagrams, so they must be enabled explicitly.
    transport.datagram_receive_buffer_size(Some(64 * 1024));
    transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    transport.keep_alive_interval(Some(Duration::from_secs(10)));

    Ok(Endpoint::server(server_config, cfg.quic_addr)?)
}

fn load_certs(path: &std::path::Path) -> anyhow::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let certs: Result<Vec<_>, _> = rustls_pemfile::certs(&mut &data[..]).collect();
    let certs = certs.context("parsing certificate chain")?;
    anyhow::ensure!(!certs.is_empty(), "no certificates in {}", path.display());
    Ok(certs)
}

fn load_key(path: &std::path::Path) -> anyhow::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    rustls_pemfile::private_key(&mut &data[..])
        .context("parsing private key")?
        .ok_or_else(|| anyhow::anyhow!("no private key in {}", path.display()))
}

pub async fn run(server: Arc<Server>, endpoint: Endpoint) {
    tracing::info!(addr = %server.cfg.quic_addr, "QUIC listener ready");
    while let Some(incoming) = endpoint.accept().await {
        let server = server.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let peer = conn.remote_address();
                    if let Err(e) = handle_connection(server, conn).await {
                        tracing::debug!(%peer, error = %e, "connection ended");
                    }
                }
                Err(e) => tracing::debug!(error = %e, "handshake failed"),
            }
        });
    }
}

async fn read_frame(stream: &mut RecvStream) -> anyhow::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len).await {
        Ok(()) => {}
        Err(_) => return Ok(None), // peer closed
    }
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(
        len > 0 && len <= MAX_CONTROL_FRAME,
        "implausible control frame length {len}"
    );
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    Ok(Some(body))
}

async fn write_frame(stream: &mut SendStream, msg: &ServerControl) -> anyhow::Result<()> {
    let body = quic::encode(msg)?;
    stream.write_all(&(body.len() as u32).to_le_bytes()).await?;
    stream.write_all(&body).await?;
    Ok(())
}

async fn handle_connection(server: Arc<Server>, conn: Connection) -> anyhow::Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await.context("no control stream")?;

    // The first frame must be Hello, and it decides whether this connection gets a player.
    let first = read_frame(&mut recv)
        .await?
        .ok_or_else(|| anyhow::anyhow!("closed before Hello"))?;
    let hello: ClientControl = quic::decode(&first)?;

    let (token, protocol_version) = match hello {
        ClientControl::Hello {
            protocol_version,
            token,
            ..
        } => (token, protocol_version),
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };

    if !quic::version_is_compatible(protocol_version) {
        write_frame(
            &mut send,
            &ServerControl::Rejected {
                reason: format!(
                    "This client speaks protocol {protocol_version}; the server needs {}. Please update.",
                    pokeplanet_proto::PROTOCOL_VERSION
                ),
            },
        )
        .await?;
        return Ok(());
    }

    // Resolve the token to a character, or start the browser login flow.
    let character = match token {
        Some(t) => db::character_for_token(&server.db, &t).await?,
        None => None,
    };
    let Some(character) = character else {
        return run_login_flow(server, conn, send, recv).await;
    };

    run_session(server, conn, send, recv, character).await
}

/// Unauthenticated connection: hand out a ticket and answer polls until the browser flow
/// completes, then upgrade straight into a normal session.
async fn run_login_flow(
    server: Arc<Server>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) -> anyhow::Result<()> {
    let ticket = auth::random_token();
    db::create_ticket(&server.db, &ticket).await?;
    write_frame(
        &mut send,
        &ServerControl::AuthRequired {
            ticket: ticket.clone(),
            login_url: server.cfg.login_url(&ticket),
        },
    )
    .await?;

    while let Some(frame) = read_frame(&mut recv).await? {
        match quic::decode::<ClientControl>(&frame)? {
            ClientControl::PollLogin { ticket } => {
                match db::claim_ticket(&server.db, &ticket).await? {
                    Some(token) => {
                        let Some(character) = db::character_for_token(&server.db, &token).await?
                        else {
                            write_frame(
                                &mut send,
                                &ServerControl::Rejected {
                                    reason: "Login completed but the session was already gone."
                                        .into(),
                                },
                            )
                            .await?;
                            return Ok(());
                        };
                        return run_session(server, conn, send, recv, character).await;
                    }
                    None => write_frame(&mut send, &ServerControl::LoginPending).await?,
                }
            }
            ClientControl::BeginLogin => {
                write_frame(
                    &mut send,
                    &ServerControl::AuthRequired {
                        ticket: ticket.clone(),
                        login_url: server.cfg.login_url(&ticket),
                    },
                )
                .await?
            }
            ClientControl::Goodbye => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}

async fn run_session(
    server: Arc<Server>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    character: db::Character,
) -> anyhow::Result<()> {
    let player_id = character.id as PlayerId;
    let session = crate::world::next_session_id();
    let name = character.name.clone();
    let session_token = auth::random_token();
    db::issue_session(&server.db, character.id, &session_token).await?;

    write_frame(
        &mut send,
        &ServerControl::Welcome {
            player_id,
            profile: character.profile(),
            token: session_token.clone(),
        },
    )
    .await?;

    // Hand over the stored save immediately after Welcome, so the client is playing this
    // character as the server last saw them rather than as this machine last saw them. A
    // character who has never saved has nothing here, and the client keeps what it has.
    //
    // Off by default because it is not finished. Pushing 128 slices down the control stream
    // right after Welcome leaves the session cycling on the 30-second idle timeout and only
    // a fraction of the slices reaching the game, so enabling it would trade a working
    // connection for a save that never arrives. Uploading is unaffected and stays on: the
    // server is already collecting saves, which is what the rest of this needs.
    write_frame(
        &mut send,
        &ServerControl::Rates {
            experience: server.rates.experience,
            encounter: server.rates.encounter,
            money: server.rates.money,
            items: server.rates.items,
            catch: server.rates.catch,
            shop_price: server.rates.shop_price,
        },
    )
    .await?;

    hand_over_save(&server, &conn, character.id, player_id).await?;

    // Fan-in for anything the world wants to push at this client.
    let (control_tx, mut control_rx) = mpsc::channel::<ServerControl>(64);

    let start_pose = Pose {
        map: pokeplanet_proto::MapId::new(character.map_group, character.map_num),
        x: character.x,
        y: character.y,
        facing: character.facing,
        elevation: character.elevation,
        moving: false,
    };

    server
        .world
        .join(
            player_id,
            Presence {
                session,
                pending_invite: None,
            battle: None,
                position_unknown: true,
                character_id: character.id,
                name: name.clone(),
                graphics_id: character.graphics_id,
                pose: start_pose,
                control: control_tx,
            },
        )
        .await;
    // Resolve the count first: an .await inside the macro's argument list would hold a
    // non-Send `fmt::Arguments` across the suspension point and make this future !Send.
    let online = server.world.online_count().await;
    tracing::info!(player = player_id, %name, online, "player online");

    let writer = tokio::spawn(async move {
        while let Some(msg) = control_rx.recv().await {
            if write_frame(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    // Movement in, snapshots out.
    let movement = tokio::spawn({
        let server = server.clone();
        let conn = conn.clone();
        async move { movement_loop(server, conn, player_id, session).await }
    });

    // Control messages from the client until it hangs up.
    let result = control_loop(
        &server, &conn, &mut recv, player_id, session, &name, character.id, &session_token,
    )
    .await;

    server.world.leave(player_id, session).await;
    if let Some(pose) = server.world.pose_of(player_id).await {
        let _ = db::save_position(&server.db, character.id, &pose).await;
    }
    movement.abort();
    writer.abort();
    tracing::info!(player = player_id, %name, "player offline");
    result
}

/// Send this character's stored save, if there is one.
///
/// On a stream of its own rather than the control stream. Sending it as a hundred-odd
/// control messages starved that stream: only a fraction reached the game and the connection
/// then cycled on the idle timeout. Control carries chat, battles and presence, and a 128KB
/// burst has no business queueing in front of them. A separate stream is what QUIC offers
/// for exactly this, and it cannot interfere with the traffic that keeps the session alive.
///
/// A character who has never saved has nothing here, and the client keeps what it has.
async fn hand_over_save(
    server: &Arc<Server>,
    conn: &Connection,
    character_id: i64,
    player_id: PlayerId,
) -> anyhow::Result<()> {
    // A character with no stored save still gets an answer -- an empty one.
    //
    // Returning silently meant the client could not tell "your save is still coming" from "you
    // have no save", so it had to guess with a timer, and a slow connection was indistinguishable
    // from a new character. Guessing wrong drops the player into the world on stale local data.
    // Sending zero bytes makes the two cases distinct, and the wait can then be a wait for an
    // answer rather than a wait for a deadline.
    let image = db::load_save(&server.db, character_id).await?.unwrap_or_default();
    // Spawned so a slow client reading its save cannot hold up whatever asked for it.
    let conn = conn.clone();
    tokio::spawn(async move {
        match conn.open_uni().await {
            Ok(mut stream) => {
                let total = image.len() as u32;
                if stream.write_all(&total.to_le_bytes()).await.is_ok()
                    && stream.write_all(&image).await.is_ok()
                {
                    let _ = stream.finish();
                    tracing::info!(player = player_id, bytes = total, "sent the stored save");
                }
            }
            Err(e) => tracing::debug!(error = %e, "could not open a stream for the save"),
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn control_loop(
    server: &Arc<Server>,
    conn: &Connection,
    recv: &mut RecvStream,
    player_id: PlayerId,
    session: crate::world::SessionId,
    name: &str,
    character_id: i64,
    session_token: &str,
) -> anyhow::Result<()> {
    // Save slices are reassembled here, per connection.
    let mut save_image: Vec<u8> = Vec::new();
    // Reassembly for whole-block reports; see ClientControl::BlockChunk below.
    let mut block_buf: Vec<u8> = Vec::new();
    let mut block_id: Option<u8> = None;
    // When this character last uploaded, so the server knows how long it had to earn what it
    // now claims. Per connection: a first upload has nothing to compare against and is not
    // judged on rate at all.
    let mut last_upload: Option<std::time::Instant> = None;

    while let Some(frame) = read_frame(recv).await? {
        match quic::decode::<ClientControl>(&frame)? {
            ClientControl::Chat { target, text } => {
                let text = sanitize_chat(&text);
                if text.is_empty() {
                    continue;
                }
                server.world.route_chat(player_id, name, &target, &text).await;
                crate::irc::relay_to_irc(name, &target, &text);
            }
            ClientControl::EnterMap { map } => {
                if let Some(mut pose) = server.world.pose_of(player_id).await {
                    pose.map = map;
                    server.world.update_pose(player_id, session, pose).await;
                }
            }
            ClientControl::RequestBattle { target } => {
                if let Err(reason) = server.world.invite_to_battle(player_id, target).await {
                    server
                        .world
                        .tell(player_id, ServerControl::BattleInvitationFailed { reason })
                        .await;
                }
            }
            ClientControl::RespondToBattle { from, accepted } => {
                if let Err(reason) = server.world.answer_battle(player_id, from, accepted).await {
                    server
                        .world
                        .tell(player_id, ServerControl::BattleInvitationFailed { reason })
                        .await;
                }
            }
            ClientControl::BlockChunk { block, offset, total, bytes } => {
                // Block 2 is the tail: Hall of Fame, Trainer Hill, recorded battle. Whole
                // sectors including footers, spliced in as they arrived.
                const TAIL_BLOCK: u8 = 2;
                let tail_len =
                    crate::save_parse::TAIL_SECTORS.len() * crate::save_parse::SECTOR_SIZE;

                if block == TAIL_BLOCK {
                    if total as usize != tail_len
                        || offset as usize + bytes.len() > tail_len
                        || bytes.len() > 0x400
                    {
                        tracing::warn!(player = player_id, "refusing a malformed tail chunk");
                        block_buf.clear();
                        continue;
                    }
                    if offset == 0 || block_id != Some(block) || block_buf.len() != offset as usize
                    {
                        block_buf.clear();
                        block_id = Some(block);
                    }
                    if block_buf.len() != offset as usize {
                        continue;
                    }
                    block_buf.extend_from_slice(&bytes);
                    if block_buf.len() != tail_len {
                        continue;
                    }

                    let tail = std::mem::take(&mut block_buf);
                    block_id = None;

                    let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                        continue;
                    };
                    let Some(candidate) = crate::save_parse::with_tail(&stored, &tail) else {
                        continue;
                    };
                    // Still has to remain a save the server can read: splicing sectors it does
                    // not model must not be a way to make the rest unparseable.
                    if crate::save_parse::parse(&candidate).is_none() {
                        tracing::warn!(player = player_id, "tail report broke the save; refusing");
                        continue;
                    }
                    db::store_save(&server.db, character_id, &candidate).await?;
                    tracing::info!(player = player_id, "tail sectors set by report");
                    continue;
                }

                let Some(sectors) = crate::save_parse::reportable_block(block) else {
                    tracing::warn!(player = player_id, block, "refusing an unknown block id");
                    continue;
                };
                let want = sectors.len() * crate::save_parse::SECTOR_DATA_SIZE;

                // Bounded by which block this is, so the wire does not get to choose how much
                // is held. Not required to *equal* it: the game's structs are smaller than the
                // sectors that carry them -- SaveBlock2 is well under one -- so a client can
                // only honestly send sizeof(struct), and reading the rest of the sector out of
                // its process would be reading past the end of the object.
                if total as usize > want
                    || offset as usize + bytes.len() > total as usize
                    || bytes.len() > 0x400
                {
                    tracing::warn!(player = player_id, block, total, "refusing a malformed chunk");
                    block_buf.clear();
                    continue;
                }

                // Restarting part way through, or arriving out of order: begin again rather
                // than stitch together pieces of two different versions of the block.
                if offset == 0 || block_id != Some(block) || block_buf.len() != offset as usize {
                    block_buf.clear();
                    block_id = Some(block);
                }
                if block_buf.len() != offset as usize {
                    continue;
                }
                block_buf.extend_from_slice(&bytes);
                if block_buf.len() != total as usize {
                    continue;
                }

                let reported = std::mem::take(&mut block_buf);
                block_id = None;

                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    continue;
                };
                // Splice what was reported over the block already held, leaving the tail of
                // the last sector as it was. That padding is not the player's data and nobody
                // reports it; zeroing it would be inventing bytes the game never wrote.
                let Some(mut assembled) = crate::save_parse::read_block(&stored, sectors) else {
                    continue;
                };
                assembled[..reported.len()].copy_from_slice(&reported);

                let Some(candidate) =
                    crate::save_parse::reauthor_block(&stored, sectors, &assembled)
                else {
                    tracing::warn!(player = player_id, block, "could not rebuild the save block");
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported block");
                    continue;
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                tracing::info!(player = player_id, block, "block set by report");
            }
            ClientControl::RegionChanged { offset, bytes } => {
                // Bounded before it is used for anything: the allowlist inside with_region is
                // the real check, but a length the wire chose should never reach a copy.
                if bytes.len() > 0x400 {
                    tracing::warn!(player = player_id, len = bytes.len(), "refusing a large region");
                    continue;
                }
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    continue;
                };
                let Some(block1) =
                    crate::save_parse::with_region(&old, offset as usize, &bytes)
                else {
                    tracing::warn!(player = player_id, offset, "refusing a region not on the list");
                    continue;
                };
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported region");
                    continue;
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                let vars: Vec<u8> = new.vars.iter().flat_map(|v| v.to_le_bytes()).collect();
                if let Err(e) =
                    db::store_story_state(&server.db, character_id, &new.flags, &vars).await
                {
                    tracing::warn!(error = %e, "could not store story state");
                }
                tracing::debug!(player = player_id, offset, "region set by report");
            }
            ClientControl::PartyChanged { count, mons } => {
                // Bounded before anything else touches it: a length this does not expect is a
                // client that is broken or probing, and neither should get to choose how much
                // the server copies.
                if mons.len() != 600 || count > 6 {
                    tracing::warn!(player = player_id, len = mons.len(), "refusing a malformed party");
                    continue;
                }
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    // Was silent. Every report for a character with no save row vanished without
                    // a trace, which is the hardest possible version of this to diagnose.
                    tracing::warn!(
                        player = player_id,
                        "party reported but there is no stored save to apply it to"
                    );
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    tracing::warn!(player = player_id, "party reported against an unreadable save");
                    continue;
                };
                let Some(block1) = crate::save_parse::with_party(&old, count, &mons) else {
                    tracing::warn!(player = player_id, "could not place the reported party");
                    continue;
                };
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    tracing::warn!(player = player_id, "could not rebuild the save to set the party");
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                // The same rules an uploaded party meets: level and EV caps, experience
                // consistent with level, and no rate of gain the configured rates disallow.
                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                    .or_else(|| {
                        last_upload.and_then(|t| {
                            crate::rates::gained_too_fast(&old, &new, &server.rates, t.elapsed())
                        })
                    })
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported party");
                    continue;
                }

                last_upload = Some(std::time::Instant::now());
                db::store_save(&server.db, character_id, &candidate).await?;
                if let Err(e) = db::store_inventory_and_party(
                    &server.db, character_id, &new.bag, &new.party,
                )
                .await
                {
                    tracing::warn!(error = %e, "could not store the party");
                }
                tracing::info!(player = player_id, party = new.party.len(), "party set by report");
            }
            ClientControl::ItemChanged { pocket, item, quantity } => {
                // Same shape as money: build the save the report implies, then judge it exactly
                // as an uploaded one. impossible() already knows the per-slot ceiling, so an
                // over-full slot is refused here by the rule that refuses it there.
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    tracing::warn!(player = player_id, "item reported with no save to apply it to");
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    tracing::warn!(player = player_id, "item reported against an unreadable save");
                    continue;
                };
                let Some(block1) = crate::save_parse::with_item(&old, pocket, item, quantity)
                else {
                    tracing::warn!(
                        player = player_id, pocket, item,
                        "no room for a reported item, or no such pocket"
                    );
                    continue;
                };
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    tracing::warn!(player = player_id, "could not rebuild the save to set an item");
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported item");
                    continue;
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                if let Err(e) = db::store_inventory_and_party(
                    &server.db, character_id, &new.bag, &new.party,
                )
                .await
                {
                    tracing::warn!(error = %e, "could not store the bag");
                }
                tracing::info!(player = player_id, item, quantity, "item set by report");
            }
            ClientControl::MoneyChanged { amount } => {
                // The server writes this into its own copy of the save rather than waiting to
                // be handed a new image. That is the whole point: for money, the upload is no
                // longer what carries the value.
                //
                // Reported values are not trusted any further than uploaded ones. The candidate
                // save is built, then run through exactly the checks an upload gets -- the same
                // caps, the same no-going-backwards rule, the same rate ceiling. Writing a
                // second, laxer set of rules for the direct path would make the direct path the
                // way to cheat.
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    tracing::warn!(player = player_id, "money reported with no save to apply it to");
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    tracing::warn!(player = player_id, "money reported against an unreadable save");
                    continue;
                };

                let block1 = crate::save_parse::with_money(&old, amount);
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    // reauthor proves it can rebuild this image faithfully before writing to
                    // it, so declining here means the save was not one it could author safely.
                    tracing::warn!(player = player_id, "could not rebuild the save to set money");
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                    .or_else(|| {
                        last_upload.and_then(|t| {
                            crate::rates::gained_too_fast(&old, &new, &server.rates, t.elapsed())
                        })
                    })
                {
                    tracing::warn!(player = player_id, %reason, "refusing reported money");
                    continue;
                }

                last_upload = Some(std::time::Instant::now());
                db::store_save(&server.db, character_id, &candidate).await?;
                tracing::info!(player = player_id, money = new.money(), "money set by report");
            }
            ClientControl::SaveUpload { offset, total, bytes } => {
                // A save is never larger than the flash image the game actually has, so a
                // client claiming otherwise is either broken or probing; refuse rather than
                // let the wire decide how much memory to hold.
                if total as usize > MAX_SAVE_BYTES {
                    tracing::warn!(player = player_id, total, "refusing an oversized save");
                    break;
                }
                if offset as usize + bytes.len() > total as usize {
                    tracing::warn!(player = player_id, "refusing a malformed save chunk");
                    break;
                }
                // Out of order, or a restart part way through: begin again.
                if offset == 0 || save_image.len() != offset as usize {
                    save_image.clear();
                }
                if save_image.len() == offset as usize {
                    save_image.extend_from_slice(&bytes);
                    if save_image.len() == total as usize {
                        // Read the save before filing it, so one that could not have come
                        // from playing is never stored in the first place.
                        //
                        // Reading it here rather than asking the client for a summary is the
                        // point: a summary would be one more thing the client is trusted to
                        // be honest about, whereas this is the same bytes the game itself
                        // reads back.
                        let parsed = crate::save_parse::parse(&save_image);

                        if let Some(reason) = parsed.as_ref().and_then(|s| s.impossible()) {
                            // Safe to refuse only because these rules are caps the game
                            // itself clamps to, so no honest save can trip them. The server
                            // keeps the copy it already believed, which sets the player back
                            // rather than locking them out.
                            tracing::warn!(
                                player = player_id, %reason,
                                "refusing a save that could not have come from playing"
                            );
                            save_image.clear();
                            continue;
                        }

                        // Compare against the copy already held, which is the only way to
                        // see a change rather than a state. Loading it costs one read per
                        // save and is what makes going backwards visible at all.
                        if let (Some(new), Ok(Some(old_image))) =
                            (parsed.as_ref(), db::load_save(&server.db, character_id).await)
                        {
                            if let Some(old) = crate::save_parse::parse(&old_image) {
                                if let Some(reason) = crate::save_parse::regressed(&old, new)
                                    .or_else(|| {
                                        last_upload.and_then(|t| {
                                            crate::rates::gained_too_fast(
                                                &old, new, &server.rates, t.elapsed(),
                                            )
                                        })
                                    })
                                {
                                    tracing::warn!(
                                        player = player_id, %reason,
                                        "refusing a save that undoes progress"
                                    );
                                    save_image.clear();
                                    continue;
                                }
                            }
                        }

                        last_upload = Some(std::time::Instant::now());
                        db::store_save(&server.db, character_id, &save_image).await?;
                        tracing::info!(
                            player = player_id, bytes = save_image.len(), "save stored"
                        );

                        // A save that will not parse at all is still kept. The client can
                        // already play it -- it is the image the game wrote -- and refusing
                        // it would lose real progress over a format this may simply not
                        // understand yet.
                        match parsed {
                            Some(state) => {
                                let vars: Vec<u8> = state
                                    .vars
                                    .iter()
                                    .flat_map(|v| v.to_le_bytes())
                                    .collect();
                                if let Err(e) = db::store_story_state(
                                    &server.db, character_id, &state.flags, &vars,
                                )
                                .await
                                {
                                    tracing::warn!(error = %e, "could not store story state");
                                }

                                // Beside the story state: the same save, projected into tables
                                // the server can query instead of bytes it can only keep.
                                if let Err(e) = db::store_inventory_and_party(
                                    &server.db, character_id, &state.bag, &state.party,
                                )
                                .await
                                {
                                    tracing::warn!(error = %e, "could not store bag and party");
                                }

                                tracing::info!(
                                    player = player_id, money = state.money(),
                                    items = state.bag.len(), party = state.party.len(),
                                    "progress read from the save"
                                );
                            }
                            None => tracing::warn!(
                                player = player_id, "could not read the uploaded save"
                            ),
                        }
                        save_image.clear();
                    }
                }
            }
            ClientControl::Resync => {
                // Read the character again rather than reusing the one sign-in loaded: this
                // is asked precisely because that copy may be hours old.
                let Some(fresh) = db::character_by_id(&server.db, character_id).await? else {
                    tracing::warn!(player = player_id, "resync for a character that is gone");
                    break;
                };
                let mut profile = fresh.profile();
                // Prefer where the world has them standing. The row is only written when
                // they save or leave, so during play it lags; the world is the authority
                // movement is validated against and is what the client must agree with.
                if let Some(pose) = server.world.pose_of(player_id).await {
                    profile.map_group = pose.map.group;
                    profile.map_num = pose.map.num;
                    profile.x = pose.x;
                    profile.y = pose.y;
                }
                tracing::info!(
                    player = player_id, map_group = profile.map_group,
                    map_num = profile.map_num, x = profile.x, y = profile.y,
                    "resyncing a newly attached game"
                );
                // The rates go out again too. They are sent once after Welcome, which is
                // normally before the game has finished booting, and GameLink drops what it
                // cannot deliver -- so a game attaching later would play on the original
                // game's rates while the server ran on different ones, and nothing would look
                // wrong until somebody compared the numbers.
                server
                    .world
                    .tell(
                        player_id,
                        ServerControl::Rates {
                            experience: server.rates.experience,
                            encounter: server.rates.encounter,
                            money: server.rates.money,
                            items: server.rates.items,
                            catch: server.rates.catch,
                            shop_price: server.rates.shop_price,
                        },
                    )
                    .await;

                server
                    .world
                    .tell(
                        player_id,
                        ServerControl::Welcome {
                            player_id,
                            profile,
                            // The same token, not a new one: this is the same session, and
                            // rotating it here would invalidate the copy the sidecar holds.
                            token: session_token.to_string(),
                        },
                    )
                    .await;
                hand_over_save(server, conn, character_id, player_id).await?;
            }
            ClientControl::LinkBlock { bytes } => {
                // The game's own buffer is 256 bytes, so anything larger is a broken or
                // hostile client rather than a battle.
                if bytes.len() > MAX_LINK_BLOCK {
                    tracing::warn!(
                        player = player_id, len = bytes.len(), "refusing an oversized block"
                    );
                    break;
                }
                server.world.route_link_block(player_id, bytes).await;
            }
            ClientControl::BattleEnded => {
                server.world.clear_battle(player_id).await;
            }
            ClientControl::Goodbye => break,
            ClientControl::Hello { .. } | ClientControl::BeginLogin | ClientControl::PollLogin { .. } => {
                // Already authenticated; nothing to do.
            }
        }
    }
    Ok(())
}

/// Strip anything the game's text renderer cannot show, and bound the length.
fn sanitize_chat(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(pokeplanet_proto::ipc::TEXT_LEN - 1)
        .collect::<String>()
        .trim()
        .to_string()
}

async fn movement_loop(
    server: Arc<Server>,
    conn: Connection,
    player_id: PlayerId,
    session: crate::world::SessionId,
) {
    let mut ticker = tokio::time::interval(SNAPSHOT_INTERVAL);
    let mut save_ticker = tokio::time::interval(POSITION_SAVE_INTERVAL);
    save_ticker.tick().await; // the first tick fires immediately; skip it

    loop {
        tokio::select! {
            datagram = conn.read_datagram() => {
                let Ok(bytes) = datagram else { return };
                match quic::decode::<ClientMovement>(&bytes) {
                    Ok(m) => {
                        let accepted = server.world.update_pose(player_id, session, m.pose).await;
                        // Only speak up when the client is somewhere the server does not
                        // agree with. Saying nothing the rest of the time keeps this at
                        // zero cost for everyone playing honestly.
                        if let Some(truth) = accepted {
                            if truth != m.pose {
                                tracing::debug!(
                                    player = player_id,
                                    claimed = ?(m.pose.x, m.pose.y),
                                    actual = ?(truth.x, truth.y),
                                    "refused an impossible step"
                                );
                                server
                                    .world
                                    .tell(player_id, ServerControl::Correction { pose: truth })
                                    .await;
                            }
                        }
                    }
                    Err(e) => tracing::debug!(player = player_id, error = %e, "bad movement datagram"),
                }
            }
            _ = ticker.tick() => {
                // Stop feeding a superseded connection rather than shadowing the live one.
                if !server.world.session_is_current(player_id, session).await { return }
                let Some(pose) = server.world.pose_of(player_id).await else { return };
                let players = server.world.snapshot(player_id, pose.map, pose).await;
                let snapshot = ServerSnapshot { players };
                if let Ok(bytes) = quic::encode(&snapshot) {
                    // Dropping a snapshot is fine; another follows in 100ms.
                    let _ = conn.send_datagram(bytes.into());
                }
            }
            _ = save_ticker.tick() => {
                if let Some(pose) = server.world.pose_of(player_id).await {
                    let _ = db::save_position(&server.db, player_id as i64, &pose).await;
                }
            }
        }
    }
}
