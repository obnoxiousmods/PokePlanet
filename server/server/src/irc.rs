//! Bridge between in-game chat and the Solanum IRC daemon on lucy.
//!
//! A single relay bot joins the configured channel. In-game global chat is echoed to IRC
//! and IRC channel traffic is injected back into the game, so players in the overworld and
//! people sitting in the channel (or The Lounge) share one conversation.

use crate::config::Config;
use crate::world::SharedWorld;
use pokeplanet_proto::quic::ChatTarget;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

/// Set once the bridge is running; `relay_to_irc` is a no-op until then.
static OUTBOUND: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// Queue an in-game message for the IRC channel.
///
/// Only global chat crosses the bridge. Local chat is map-scoped and would be noise, and
/// private messages must not be echoed into a public channel.
pub fn relay_to_irc(from: &str, target: &ChatTarget, text: &str) {
    if !matches!(target, ChatTarget::Global) {
        return;
    }
    if let Some(tx) = OUTBOUND.get() {
        let _ = tx.try_send(format!("<{from}> {text}"));
    }
}

/// Accept any certificate. Only used for loopback connections, where the peer is another
/// process on this machine and the TLS session is never exposed to the network.
#[derive(Debug)]
struct AcceptLoopback;

impl rustls::client::danger::ServerCertVerifier for AcceptLoopback {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Run the bridge, reconnecting forever. Never returns.
pub async fn run(cfg: Arc<Config>, world: SharedWorld) {
    if !cfg.irc_enabled {
        tracing::info!("IRC bridge disabled");
        return;
    }
    let (tx, mut rx) = mpsc::channel::<String>(256);
    if OUTBOUND.set(tx).is_err() {
        return;
    }

    let mut backoff = 2u64;
    loop {
        match session(&cfg, &world, &mut rx).await {
            Ok(()) => {
                tracing::warn!("IRC connection closed; reconnecting");
                backoff = 2;
            }
            Err(e) => {
                tracing::warn!(error = %e, backoff, "IRC bridge error; retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn session(
    cfg: &Config,
    world: &SharedWorld,
    outbound: &mut mpsc::Receiver<String>,
) -> anyhow::Result<()> {
    let tcp = TcpStream::connect((cfg.irc_host.as_str(), cfg.irc_port)).await?;
    tcp.set_nodelay(true)?;

    let is_loopback =
        cfg.irc_host == "127.0.0.1" || cfg.irc_host == "::1" || cfg.irc_host == "localhost";
    let tls_config = if is_loopback {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptLoopback))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };

    let server_name = rustls::pki_types::ServerName::try_from(cfg.irc_host.clone())?;
    let stream = TlsConnector::from(Arc::new(tls_config))
        .connect(server_name, tcp)
        .await?;

    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();

    // The nick can change if the server says it is taken (see 433 below), so it is owned here.
    let mut nick = "PokePlanet".to_string();
    write_half
        .write_all(
            format!("NICK {nick}\r\nUSER pokeplanet 0 * :PokePlanet game bridge\r\n").as_bytes(),
        )
        .await?;
    tracing::info!(host = %cfg.irc_host, channel = %cfg.irc_channel, "IRC bridge connecting");

    // Detecting a wedged connection, which is what an ircd restart leaves behind: the socket
    // stays open (so nothing errors) but nothing arrives. Without this the bridge sat on a dead
    // connection forever, silently dropping every message. The heartbeat sends a PING when the
    // link has been quiet, and gives up if that PING is not answered by the next tick.
    let mut joined = false;
    let start = tokio::time::Instant::now();
    let register_deadline = start + std::time::Duration::from_secs(30);
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
    heartbeat.tick().await; // the first tick fires immediately; skip it
    let mut awaiting_pong = false;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                awaiting_pong = false; // any line proves the link is alive
                let Some(line) = line? else { return Ok(()) };
                // Keep the connection alive.
                if let Some(payload) = line.strip_prefix("PING ") {
                    write_half.write_all(format!("PONG {payload}\r\n").as_bytes()).await?;
                    continue;
                }
                // 433 is ERR_NICKNAMEINUSE. After an ircd restart a ghost of our old session can
                // still hold the nick, and without handling this registration never finishes and
                // the bridge is stuck. Take a variant and try again.
                if line.contains(" 433 ") {
                    nick.push('_');
                    tracing::warn!(%nick, "IRC nick was taken; retrying with a variant");
                    write_half.write_all(format!("NICK {nick}\r\n").as_bytes()).await?;
                    continue;
                }
                // 001 is RPL_WELCOME: registration finished, safe to join.
                if !joined && line.contains(" 001 ") {
                    write_half
                        .write_all(format!("JOIN {}\r\n", cfg.irc_channel).as_bytes())
                        .await?;
                    joined = true;
                    tracing::info!(channel = %cfg.irc_channel, "IRC bridge joined");
                    continue;
                }
                if let Some((sender, target, text)) = parse_privmsg(&line) {
                    // Ignore our own echoes and anything outside the bridged channel.
                    if sender != nick && target.eq_ignore_ascii_case(&cfg.irc_channel) {
                        world.inject_chat(&sender, &text).await;
                    }
                }
            }
            msg = outbound.recv() => {
                let Some(msg) = msg else { return Ok(()) };
                if joined {
                    let line = format!("PRIVMSG {} :{}\r\n", cfg.irc_channel, msg);
                    write_half.write_all(line.as_bytes()).await?;
                }
            }
            _ = heartbeat.tick() => {
                if !joined && tokio::time::Instant::now() >= register_deadline {
                    anyhow::bail!("IRC registration did not complete in time; reconnecting");
                }
                if awaiting_pong {
                    // A PING went unanswered since the last tick: the link is dead.
                    anyhow::bail!("IRC connection went silent; reconnecting");
                }
                write_half.write_all(b"PING :pokeplanet-keepalive\r\n").await?;
                awaiting_pong = true;
            }
        }
    }
}

/// Pull sender, target and text out of `:nick!user@host PRIVMSG #chan :message`.
fn parse_privmsg(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix(':')?;
    let (prefix, rest) = rest.split_once(' ')?;
    let sender = prefix.split('!').next()?.to_string();
    let rest = rest.strip_prefix("PRIVMSG ")?;
    let (target, text) = rest.split_once(" :")?;
    Some((sender, target.to_string(), text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_channel_message() {
        let got = parse_privmsg(":ash!~a@host PRIVMSG #pokeplanet :hello there").unwrap();
        assert_eq!(got.0, "ash");
        assert_eq!(got.1, "#pokeplanet");
        assert_eq!(got.2, "hello there");
    }

    #[test]
    fn a_message_containing_a_colon_keeps_its_text_intact() {
        let got = parse_privmsg(":a!b@c PRIVMSG #x :see: this").unwrap();
        assert_eq!(got.2, "see: this");
    }

    #[test]
    fn non_privmsg_lines_are_ignored() {
        assert!(parse_privmsg(":server 001 nick :Welcome").is_none());
        assert!(parse_privmsg("PING :abc").is_none());
    }
}
