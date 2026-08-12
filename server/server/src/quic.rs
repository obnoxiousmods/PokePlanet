//! QUIC listener and per-connection handling.

use crate::auth;
use crate::config::Config;
use crate::db::{self, Db};
use crate::world::{Presence, SharedWorld};
use anyhow::Context;
use pokeplanet_proto::quic::{self, ClientControl, ClientMovement, ServerControl, ServerSnapshot};
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
#[allow(dead_code)]
const SAVE_STREAM_CHUNK: usize = 1024;

/// The game's BLOCK_BUFFER_SIZE. A block larger than the buffer it is destined for cannot
/// be anything the battle engine produced.
const MAX_LINK_BLOCK: usize = 256;

pub struct Server {
    pub cfg: Arc<Config>,
    /// The gameplay rates this server publishes. See rates.rs.
    pub rates: Arc<crate::rates::ModeRates>,
    pub db: Db,
    pub world: SharedWorld,
    pub http: reqwest::Client,
    /// Headless instances of the game, one per signed-in character, used to check the client's
    /// account of what happened against the server's own. See instances.rs.
    ///
    /// Optional because it needs the game binary, which a server does not necessarily have --
    /// and a missing binary must degrade to the rules that already exist, not stop anyone
    /// playing. A Mutex rather than per-connection state because the cap is global: the whole
    /// point of a bound is that it is shared.
    pub instances: Option<Arc<tokio::sync::Mutex<crate::instances::Instances>>>,
    /// One lock per character, guarding the load->modify->store of that character's save.
    ///
    /// A typed report reads the 128KB save, rewrites the changed part, and stores it back, across
    /// separate pooled connections. Two overlapping sessions for one character (a fast reconnect,
    /// or a second client -- several session tokens are valid at once) could otherwise interleave:
    /// both read the same image, and the second store clobbers the first's delta, silently losing a
    /// just-gained item or Pokemon. Serialising the read-modify-write per character removes that
    /// window. Weak so the map does not grow without bound: an entry is reused while any session
    /// holds the lock and re-created afterward. Only heavy (save-touching) messages take it.
    pub save_locks: SaveLocks,
}

/// Per-character save locks, keyed by character id. Weak so a character seen once does not pin a
/// lock forever: the entry is live only while a session holds it.
pub type SaveLocks =
    std::sync::Mutex<std::collections::HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>;

/// The save lock for a character, creating it if this is the first live reference. Two callers for
/// the same character get the same lock; different characters get independent locks.
fn save_lock_in(locks: &SaveLocks, character_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = locks.lock().expect("save-lock map is never poisoned");
    if let Some(lock) = map.get(&character_id).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    map.insert(character_id, Arc::downgrade(&lock));
    lock
}

impl Server {
    /// The save lock for a character. Held across a heavy handler's load->modify->store so
    /// overlapping sessions for one character cannot interleave and lose a delta.
    pub fn save_lock(&self, character_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        save_lock_in(&self.save_locks, character_id)
    }
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

fn load_certs(
    path: &std::path::Path,
) -> anyhow::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
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

    let (token, protocol_version, mode) = match hello {
        ClientControl::Hello {
            protocol_version,
            token,
            mode,
            ..
        } => (token, protocol_version, mode),
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };
    // Only two worlds exist; anything else is treated as normal so a bad value cannot conjure a
    // third ruleset.
    let mode = if mode == "deadman" {
        "deadman"
    } else {
        "normal"
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

    // The session token anchors to the account's 'normal' character (that is what login created).
    // Resolve to the character for the selected world; a deadman character is created here the first
    // time an account enters that world, with the same name and look as the anchor.
    let character = if character.mode == mode {
        character
    } else {
        db::ensure_character(
            &server.db,
            character.account_id,
            mode,
            &character.name,
            character.graphics_id,
        )
        .await?
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

    // Two bounds on an unauthenticated connection, so it cannot be used to hammer the database
    // or to sit open forever. Neither touches an honest login: a real client polls every couple
    // of seconds while the user finishes the browser step, which can legitimately take minutes.
    //
    //   - PollLogin hits the database (a claim_ticket UPDATE) only once a second at most; a poll
    //     arriving sooner is answered "pending" without a query. Honest polling is slower than
    //     this; wire-speed spam is absorbed for free.
    //   - The whole pre-auth phase is given a generous deadline. A connection that has neither
    //     authenticated nor said goodbye by then is dropped, so an abandoned or malicious one
    //     does not linger.
    let pre_auth_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    let mut last_poll: Option<tokio::time::Instant> = None;

    loop {
        let frame = match tokio::time::timeout_at(pre_auth_deadline, read_frame(&mut recv)).await {
            Ok(f) => f?,
            Err(_) => {
                tracing::info!("dropping a connection that never signed in");
                return Ok(());
            }
        };
        let Some(frame) = frame else { break };

        match quic::decode::<ClientControl>(&frame)? {
            ClientControl::PollLogin { ticket } => {
                let now = tokio::time::Instant::now();
                if last_poll
                    .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1))
                {
                    // Too soon: answer without touching the database.
                    write_frame(&mut send, &ServerControl::LoginPending).await?;
                    continue;
                }
                last_poll = Some(now);
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
    // Character ids are BIGSERIAL (i64); PlayerId on the wire is u32. At any real player count
    // the id fits, but rather than let a future id past 4 billion silently truncate -- two
    // characters colliding on one wire id, which routes one player's traffic to another -- refuse
    // the connection here. It cannot happen yet; this is so it fails loudly if it ever could.
    if character.id < 0 || character.id > PlayerId::MAX as i64 {
        anyhow::bail!(
            "character id {} does not fit the wire's player id; refusing",
            character.id
        );
    }
    let player_id = character.id as PlayerId;
    let session = crate::world::next_session_id();
    let name = character.name.clone();
    // The handle this player's chat carries into the IRC channel. The bridge posts under one bot
    // nick, so tagging each line with the player's Discord name is what lets people in the channel
    // tell who is speaking. In-game chat still shows the character name -- that is the identity
    // other players see in the overworld -- so this is fetched separately and used only for IRC.
    // Falls back to the character name if the account has no name on record.
    let irc_handle = db::discord_username_for_account(&server.db, character.account_id)
        .await
        .unwrap_or_else(|| name.clone());
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
    // The rates for this character's world (Deadman runs a harsher set than Normal).
    let rates = server.rates.for_mode(&character.mode);
    write_frame(
        &mut send,
        &ServerControl::Rates {
            experience: rates.experience,
            encounter: rates.encounter,
            money: rates.money,
            items: rates.items,
            catch: rates.catch,
            shop_price: rates.shop_price,
            species_encounter: rates.species_encounter.iter().map(|(&s, &m)| (s, m)).collect(),
        },
    )
    .await?;

    hand_over_save(&server, &conn, character.id, player_id).await?;

    // Deadman characters carry a PC bank; tell the client its balance up front so the bank menu can
    // show it. `carried` is read from the same save just handed over, so adopting it is a no-op.
    if character.mode == "deadman" {
        let bank = db::bank_balance(&server.db, character.id).await.unwrap_or(0);
        let carried = db::load_save(&server.db, character.id)
            .await
            .ok()
            .flatten()
            .and_then(|s| crate::save_parse::parse(&s))
            .map(|s| s.money())
            .unwrap_or(character.money as u32);
        write_frame(&mut send, &ServerControl::BankState { bank, carried }).await?;
    }

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

    // Seed the name-tag party count from the stored save. One read at sign-in; party reports keep
    // it current after that. A save that will not load leaves it at zero rather than failing join.
    let party_count = db::load_save(&server.db, character.id)
        .await
        .ok()
        .flatten()
        .and_then(|image| crate::save_parse::parse(&image))
        .map(|state| state.party.len() as u8)
        .unwrap_or(0);

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
                party_count,
                badges: character.badges,
                mode: character.mode.clone(),
                control: control_tx,
            },
        )
        .await;
    // Resolve the count first: an .await inside the macro's argument list would hold a
    // non-Send `fmt::Arguments` across the suspension point and make this future !Send.
    let online = server.world.online_count().await;
    tracing::info!(player = player_id, %name, online, "player online");

    // Bring up this character's validation instance. Best effort by design: if it will not
    // start, the server falls back to the rules it already enforces rather than refusing the
    // player. An extra check that can deny access is worse than no extra check.
    if let Some(instances) = &server.instances {
        instances.lock().await.start(character.id);
    }

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
        &server,
        &conn,
        &mut recv,
        player_id,
        session,
        &name,
        &irc_handle,
        character.id,
        &character.mode,
        &session_token,
    )
    .await;

    // The instance goes when the player does -- but only if this session still owns it. On a
    // reconnect the new session's `start` finds the instance already running and reuses it; if the
    // superseded session then stopped it here, keyed only by character, it would tear the instance
    // out from under the live session. Guarding on the current session keeps each teardown to the
    // instance it actually brought up. Take under the lock, reap after releasing it, so one slow
    // stop does not stall every other connection on the shared supervisor lock.
    if let Some(instances) = &server.instances {
        if server.world.session_is_current(player_id, session).await {
            let stopping = instances.lock().await.take(character.id);
            if let Some(stopping) = stopping {
                stopping.finish().await;
            }
        }
    }

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
    let image = db::load_save(&server.db, character_id)
        .await?
        .unwrap_or_default();
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

/// Whether a control message makes the server do heavy save work -- a full 128KB load, parse,
/// rebuild and store, or a fresh copy handed back. These are the operations RequestBudget paces; a
/// client's cheap traffic (keys, chat, battle relay, map/battle requests) is never throttled.
fn is_heavy_control(control: &ClientControl) -> bool {
    matches!(
        control,
        ClientControl::SaveUpload { .. }
            | ClientControl::Resync
            | ClientControl::MoneyChanged { .. }
            | ClientControl::ItemChanged { .. }
            | ClientControl::PartyChanged { .. }
            | ClientControl::RegionChanged { .. }
            | ClientControl::BlockChunk { .. }
            | ClientControl::HardReset
            | ClientControl::BankDeposit
            | ClientControl::BankWithdraw
    )
}

/// Persist the badge count a save implies into the `characters.badges` column and refresh the
/// player's live presence. The count is derived from the save's own flags (server-side, never the
/// client's word), so it drives the combat level, the ladder sort, and the Deadman PvP badge-range
/// gate from the authoritative source. Called after any accepted report that could carry a gym win.
async fn project_badges(
    server: &Arc<Server>,
    character_id: i64,
    player_id: PlayerId,
    state: &crate::save_parse::SaveState,
) {
    let badges = crate::quest_flags::badge_count(state);
    if let Err(e) = db::update_badges(&server.db, character_id, badges).await {
        tracing::warn!(error = %e, "could not project the badge count");
    }
    server.world.set_badges(player_id, badges).await;
}

#[allow(clippy::too_many_arguments)]
async fn control_loop(
    server: &Arc<Server>,
    conn: &Connection,
    recv: &mut RecvStream,
    player_id: PlayerId,
    session: crate::world::SessionId,
    name: &str,
    irc_handle: &str,
    character_id: i64,
    mode: &str,
    session_token: &str,
) -> anyhow::Result<()> {
    // Save slices are reassembled here, per connection.
    // One running allowance for this connection, refilled by time and spent by gains.
    //
    // Replaces a bare "time since the last report" timestamp. That granted a fresh minimum
    // allowance per report, so a client that reported ten times a second collected ten times the
    // headroom -- the ceiling measured how often the client spoke rather than how fast it gained.
    let mut allowance = crate::rates::Allowance::new(server.rates.for_mode(mode));
    // Paces the operations that each cost a full 128KB save load/rebuild/store or a fresh copy
    // handed back, so a client cannot amplify a tiny frame into thousands of save round-trips a
    // second. Cheap messages (keys, chat, battle) are not gated by it. See RequestBudget.
    let mut heavy_budget = crate::rates::RequestBudget::new();
    let mut save_image: Vec<u8> = Vec::new();
    // Reassembly for whole-block reports; see ClientControl::BlockChunk below.
    let mut block_buf: Vec<u8> = Vec::new();
    let mut block_id: Option<u8> = None;
    // When this character last uploaded, so the server knows how long it had to earn what it
    // now claims. Per connection: a first upload has nothing to compare against and is not
    // judged on rate at all.

    while let Some(frame) = read_frame(recv).await? {
        // Stop the moment this connection has been superseded by a newer sign-in.
        //
        // Superseded was only ever a *message* the client could ignore. A hostile client that
        // ignored it kept this loop running, and each live connection carries its own rate
        // allowance -- so N connections on one token meant N independent ceilings, and
        // reconnect-spam multiplied the rate of gain without bound. Checking session currency
        // here means a displaced connection processes no further reports: only the current
        // session spends, and the ceiling is per character again rather than per connection.
        if !server.world.session_is_current(player_id, session).await {
            tracing::info!(
                player = player_id,
                "connection superseded; stopping its control loop"
            );
            return Ok(());
        }
        let control = quic::decode::<ClientControl>(&frame)?;
        let heavy = is_heavy_control(&control);
        // Throttle only the operations that make the server do heavy save work. Over budget, the
        // message is dropped rather than the connection cut: an honest client never spends its way
        // empty (see RequestBudget), and a dropped report is reconciled by the next one or a resync,
        // so this cannot corrupt a save or disconnect a laggy but honest player.
        if heavy && !heavy_budget.take() {
            tracing::warn!(
                player = player_id,
                "throttling a burst of heavy save operations from one connection"
            );
            continue;
        }
        // Serialise this character's save read-modify-write. Held across the whole heavy handler
        // (load -> author -> store), so two overlapping sessions for one character cannot interleave
        // and clobber each other's delta. Only heavy messages take it; cheap ones (keys, chat) run
        // unserialised. A single honest session never contends -- it holds and releases in order.
        let _save_guard = if heavy {
            // lock_owned, not lock: the guard has to outlive the Arc returned by save_lock (it is
            // held across the whole match arm below), so it must own that Arc rather than borrow it.
            Some(server.save_lock(character_id).lock_owned().await)
        } else {
            None
        };
        match control {
            ClientControl::Chat { target, text } => {
                let text = sanitize_chat(&text);
                if text.is_empty() {
                    continue;
                }
                server
                    .world
                    .route_chat(player_id, name, &target, &text)
                    .await;
                crate::irc::relay_to_irc(irc_handle, &target, &text);
            }
            ClientControl::EnterMap { map } => {
                if let Some(mut pose) = server.world.pose_of(player_id).await {
                    pose.map = map;
                    server.world.update_pose(player_id, session, pose).await;
                }
                // Hand the newcomer the items already lying on this map, so drops made before they
                // arrived are visible to them too.
                let drops: Vec<(u64, u16, u16, i16, i16)> = server
                    .world
                    .drops_on_map(map)
                    .await
                    .into_iter()
                    .map(|d| (d.id, d.item, d.quantity, d.x, d.y))
                    .collect();
                server
                    .world
                    .tell(player_id, ServerControl::MapDrops { drops })
                    .await;
            }
            ClientControl::RequestBattle { target } => {
                if let Err(reason) = server.world.invite_to_battle(player_id, target).await {
                    server
                        .world
                        .tell(player_id, ServerControl::BattleInvitationFailed { reason })
                        .await;
                }
            }
            ClientControl::ForceBattle { target } => {
                // A Deadman line-of-sight lock. The server validates the rules (deadman, badge
                // range, same map, neither fighting) and forces both in; the client cannot force a
                // battle the rules forbid. A rejection is silent -- the client just keeps walking.
                if let Err(reason) = server.world.force_battle(player_id, target).await {
                    tracing::debug!(player = player_id, target, %reason, "forced battle refused");
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
            ClientControl::Keys { frames } => {
                // Replay: hand the player's own inputs to the instance computing what should
                // have happened. Bounded first -- a client choosing how much the server buffers
                // is a client choosing how much memory to spend.
                if frames.len() > 600 {
                    tracing::warn!(
                        player = player_id,
                        n = frames.len(),
                        "refusing a long key run"
                    );
                    continue;
                }
                if let Some(instances) = &server.instances {
                    let mut instances = instances.lock().await;
                    instances.connect_ready().await;
                    for keys in frames {
                        // Dropped silently when no instance is running: validation is an extra
                        // check, and its absence must never affect the player's own session.
                        instances.send_input(character_id, keys).await;
                    }
                }
            }
            ClientControl::BlockChunk {
                block,
                offset,
                total,
                bytes,
            } => {
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
                    tracing::warn!(
                        player = player_id,
                        block,
                        total,
                        "refusing a malformed chunk"
                    );
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

                // Block 0 is SaveBlock2, which holds the encryption key the game re-rolls on
                // every save. Keep the server's own key: money(), coins() and item quantities
                // are all decoded against it, so pinning it here lets the rest of SaveBlock2 --
                // options, play time, the Pokedex -- update while none of the money-bearing
                // values move. This is what an earlier blanket "refuse if the key changed" guard
                // got wrong: a fresh key is normal, and refusing it dropped every SaveBlock2
                // report a client sent, so options never persisted at all.
                const SAVEBLOCK2_BLOCK: u8 = 0;
                if block == SAVEBLOCK2_BLOCK {
                    crate::save_parse::pin_encryption_key(&mut assembled, old.encryption_key);
                }

                let Some(candidate) =
                    crate::save_parse::reauthor_block(&stored, sectors, &assembled)
                else {
                    tracing::warn!(
                        player = player_id,
                        block,
                        "could not rebuild the save block"
                    );
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                // With the key pinned above, a SaveBlock2 report cannot move money or coins:
                // both decode against the key, which did not change, and money_raw/coins_raw
                // live in SaveBlock1, which this path never writes. This check is a belt-and-
                // braces assertion of that -- if it ever fires, the pin failed and the report
                // must be refused rather than allowed to persist a money change.
                if block == SAVEBLOCK2_BLOCK
                    && (new.encryption_key != old.encryption_key || new.money() != old.money())
                {
                    tracing::error!(
                        player = player_id,
                        "SaveBlock2 report still moved the key or money after pinning; refusing"
                    );
                    continue;
                }

                // Block 1 is the PC boxes -- 35KB the server does not otherwise decode. Judge
                // each boxed Pokemon the way the party is judged, so a box cannot smuggle in a
                // mon that could not have come from playing.
                const STORAGE_BLOCK: u8 = 1;
                if block == STORAGE_BLOCK {
                    if let Some(reason) = crate::save_parse::boxes_impossible(&assembled) {
                        tracing::warn!(player = player_id, %reason, "refusing a reported box");
                        continue;
                    }
                    // Deadman Mode: the graveyard box is read-only. Refuse a storage report that has
                    // removed any corpse -- a dead Pokemon cannot be brought back into play. Compared
                    // against the block as it was stored (before this report's splice).
                    if mode == "deadman" {
                        if let Some(old_block) = crate::save_parse::read_block(&stored, sectors) {
                            if let Some(reason) =
                                crate::save_parse::graveyard_regressed(&old_block, &assembled)
                            {
                                tracing::warn!(player = player_id, %reason, "refusing a deadman box");
                                continue;
                            }
                        }
                    }
                }

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                    .or_else(|| crate::quest_flags::badge_regressed(&old, &new))
                    .or_else(|| allowance.check(&old, &new, server.rates.for_mode(mode)))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported block");
                    continue;
                }
                if let Some(id) = crate::quest_flags::monotonic_cleared(&old, &new) {
                    tracing::warn!(
                        player = player_id,
                        flag = id,
                        "a monotonic story flag was cleared (advisory, not yet enforced)"
                    );
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                tracing::info!(player = player_id, block, "block set by report");
                // A gym win is a flag in SaveBlock1, so a block report is where badges change.
                project_badges(server, character_id, player_id, &new).await;

                // Deadman Mode: after the storage block is accepted, record the graveyard so the
                // website can show what this character has lost. The read-only check above
                // guarantees corpses only ever accrue, so this count never falls except on a
                // hard reset (which clears it).
                if mode == "deadman" && block == STORAGE_BLOCK {
                    let corpses = crate::save_parse::graveyard_corpses(&assembled);
                    if let Err(e) = db::record_deaths(&server.db, character_id, &corpses).await {
                        tracing::warn!(player = player_id, error = %e, "could not record deadman deaths");
                    }
                    // One-living-per-species cross-check. Advisory until eggs are told apart from
                    // live mons (a live mon plus its egg is a legitimate duplicate here); logged so
                    // a client farming duplicates is visible, and ready to flip to enforcing.
                    if let Some(species) =
                        crate::save_parse::living_species_duplicated(&new.party, &assembled)
                    {
                        tracing::warn!(
                            player = player_id,
                            species,
                            "deadman holds two living of one species (advisory, not enforced)"
                        );
                    }
                }
            }
            ClientControl::RegionChanged { offset, bytes } => {
                // Bounded before it is used for anything: the allowlist inside with_region is
                // the real check, but a length the wire chose should never reach a copy.
                if bytes.len() > 0x400 {
                    tracing::warn!(
                        player = player_id,
                        len = bytes.len(),
                        "refusing a large region"
                    );
                    continue;
                }
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    continue;
                };
                let Some(block1) = crate::save_parse::with_region(&old, offset as usize, &bytes)
                else {
                    tracing::warn!(
                        player = player_id,
                        offset,
                        "refusing a region not on the list"
                    );
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
                    .or_else(|| crate::quest_flags::badge_regressed(&old, &new))
                    .or_else(|| allowance.check(&old, &new, server.rates.for_mode(mode)))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported region");
                    continue;
                }
                // Advisory for now: a monotonic story flag going backwards is logged, not
                // refused, until the derived set is proven quiet against real play.
                if let Some(id) = crate::quest_flags::monotonic_cleared(&old, &new) {
                    tracing::warn!(
                        player = player_id,
                        flag = id,
                        "a monotonic story flag was cleared (advisory, not yet enforced)"
                    );
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                let vars: Vec<u8> = new.vars.iter().flat_map(|v| v.to_le_bytes()).collect();
                if let Err(e) =
                    db::store_story_state(&server.db, character_id, &new.flags, &vars).await
                {
                    tracing::warn!(error = %e, "could not store story state");
                }
                project_badges(server, character_id, player_id, &new).await;
                tracing::debug!(player = player_id, offset, "region set by report");
            }
            ClientControl::PartyChanged { count, mons } => {
                // Bounded before anything else touches it: a length this does not expect is a
                // client that is broken or probing, and neither should get to choose how much
                // the server copies.
                if mons.len() != 600 || count > 6 {
                    tracing::warn!(
                        player = player_id,
                        len = mons.len(),
                        "refusing a malformed party"
                    );
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
                    tracing::warn!(
                        player = player_id,
                        "party reported against an unreadable save"
                    );
                    continue;
                };
                let Some(block1) = crate::save_parse::with_party(&old, count, &mons) else {
                    tracing::warn!(player = player_id, "could not place the reported party");
                    continue;
                };
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    tracing::warn!(
                        player = player_id,
                        "could not rebuild the save to set the party"
                    );
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
                    .or_else(|| allowance.check(&old, &new, server.rates.for_mode(mode)))
                {
                    tracing::warn!(player = player_id, %reason, "refusing a reported party");
                    continue;
                }

                // Deadman Mode caps progression to the next gym leader: no party mon may exceed the
                // badge level cap, and the party cannot be larger than the badge party-size cap.
                // Enforced server-side so a patched client cannot out-level or out-number the world.
                if mode == "deadman" {
                    let badges = crate::quest_flags::badge_count(&new);
                    let level_cap = crate::deadman::level_cap(badges);
                    if let Some(m) = new
                        .party
                        .iter()
                        .find(|m| m.species != 0 && m.level > level_cap)
                    {
                        tracing::warn!(
                            player = player_id,
                            level = m.level,
                            level_cap,
                            badges,
                            "refusing a deadman party above the level cap"
                        );
                        continue;
                    }
                    let living = new.party.iter().filter(|m| m.species != 0).count() as u8;
                    let party_cap = crate::deadman::party_cap(badges);
                    if living > party_cap {
                        tracing::warn!(
                            player = player_id,
                            size = living,
                            party_cap,
                            badges,
                            "refusing a deadman party above the size cap"
                        );
                        continue;
                    }
                }

                db::store_save(&server.db, character_id, &candidate).await?;
                if let Err(e) =
                    db::store_inventory_and_party(&server.db, character_id, &new.bag, &new.party)
                        .await
                {
                    tracing::warn!(error = %e, "could not store the party");
                }
                server
                    .world
                    .set_party_count(player_id, new.party.len() as u8)
                    .await;
                project_badges(server, character_id, player_id, &new).await;
                tracing::info!(
                    player = player_id,
                    party = new.party.len(),
                    "party set by report"
                );

                // If a validation instance is running for this character, compare what the client
                // just reported against what the instance's own run produced. Advisory only: a
                // false accusation is worse than a missed cheat, so a divergence is logged, never
                // enforced, until real play proves the run and the client agree. Dormant until
                // instances are enabled at all (POKEPLANET_GAME_BINARY unset).
                if let Some(instances) = &server.instances {
                    if let Some(computed) = instances.lock().await.latest_state(character_id) {
                        if let Some(reason) = crate::save_parse::diverged(&new, &computed) {
                            tracing::warn!(
                                player = player_id,
                                %reason,
                                "replay divergence (advisory, not enforced)"
                            );
                        }
                    }
                }
            }
            ClientControl::ItemChanged {
                pocket,
                item,
                quantity,
            } => {
                // Same shape as money: build the save the report implies, then judge it exactly
                // as an uploaded one. impossible() already knows the per-slot ceiling, so an
                // over-full slot is refused here by the rule that refuses it there.
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    tracing::warn!(
                        player = player_id,
                        "item reported with no save to apply it to"
                    );
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    tracing::warn!(
                        player = player_id,
                        "item reported against an unreadable save"
                    );
                    continue;
                };
                let Some(block1) = crate::save_parse::with_item(&old, pocket, item, quantity)
                else {
                    tracing::warn!(
                        player = player_id,
                        pocket,
                        item,
                        "no room for a reported item, or no such pocket"
                    );
                    continue;
                };
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    tracing::warn!(
                        player = player_id,
                        "could not rebuild the save to set an item"
                    );
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
                if let Err(e) =
                    db::store_inventory_and_party(&server.db, character_id, &new.bag, &new.party)
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
                    tracing::warn!(
                        player = player_id,
                        "money reported with no save to apply it to"
                    );
                    continue;
                };
                let Some(old) = crate::save_parse::parse(&stored) else {
                    tracing::warn!(
                        player = player_id,
                        "money reported against an unreadable save"
                    );
                    continue;
                };

                let block1 = crate::save_parse::with_money(&old, amount);
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    // reauthor proves it can rebuild this image faithfully before writing to
                    // it, so declining here means the save was not one it could author safely.
                    tracing::warn!(
                        player = player_id,
                        "could not rebuild the save to set money"
                    );
                    continue;
                };
                let Some(new) = crate::save_parse::parse(&candidate) else {
                    tracing::error!(player = player_id, "rebuilt a save that will not parse");
                    continue;
                };

                if let Some(reason) = new
                    .impossible()
                    .or_else(|| crate::save_parse::regressed(&old, &new))
                    .or_else(|| allowance.check(&old, &new, server.rates.for_mode(mode)))
                {
                    tracing::warn!(player = player_id, %reason, "refusing reported money");
                    continue;
                }
                db::store_save(&server.db, character_id, &candidate).await?;
                tracing::info!(
                    player = player_id,
                    money = new.money(),
                    "money set by report"
                );
            }
            ClientControl::SaveUpload {
                offset,
                total,
                bytes,
            } => {
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
                        // The full-image upload exists only to seed a character's *first* save.
                        //
                        // Once the server holds a save, every change reaches it through a typed
                        // report that is validated field by field -- money against the rate
                        // ceiling, items against their caps, flags against the story rules,
                        // and the SaveBlock2 key pinned so it cannot be used to mint money. A
                        // full-image overwrite is a strict superset of all of those: it rewrites
                        // all 32 sectors at once, bypassing reauthor's faithfulness proof and the
                        // protected span, and would hand a cheater back everything the typed
                        // paths were built to prevent. So an upload is accepted only when there
                        // is nothing stored yet; after that it is refused, and the honest client
                        // never sends one (it is gated on the server not owning a save).
                        match db::load_save(&server.db, character_id).await {
                            Ok(Some(_)) => {
                                tracing::warn!(
                                    player = player_id,
                                    "refusing a full-save upload: the server already holds this                                      character's save; changes must come through typed reports"
                                );
                                save_image.clear();
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "could not check for a stored save");
                                save_image.clear();
                                continue;
                            }
                            Ok(None) => {}
                        }

                        // The first save must parse. Storing an unparseable blob used to be
                        // allowed on the theory the client could still play it -- but with the
                        // server authoritative that blob becomes the record, bricks every future
                        // typed report (they all read the stored save first), and is handed back
                        // verbatim at sign-in. An image the server cannot read is not a save it
                        // can own, so refuse it.
                        let Some(state) = crate::save_parse::parse(&save_image) else {
                            tracing::warn!(
                                player = player_id,
                                "refusing a first save the server cannot read"
                            );
                            save_image.clear();
                            continue;
                        };

                        if let Some(reason) = state.impossible() {
                            tracing::warn!(
                                player = player_id, %reason,
                                "refusing a first save that could not have come from playing"
                            );
                            save_image.clear();
                            continue;
                        }

                        db::store_save(&server.db, character_id, &save_image).await?;
                        tracing::info!(
                            player = player_id,
                            bytes = save_image.len(),
                            "first save stored"
                        );

                        let vars: Vec<u8> =
                            state.vars.iter().flat_map(|v| v.to_le_bytes()).collect();
                        if let Err(e) =
                            db::store_story_state(&server.db, character_id, &state.flags, &vars)
                                .await
                        {
                            tracing::warn!(error = %e, "could not store story state");
                        }
                        if let Err(e) = db::store_inventory_and_party(
                            &server.db,
                            character_id,
                            &state.bag,
                            &state.party,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "could not store bag and party");
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
                    player = player_id,
                    map_group = profile.map_group,
                    map_num = profile.map_num,
                    x = profile.x,
                    y = profile.y,
                    "resyncing a newly attached game"
                );
                // The rates go out again too. They are sent once after Welcome, which is
                // normally before the game has finished booting, and GameLink drops what it
                // cannot deliver -- so a game attaching later would play on the original
                // game's rates while the server ran on different ones, and nothing would look
                // wrong until somebody compared the numbers.
                let rates = server.rates.for_mode(mode);
                server
                    .world
                    .tell(
                        player_id,
                        ServerControl::Rates {
                            experience: rates.experience,
                            encounter: rates.encounter,
                            money: rates.money,
                            items: rates.items,
                            catch: rates.catch,
                            shop_price: rates.shop_price,
                            species_encounter: rates
                                .species_encounter
                                .iter()
                                .map(|(&s, &m)| (s, m))
                                .collect(),
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
                        player = player_id,
                        len = bytes.len(),
                        "refusing an oversized block"
                    );
                    break;
                }
                server.world.route_link_block(player_id, bytes).await;
            }
            ClientControl::BattleEnded => {
                server.world.clear_battle(player_id).await;
            }
            ClientControl::HardReset => {
                // Only a Deadman character hard-resets; a normal client never sends this, and if one
                // did there is nothing to gain -- a wipe destroys everything and grants nothing.
                if mode != "deadman" {
                    continue;
                }
                // The save lock is already held (HardReset is a heavy op). Destroy everything this
                // character owns and reset it to a fresh start; the client resets itself to match.
                db::wipe_character(&server.db, character_id).await?;
                server.world.set_party_count(player_id, 0).await;
                tracing::info!(
                    player = player_id,
                    "deadman hard reset: character wiped to a fresh start"
                );
            }
            ClientControl::BankDeposit | ClientControl::BankWithdraw => {
                // The bank is a Deadman feature. The save lock is held (heavy op), so the read of
                // the wallet and the write of the moved money are one atomic step -- no other
                // writer can interleave and lose or double the money.
                if mode != "deadman" {
                    continue;
                }
                let deposit = matches!(control, ClientControl::BankDeposit);
                let Ok(Some(stored)) = db::load_save(&server.db, character_id).await else {
                    continue;
                };
                let Some(save) = crate::save_parse::parse(&stored) else {
                    continue;
                };
                let carried = save.money();
                let bank = db::bank_balance(&server.db, character_id).await.unwrap_or(0);
                let (new_carried, new_bank) = if deposit {
                    crate::economy::deposit_all(carried, bank)
                } else {
                    crate::economy::withdraw_all(carried, bank)
                };
                // Author the new carried money into the save, proving the image can be rebuilt
                // faithfully before storing -- the same gate every money write goes through.
                let block1 = crate::save_parse::with_money(&save, new_carried);
                let Some(candidate) = crate::save_parse::reauthor(&stored, &block1) else {
                    tracing::warn!(player = player_id, "could not rebuild save for a bank move");
                    continue;
                };
                db::store_save(&server.db, character_id, &candidate).await?;
                db::set_bank_balance(&server.db, character_id, new_bank).await?;
                server
                    .world
                    .tell(
                        player_id,
                        ServerControl::BankState {
                            bank: new_bank,
                            carried: new_carried,
                        },
                    )
                    .await;
                tracing::info!(
                    player = player_id,
                    deposit,
                    bank = new_bank,
                    carried = new_carried,
                    "bank move"
                );
            }
            ClientControl::DropItem { item, quantity } => {
                // The client removed the item from its own bag before sending this (the same trust
                // as every other bag report); the server records the drop and shows it to everyone
                // on the map. The pickup side is where duplication is actually prevented -- a drop
                // is handed to exactly one taker.
                if quantity == 0 || item == 0 {
                    continue;
                }
                if let Some(pose) = server.world.pose_of(player_id).await {
                    server
                        .world
                        .drop_item(pose.map, pose.x, pose.y, item, quantity, None)
                        .await;
                    server.world.broadcast_map_drops(pose.map).await;
                    tracing::info!(player = player_id, item, quantity, "item dropped");
                }
            }
            ClientControl::PickUpItem { id } => {
                // take_drop removes the drop atomically and only if the pickup rules allow, so two
                // players cannot both receive it. Only then is the taker told to add it to their bag.
                if let Some(pose) = server.world.pose_of(player_id).await {
                    if let Some((item, quantity)) =
                        server.world.take_drop(pose.map, id, player_id).await
                    {
                        server
                            .world
                            .tell(player_id, ServerControl::PickedUp { item, quantity })
                            .await;
                        server.world.broadcast_map_drops(pose.map).await;
                        tracing::info!(player = player_id, item, quantity, "item picked up");
                    }
                }
            }
            ClientControl::Goodbye => break,
            ClientControl::Hello { .. }
            | ClientControl::BeginLogin
            | ClientControl::PollLogin { .. } => {
                // Already authenticated; nothing to do.
            }
        }
    }
    Ok(())
}

/// Strip anything the game's text renderer cannot show, and bound the length.
pub(crate) fn sanitize_chat(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    fn empty_locks() -> SaveLocks {
        std::sync::Mutex::new(std::collections::HashMap::new())
    }

    /// One character shares one lock (so overlapping sessions contend); different characters get
    /// independent locks (so they never block each other); and once no session holds it, the entry
    /// does not pin a stale lock forever.
    #[test]
    fn save_locks_are_per_character_and_release() {
        let locks = empty_locks();

        let a = save_lock_in(&locks, 1);
        let b = save_lock_in(&locks, 1);
        assert!(Arc::ptr_eq(&a, &b), "one character shares one lock");

        let other = save_lock_in(&locks, 2);
        assert!(
            !Arc::ptr_eq(&a, &other),
            "different characters get independent locks"
        );

        drop(a);
        drop(b);
        let refreshed = save_lock_in(&locks, 1);
        assert!(
            !Arc::ptr_eq(&refreshed, &other),
            "a freed character gets a fresh lock, not a leaked one"
        );
    }

    /// The point of the lock: a character's read-modify-write cannot lose an update under overlap.
    ///
    /// Each task models one report -- read the stored value, yield (the window where an unlocked
    /// writer would interleave), write value+1. All fifty run on the same character, so all take the
    /// same lock; every increment must survive. Without the lock the yield between load and store
    /// loses updates and the total falls short.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overlapping_writers_on_one_character_lose_no_update() {
        let locks = Arc::new(empty_locks());
        let stored = Arc::new(AtomicI32::new(0));

        let mut handles = Vec::new();
        for _ in 0..50 {
            let locks = locks.clone();
            let stored = stored.clone();
            handles.push(tokio::spawn(async move {
                let lock = save_lock_in(&locks, 42);
                let _guard = lock.lock().await;
                let current = stored.load(Ordering::SeqCst);
                tokio::task::yield_now().await; // an unlocked writer would interleave here
                stored.store(current + 1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            stored.load(Ordering::SeqCst),
            50,
            "every increment survived; the per-character lock let none be clobbered"
        );
    }

    /// The negative control: two *different* characters are not serialised against each other, so an
    /// honest single session per character is never blocked by another character's report.
    #[tokio::test]
    async fn different_characters_do_not_block_each_other() {
        let locks = empty_locks();
        let a = save_lock_in(&locks, 1);
        let b = save_lock_in(&locks, 2);
        let _held_a = a.lock().await;
        // b belongs to a different character; acquiring it must not wait on a's held guard.
        let _held_b = b
            .try_lock()
            .expect("a different character's lock is free while ours is held");
    }
}
