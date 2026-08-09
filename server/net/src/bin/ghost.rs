//! A headless test player.
//!
//! Connects with an existing session token and walks a small loop, so the real game
//! client can be checked against a second player without needing a second Discord
//! account. Also prints every snapshot it receives, which verifies the server's
//! map-scoped fan-out from the other direction.
//!
//! Usage:
//!   pokeplanet-ghost --token TOKEN [--server HOST[:PORT]] [--map GROUP:NUM]
//!                    [--at X,Y] [--still] [--challenge PLAYER_ID]

use pokeplanet_proto::quic::{
    self, ChatTarget, ClientControl, ClientMovement, ServerControl, ServerSnapshot,
};
use pokeplanet_proto::{MapId, Pose, PROTOCOL_VERSION};
use std::sync::Arc;
use std::time::Duration;

struct Options {
    token: String,
    host: String,
    port: u16,
    map: MapId,
    x: i16,
    y: i16,
    still: bool,
    /// Challenge this player id a few seconds after signing in, so the receiving side of
    /// the invitation flow can be exercised without a second human.
    challenge: Option<u32>,
    /// Say something in global chat shortly after signing in, so the receiving
    /// client's chat display can be exercised.
    say: Option<String>,
}

fn parse_options() -> anyhow::Result<Options> {
    let mut opts = Options {
        token: String::new(),
        host: "pokeplanet.obby.ca".into(),
        port: 4433,
        map: MapId::new(0, 9), // Littleroot Town
        x: 17,
        y: 18,
        still: false,
        challenge: None,
        say: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut next = || {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--token" => opts.token = next()?,
            "--server" => {
                let v = next()?;
                match v.rsplit_once(':') {
                    Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
                        opts.host = h.into();
                        opts.port = p.parse()?;
                    }
                    _ => opts.host = v,
                }
            }
            "--map" => {
                let v = next()?;
                let (g, n) = v
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("--map wants GROUP:NUM"))?;
                opts.map = MapId::new(g.parse()?, n.parse()?);
            }
            "--at" => {
                let v = next()?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| anyhow::anyhow!("--at wants X,Y"))?;
                opts.x = x.parse()?;
                opts.y = y.parse()?;
            }
            "--still" => opts.still = true,
            "--challenge" => opts.challenge = Some(next()?.parse()?),
            "--say" => opts.say = Some(next()?),
            other => anyhow::bail!("unrecognised argument {other}"),
        }
    }
    anyhow::ensure!(!opts.token.is_empty(), "--token is required");
    Ok(opts)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("could not install the rustls crypto provider"))?;

    let mut opts = parse_options()?;

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"pokeplanet/1".to_vec()];
    let mut client_config =
        quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    client_config.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(client_config);

    let addr = tokio::net::lookup_host((opts.host.as_str(), opts.port))
        .await?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no address for {}", opts.host))?;
    tracing::info!(%addr, "connecting");
    let conn = endpoint.connect(addr, &opts.host)?.await?;

    let (mut send, mut recv) = conn.open_bi().await?;
    let hello = quic::encode(&ClientControl::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: Some(opts.token.clone()),
        client_version: format!("ghost/{}", env!("CARGO_PKG_VERSION")),
    })?;
    send.write_all(&(hello.len() as u32).to_le_bytes()).await?;
    send.write_all(&hello).await?;

    // The reader owns  but not , so anything it wants to reply to goes back to
    // the main loop through here. Auto-accepting is what lets the *outgoing* half of the
    // invitation flow be tested: a real client challenges the ghost and gets an answer.
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<u32>(8);

    // Read the control stream in the background so Welcome and chat are visible.
    tokio::spawn(async move {
        loop {
            let mut len = [0u8; 4];
            if recv.read_exact(&mut len).await.is_err() {
                tracing::warn!("control stream closed");
                return;
            }
            let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
            if recv.read_exact(&mut body).await.is_err() {
                return;
            }
            match quic::decode::<ServerControl>(&body) {
                Ok(ServerControl::Welcome { player_id, profile, .. }) => {
                    tracing::info!(
                        player_id, name = %profile.name, graphics_id = profile.graphics_id,
                        "signed in"
                    );
                }
                Ok(ServerControl::AuthRequired { .. }) => {
                    tracing::error!("token rejected; the server wants a browser login");
                }
                Ok(ServerControl::BattleInvitation { from, from_name }) => {
                    tracing::info!(from, %from_name, "challenged; accepting");
                    let _ = reply_tx.send(from).await;
                }
                Ok(ServerControl::Rejected { reason }) => tracing::error!(%reason, "rejected"),
                Ok(other) => tracing::info!(?other, "control"),
                Err(e) => tracing::warn!(error = %e, "undecodable control frame"),
            }
        }
    });

    // Walk a four-tile square so the real client has visible movement to animate.
    // DIR: 1=south 2=north 3=west 4=east
    let loop_path: [(i16, i16, u8); 8] = [
        (1, 0, 4), (1, 0, 4), (0, 1, 1), (0, 1, 1),
        (-1, 0, 3), (-1, 0, 3), (0, -1, 2), (0, -1, 2),
    ];
    let mut step = 0usize;
    let mut pose = Pose {
        map: opts.map,
        x: opts.x,
        y: opts.y,
        facing: 1,
        elevation: 3,
        moving: false,
    };

    // The first tick of an interval fires immediately, so consume it here; the challenge
    // should land a few seconds after sign-in, once the other client is actually in the
    // world and able to be interrupted.
    let mut challenge_at = tokio::time::interval(Duration::from_secs(6));
    challenge_at.tick().await;
    let mut say_at = tokio::time::interval(Duration::from_secs(8));
    say_at.tick().await;

    let mut movement = tokio::time::interval(Duration::from_millis(100));
    let mut walk = tokio::time::interval(Duration::from_millis(500));
    let mut reported = tokio::time::interval(Duration::from_secs(5));

    tracing::info!(map = ?opts.map, x = pose.x, y = pose.y, still = opts.still, "walking");

    loop {
        tokio::select! {
            datagram = conn.read_datagram() => {
                let bytes = datagram?;
                if let Ok(snap) = quic::decode::<ServerSnapshot>(&bytes) {
                    if !snap.players.is_empty() {
                        for p in &snap.players {
                            tracing::info!(
                                id = p.player_id, name = %p.name, gfx = p.graphics_id,
                                x = p.pose.x, y = p.pose.y,
                                map = format!("{}:{}", p.pose.map.group, p.pose.map.num),
                                "sees"
                            );
                        }
                    }
                }
            }
            _ = walk.tick(), if !opts.still => {
                let (dx, dy, facing) = loop_path[step % loop_path.len()];
                step += 1;
                pose.x += dx;
                pose.y += dy;
                pose.facing = facing;
            }
            _ = movement.tick() => {
                let bytes = quic::encode(&ClientMovement { pose })?;
                let _ = conn.send_datagram(bytes.into());
            }
            _ = say_at.tick(), if opts.say.is_some() => {
                if let Some(text) = opts.say.take() {
                    let msg = quic::encode(&ClientControl::Chat {
                        target: ChatTarget::Global,
                        text: text.clone(),
                    })?;
                    send.write_all(&(msg.len() as u32).to_le_bytes()).await?;
                    send.write_all(&msg).await?;
                    tracing::info!(%text, "said");
                }
            }
            _ = challenge_at.tick(), if opts.challenge.is_some() => {
                if let Some(target) = opts.challenge.take() {
                    let msg = quic::encode(&ClientControl::RequestBattle { target })?;
                    send.write_all(&(msg.len() as u32).to_le_bytes()).await?;
                    send.write_all(&msg).await?;
                    tracing::info!(target, "challenge sent");
                }
            }
            Some(from) = reply_rx.recv() => {
                let msg = quic::encode(&ClientControl::RespondToBattle { from, accepted: true })?;
                send.write_all(&(msg.len() as u32).to_le_bytes()).await?;
                send.write_all(&msg).await?;
                tracing::info!(from, "accepted challenge");
            }
            _ = reported.tick() => {
                tracing::debug!(x = pose.x, y = pose.y, "still here");
            }
        }
    }
}
