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

/// The two messages a newly attached game cannot do without. Both are sent once, at the
/// moment they change, so without keeping a copy a game that attaches later never learns
/// them.
#[derive(Default)]
struct Latched {
    status: Option<Vec<u8>>,
    profile: Option<Vec<u8>>,
}

/// Handle used by the rest of the sidecar to talk to whichever game process is attached.
#[derive(Clone, Default)]
pub struct GameLink {
    outbound: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    latched: Arc<Mutex<Latched>>,
    /// Held while a save is being handed to the game, so two never interleave.
    save_run: Arc<Mutex<()>>,
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

    /// Send the sign-in state, remembering it for whichever game attaches next.
    pub async fn send_status(&self, frame: Vec<u8>) {
        self.latched.lock().await.status = Some(frame.clone());
        self.send(frame).await;
    }

    /// Send the save summary, remembering it for whichever game attaches next.
    pub async fn send_profile(&self, frame: Vec<u8>) {
        self.latched.lock().await.profile = Some(frame.clone());
        self.send(frame).await;
    }

    /// Hand a whole save to the game, in the slices it should arrive as.
    ///
    /// All of it or none of it, because the game writes each slice straight into its flash
    /// image at the offset the slice names. Two saves can genuinely be in flight at once --
    /// the one sign-in sends and the one a resync asks for moments later -- and they arrive
    /// on separate streams read by separate tasks. Letting those two interleave would build
    /// an image in the game that is half of each and was never valid anywhere. So a save is
    /// delivered under a lock and the later one simply overwrites the earlier in full.
    ///
    /// Deliberately not remembered for replay, unlike the status and profile: an old save
    /// resent to a game that attaches later has the same tearing problem and is stale as
    /// well. A game that attaches late asks for the save it should have, which is what
    /// `ClientControl::Resync` is for.
    ///
    /// Snapshots pour through this same queue ten times a second and losing one costs
    /// nothing, but a missing slice means the save never completes and the player silently
    /// carries on with the copy on this machine. So the queue is deep enough to hold a whole
    /// save alongside snapshot churn -- and this stays non-blocking, because waiting here
    /// would stall the session loop that is also reading the server's control stream.
    pub async fn send_save_image(&self, frames: Vec<Vec<u8>>) {
        let _run = self.save_run.lock().await;
        for frame in frames {
            self.send(frame).await;
        }
    }

    async fn attach(&self, tx: mpsc::Sender<Vec<u8>>) {
        // Replay what the sign-in screen needs. Otherwise a game that attaches after the
        // sidecar has already signed in -- a restart, or just a sidecar that reconnected
        // faster than the game could boot -- waits on the connecting screen forever.
        {
            let latched = self.latched.lock().await;
            // Profile first: the menu reads the save summary as soon as it sees ONLINE.
            //
            // Safe to replay where the save is not: it is a small whole value that the
            // fresh one replaces outright, so the worst case is the sign-in screen showing
            // a slightly old badge count for the few milliseconds before the resync lands.
            if let Some(profile) = latched.profile.as_ref() {
                let _ = tx.try_send(profile.clone());
            }
            if let Some(status) = latched.status.as_ref() {
                let _ = tx.try_send(status.clone());
            }
        }
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
        // The latched frames replayed below are only as fresh as the last sign-in, and this
        // sidecar may have been signed in for hours. Ask for the current ones, which arrive
        // by the same path and overwrite them.
        let _ = commands.try_send(GameMessage::Attached);
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
    // Deep enough for a whole save in 1KB slices plus the snapshot churn beside it.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(512);
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
