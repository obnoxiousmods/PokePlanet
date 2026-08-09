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

pub struct Server {
    pub cfg: Arc<Config>,
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
    let name = character.name.clone();
    let token = auth::random_token();
    db::issue_session(&server.db, character.id, &token).await?;

    write_frame(
        &mut send,
        &ServerControl::Welcome {
            player_id,
            profile: character.profile(),
            token,
        },
    )
    .await?;

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
        async move { movement_loop(server, conn, player_id).await }
    });

    // Control messages from the client until it hangs up.
    let result = control_loop(&server, &mut recv, player_id, &name).await;

    server.world.leave(player_id).await;
    if let Some(pose) = server.world.pose_of(player_id).await {
        let _ = db::save_position(&server.db, character.id, &pose).await;
    }
    movement.abort();
    writer.abort();
    tracing::info!(player = player_id, %name, "player offline");
    result
}

async fn control_loop(
    server: &Arc<Server>,
    recv: &mut RecvStream,
    player_id: PlayerId,
    name: &str,
) -> anyhow::Result<()> {
    while let Some(frame) = read_frame(recv).await? {
        match quic::decode::<ClientControl>(&frame)? {
            ClientControl::Chat { target, text } => {
                let text = sanitize_chat(&text);
                if text.is_empty() {
                    continue;
                }
                server.world.route_chat(name, &target, &text).await;
                crate::irc::relay_to_irc(name, &target, &text);
            }
            ClientControl::EnterMap { map } => {
                if let Some(mut pose) = server.world.pose_of(player_id).await {
                    pose.map = map;
                    server.world.update_pose(player_id, pose).await;
                }
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

async fn movement_loop(server: Arc<Server>, conn: Connection, player_id: PlayerId) {
    let mut ticker = tokio::time::interval(SNAPSHOT_INTERVAL);
    let mut save_ticker = tokio::time::interval(POSITION_SAVE_INTERVAL);
    save_ticker.tick().await; // the first tick fires immediately; skip it

    loop {
        tokio::select! {
            datagram = conn.read_datagram() => {
                let Ok(bytes) = datagram else { return };
                match quic::decode::<ClientMovement>(&bytes) {
                    Ok(m) => server.world.update_pose(player_id, m.pose).await,
                    Err(e) => tracing::debug!(player = player_id, error = %e, "bad movement datagram"),
                }
            }
            _ = ticker.tick() => {
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
