//! The upstream QUIC session: connect, authenticate, then relay in both directions.

use crate::browser;
use crate::ipc::GameLink;
use crate::settings::Settings;
use crate::token::TokenStore;
use anyhow::Context;
use pokeplanet_proto::ipc as wire;
use pokeplanet_proto::quic::{
    self, ChatTarget, ClientControl, ClientMovement, ServerControl, ServerSnapshot,
};
use pokeplanet_proto::{Pose, PROTOCOL_VERSION};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// How often an unfinished browser login is re-checked with the server.
const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the local player's position is pushed upstream.
const MOVEMENT_INTERVAL: Duration = Duration::from_millis(100);
const MAX_CONTROL_FRAME: usize = 64 * 1024;

pub struct Session {
    settings: Settings,
    link: GameLink,
    tokens: TokenStore,
}

impl Session {
    pub fn new(settings: Settings, link: GameLink, tokens: TokenStore) -> Self {
        Self { settings, link, tokens }
    }

    /// Run forever, reconnecting with backoff. Each attempt reports its status to the
    /// game so the login menu can show something truthful.
    pub async fn run(self: Arc<Self>, mut commands: mpsc::Receiver<wire::GameMessage>) {
        let mut backoff = 1u64;
        loop {
            self.report(wire::AUTH_CONNECTING, "", "").await;
            match self.clone().attempt(&mut commands).await {
                Ok(()) => {
                    tracing::info!("session closed cleanly");
                    backoff = 1;
                }
                Err(e) => tracing::warn!(error = %e, backoff, "session failed; retrying"),
            }
            self.report(wire::AUTH_OFFLINE, "", "").await;
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }

    async fn report(&self, state: u8, name: &str, login_url: &str) {
        self.link
            .send(wire::encode_status(state, name, login_url))
            .await;
    }

    fn endpoint(&self) -> anyhow::Result<Endpoint> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;

        let tls = if self.settings.insecure {
            tracing::warn!("TLS verification disabled (--insecure)");
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(crate::tls::AcceptAnything))
                .with_no_client_auth()
        } else {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        };

        let mut tls = tls;
        tls.alpn_protocols = vec![b"pokeplanet/1".to_vec()];
        let quic_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_tls));

        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
        transport.keep_alive_interval(Some(Duration::from_secs(10)));
        client_config.transport_config(Arc::new(transport));

        endpoint.set_default_client_config(client_config);
        Ok(endpoint)
    }

    async fn attempt(
        self: Arc<Self>,
        commands: &mut mpsc::Receiver<wire::GameMessage>,
    ) -> anyhow::Result<()> {
        let endpoint = self.endpoint()?;
        let addr = tokio::net::lookup_host((
            self.settings.server_host.as_str(),
            self.settings.server_port,
        ))
        .await
        .with_context(|| format!("resolving {}", self.settings.server_host))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no address for {}", self.settings.server_host))?;

        tracing::info!(%addr, host = %self.settings.server_host, "connecting");
        let conn = endpoint
            .connect(addr, &self.settings.server_host)?
            .await
            .context("QUIC handshake failed")?;

        let (mut send, recv) = conn.open_bi().await?;
        // The server does not see a stream until it carries data, so Hello must go first.
        write_control(
            &mut send,
            &ClientControl::Hello {
                protocol_version: PROTOCOL_VERSION,
                token: self.tokens.load(),
                client_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )
        .await?;

        self.relay(conn, send, recv, commands).await
    }

    async fn relay(
        self: Arc<Self>,
        conn: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
        commands: &mut mpsc::Receiver<wire::GameMessage>,
    ) -> anyhow::Result<()> {
        let mut player_name = String::new();
        let mut pending_ticket: Option<String> = None;
        let mut latest_pose: Option<(Pose, u8)> = None;

        let mut movement = tokio::time::interval(MOVEMENT_INTERVAL);
        let mut login_poll = tokio::time::interval(LOGIN_POLL_INTERVAL);

        loop {
            tokio::select! {
                // --- from the server, control stream ---
                frame = read_control(&mut recv) => {
                    let Some(frame) = frame? else { return Ok(()) };
                    match quic::decode::<ServerControl>(&frame)? {
                        ServerControl::Welcome { name, token, graphics_id, player_id } => {
                            tracing::info!(player_id, %name, graphics_id, "signed in");
                            self.tokens.store(&token);
                            player_name = name.clone();
                            pending_ticket = None;
                            self.report(wire::AUTH_ONLINE, &name, "").await;
                        }
                        ServerControl::AuthRequired { ticket, login_url } => {
                            tracing::info!(%login_url, "login required");
                            // Only launch a browser the first time we see a given ticket.
                            // The server re-sends AuthRequired if the game asks again, and
                            // spawning a tab per retry would be hostile.
                            let is_new_ticket = pending_ticket.as_deref() != Some(ticket.as_str());
                            pending_ticket = Some(ticket);
                            self.report(wire::AUTH_NEEDS_LOGIN, "", &login_url).await;
                            if is_new_ticket {
                                self.open_login(&login_url).await;
                                self.report(wire::AUTH_AWAITING_BROWSER, "", &login_url).await;
                            }
                        }
                        ServerControl::LoginPending => {}
                        ServerControl::PlayerJoined { name, .. } => {
                            tracing::debug!(%name, "player joined");
                        }
                        ServerControl::PlayerLeft { player_id } => {
                            tracing::debug!(player_id, "player left");
                        }
                        ServerControl::Chat { from, target, text } => {
                            let kind = match target {
                                ChatTarget::Global => wire::CHAT_GLOBAL,
                                ChatTarget::Local => wire::CHAT_LOCAL,
                                ChatTarget::Private(_) => wire::CHAT_PRIVATE,
                            };
                            self.link.send(wire::encode_chat(kind, &from, &text)).await;
                        }
                        ServerControl::Rejected { reason } => {
                            tracing::error!(%reason, "server rejected this client");
                            // A stale token is the common cause; drop it so the next
                            // attempt starts a fresh browser login instead of looping.
                            self.tokens.clear();
                            anyhow::bail!("{reason}");
                        }
                    }
                }

                // --- from the server, movement datagrams ---
                datagram = conn.read_datagram() => {
                    let bytes = datagram.context("datagram stream ended")?;
                    match quic::decode::<ServerSnapshot>(&bytes) {
                        Ok(snapshot) => {
                            let entries: Vec<wire::SnapshotEntry> = snapshot
                                .players
                                .into_iter()
                                .map(|p| wire::SnapshotEntry {
                                    player_id: p.player_id,
                                    name: p.name,
                                    graphics_id: p.graphics_id,
                                    pose: p.pose,
                                })
                                .collect();
                            self.link.send(wire::encode_snapshot(&entries)).await;
                        }
                        Err(e) => tracing::debug!(error = %e, "bad snapshot"),
                    }
                }

                // --- from the game ---
                Some(msg) = commands.recv() => {
                    match msg {
                        wire::GameMessage::SelfState { pose, graphics_id } => {
                            latest_pose = Some((pose, graphics_id));
                        }
                        wire::GameMessage::BeginLogin => {
                            match &pending_ticket {
                                Some(_) => {
                                    write_control(&mut send, &ClientControl::BeginLogin).await?;
                                }
                                None => {
                                    // Already signed in; re-announce so the menu updates.
                                    self.report(wire::AUTH_ONLINE, &player_name, "").await;
                                }
                            }
                        }
                        wire::GameMessage::CancelLogin => {
                            self.report(wire::AUTH_OFFLINE, "", "").await;
                        }
                        wire::GameMessage::ChatSend { kind, target, text } => {
                            let target = match kind {
                                wire::CHAT_LOCAL => ChatTarget::Local,
                                wire::CHAT_PRIVATE => ChatTarget::Private(target),
                                _ => ChatTarget::Global,
                            };
                            write_control(&mut send, &ClientControl::Chat { target, text }).await?;
                        }
                        wire::GameMessage::Logout => {
                            self.tokens.clear();
                            write_control(&mut send, &ClientControl::Goodbye).await?;
                            return Ok(());
                        }
                    }
                }

                // --- periodic: push our position ---
                _ = movement.tick() => {
                    if let Some((pose, _)) = latest_pose {
                        if let Ok(bytes) = quic::encode(&ClientMovement { pose }) {
                            // Best effort by design; the next tick supersedes a loss.
                            let _ = conn.send_datagram(bytes.into());
                        }
                    }
                }

                // --- periodic: has the browser login finished? ---
                _ = login_poll.tick() => {
                    if let Some(ticket) = pending_ticket.clone() {
                        write_control(&mut send, &ClientControl::PollLogin { ticket }).await?;
                    }
                }
            }
        }
    }

    /// Open the player's browser at the Discord authorisation page.
    pub async fn open_login(&self, url: &str) {
        if let Err(e) = browser::open(url) {
            tracing::warn!(error = %e, %url, "could not open a browser; the player must visit the URL manually");
        }
    }
}

async fn write_control(stream: &mut SendStream, msg: &ClientControl) -> anyhow::Result<()> {
    let body = quic::encode(msg)?;
    stream.write_all(&(body.len() as u32).to_le_bytes()).await?;
    stream.write_all(&body).await?;
    Ok(())
}

async fn read_control(stream: &mut RecvStream) -> anyhow::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    if stream.read_exact(&mut len).await.is_err() {
        return Ok(None);
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
