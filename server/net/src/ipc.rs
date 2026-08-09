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
    /// The stored save, in the slices it arrived as. Latched for the same reason as the
    /// other two and more urgently: it is sent once, immediately after sign-in, which is
    /// normally before the game has even finished booting. Dropping it means the player
    /// silently continues the save on this machine instead of the one the server holds.
    save_image: Vec<Vec<u8>>,
}

/// Handle used by the rest of the sidecar to talk to whichever game process is attached.
#[derive(Clone, Default)]
pub struct GameLink {
    outbound: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    latched: Arc<Mutex<Latched>>,
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

    /// Send one slice of the stored save, remembering the whole run. `first` starts a new
    /// run, so a second sign-in cannot leave slices of an older save in front of a newer one.
    ///
    /// Snapshots pour through this same queue ten times a second and losing one costs
    /// nothing, but a missing slice means the save never completes and the player silently
    /// carries on with the copy on this machine. So the queue is deep enough to hold a whole
    /// save alongside snapshot churn -- and this stays non-blocking, because waiting here
    /// would stall the session loop that is also reading the server's control stream.
    pub async fn send_save_image(&self, frame: Vec<u8>, first: bool) {
        {
            let mut latched = self.latched.lock().await;
            if first {
                latched.save_image.clear();
            }
            latched.save_image.push(frame.clone());
        }

        self.send(frame).await;
    }

    async fn attach(&self, tx: mpsc::Sender<Vec<u8>>) {
        // Replay what the sign-in screen needs. Otherwise a game that attaches after the
        // sidecar has already signed in -- a restart, or just a sidecar that reconnected
        // faster than the game could boot -- waits on the connecting screen forever.
        {
            let latched = self.latched.lock().await;
            // Profile first: the menu reads the save summary as soon as it sees ONLINE.
            if let Some(profile) = latched.profile.as_ref() {
                let _ = tx.try_send(profile.clone());
            }
            if let Some(status) = latched.status.as_ref() {
                let _ = tx.try_send(status.clone());
            }
            // After the status, so the game is already signed in by the time the save it is
            // meant to play arrives.
            for piece in &latched.save_image {
                let _ = tx.try_send(piece.clone());
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
