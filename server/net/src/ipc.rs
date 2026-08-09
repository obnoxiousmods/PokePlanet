//! The loopback link to the game client.
//!
//! The game is a single local process, so exactly one connection is live at a time. A new
//! connection replaces the old one, which is what happens when the player restarts the
//! game while the sidecar keeps running.

use pokeplanet_proto::ipc::{self, GameMessage};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

/// Handle used by the rest of the sidecar to talk to whichever game process is attached.
#[derive(Clone, Default)]
pub struct GameLink {
    outbound: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
}

impl GameLink {
    /// Send an already-framed message. Silently dropped when no game is attached, which
    /// is the normal state while the player is still launching.
    pub async fn send(&self, frame: Vec<u8>) {
        let guard = self.outbound.lock().await;
        if let Some(tx) = guard.as_ref() {
            let _ = tx.try_send(frame);
        }
    }

    async fn attach(&self, tx: mpsc::Sender<Vec<u8>>) {
        *self.outbound.lock().await = Some(tx);
    }

    async fn detach(&self) {
        *self.outbound.lock().await = None;
    }
}

/// Accept game connections forever, forwarding decoded messages to `commands`.
pub async fn serve(
    listener: TcpListener,
    link: GameLink,
    commands: mpsc::Sender<GameMessage>,
) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        // Defence in depth: the listener is bound to loopback, but reject anything else
        // outright rather than trusting the bind alone.
        if !peer.ip().is_loopback() {
            tracing::warn!(%peer, "refusing non-loopback IPC connection");
            continue;
        }
        tracing::info!(%peer, "game attached");
        let link = link.clone();
        let commands = commands.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_game(stream, &link, commands).await {
                tracing::debug!(error = %e, "game connection ended");
            }
            link.detach().await;
            tracing::info!("game detached");
        });
    }
}

async fn handle_game(
    stream: TcpStream,
    link: &GameLink,
    commands: mpsc::Sender<GameMessage>,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, mut writer) = stream.into_split();

    // A bounded queue means a wedged game cannot make the sidecar grow without limit;
    // snapshots are disposable, so dropping the oldest is the right failure mode.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    link.attach(tx).await;

    let pump = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break; // game closed
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Some(body) = ipc::take_frame(&mut buf)? {
            match ipc::decode_game_message(&body) {
                Ok(msg) => {
                    if commands.send(msg).await.is_err() {
                        return Ok(()); // session gone
                    }
                }
                Err(e) => tracing::warn!(error = %e, "undecodable frame from game"),
            }
        }
    }

    pump.abort();
    Ok(())
}
