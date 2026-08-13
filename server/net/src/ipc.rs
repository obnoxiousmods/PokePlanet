//! The loopback link to the game client.
//!
//! The game is a single local process, so exactly one connection is live at a time. A new
//! connection replaces the old one, which is what happens when the player restarts the
//! game while the sidecar keeps running.

use pokeplanet_proto::ipc::{self, GameMessage};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The two-world save summaries for the select menu. Kept separate from `profile` because,
    /// unlike a single character's profile, these carry no position for the client to adopt -- so
    /// they are safe to replay to a game that attaches after they were sent, which is exactly the
    /// race that left the world-select menu blank on a fresh launch (the sidecar reaches the server
    /// and gets the summaries before the slower-booting game has attached to receive them).
    mode_profiles: Option<Vec<u8>>,
}

/// How long to wait for a game to come back before following it out. Long enough to cover a
/// relaunch, short enough that a closed game does not leave a process behind.
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

/// Handle used by the rest of the sidecar to talk to whichever game process is attached.
#[derive(Clone, Default)]
pub struct GameLink {
    outbound: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    latched: Arc<Mutex<Latched>>,
    /// Held while a save is being handed to the game, so two never interleave.
    save_run: Arc<Mutex<()>>,
    /// Bumped every time a game attaches, so a pending shutdown can tell whether the game
    /// it was waiting for came back or a different one arrived.
    generation: Arc<AtomicU64>,
    /// True while a game is connected.
    attached: Arc<AtomicBool>,
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

    /// Send the two-world select summaries, remembering them for whichever game attaches next.
    /// Replayed on attach (unlike `profile`) because they carry no position -- see `Latched`.
    pub async fn send_mode_profiles(&self, frame: Vec<u8>) {
        self.latched.lock().await.mode_profiles = Some(frame.clone());
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
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.attached.store(true, Ordering::SeqCst);

        // Replay what the sign-in screen needs. Otherwise a game that attaches after the
        // sidecar has already signed in -- a restart, or just a sidecar that reconnected
        // faster than the game could boot -- waits on the connecting screen forever.
        {
            let latched = self.latched.lock().await;
            // The profile is deliberately NOT replayed, and this was a real bug rather than
            // caution. It is not merely a stale badge count: it carries the position the
            // client adopts, and the save it must agree with arrives on a separate stream
            // read by a separate task. So the save could finish first and the client would
            // adopt the position from whenever this sidecar last signed in -- warping the
            // player to a map they left hours ago, and freezing there, because the rest of
            // the load path had agreed on somewhere else.
            //
            // A game that attaches late asks for the current profile with the same Resync
            // that fetches its save, which is the only copy of either it should act on.
            if let Some(status) = latched.status.as_ref() {
                let _ = tx.try_send(status.clone());
            }
            // The select-menu summaries are safe to replay (no position), and doing so is what
            // lets a game that boots slower than the server handshake still show the world-select
            // menu instead of a blank one. Only meaningful while a pick is pending; once a world is
            // chosen the session stops sending them and the stale copy is harmless.
            if let Some(mode_profiles) = latched.mode_profiles.as_ref() {
                let _ = tx.try_send(mode_profiles.clone());
            }
        }
        *self.outbound.lock().await = Some(tx);
    }

    /// The game has gone. Wait a moment in case it is restarting, then follow it out.
    ///
    /// The sidecar used to outlive the game deliberately, so a relaunch skipped the browser.
    /// That reasoning is stale: the token is cached to disk, so a fresh sidecar signs in
    /// without a browser anyway. What it actually produced was a process that had to be
    /// killed by hand, and -- worse -- one still holding a session and a port that a
    /// different game could then attach to and be signed in as the wrong character.
    ///
    /// The grace period exists because closing and reopening the game is ordinary, and
    /// exiting the instant the socket drops would make every restart a fresh login round
    /// trip. The generation counter is what tells a returning game from a new one.
    async fn detach(&self) {
        *self.outbound.lock().await = None;
        self.attached.store(false, Ordering::SeqCst);

        let generation = self.generation.load(Ordering::SeqCst);
        let seen = self.generation.clone();
        let attached = self.attached.clone();

        tokio::spawn(async move {
            tokio::time::sleep(EXIT_GRACE).await;
            // Still nobody, and nobody has been here since: the game is not coming back.
            if !attached.load(Ordering::SeqCst) && seen.load(Ordering::SeqCst) == generation {
                tracing::info!("the game has gone; shutting down");
                std::process::exit(0);
            }
        });
    }
}

/// Accept game connections forever, forwarding decoded messages to `commands`.
/// How long a freshly started sidecar waits for a game before giving up.
///
/// Generous, because it has to cover the game's whole boot. The point is not to be prompt; it is
/// that a sidecar nobody ever attached to must not live forever.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(90);

pub async fn serve(
    listener: TcpListener,
    link: GameLink,
    commands: mpsc::Sender<GameMessage>,
    instance: String,
) -> anyhow::Result<()> {
    // Nothing has ever attached, so detach() -- the only other way out -- is unreachable. Without
    // this, a sidecar that binds the port and never sees a game runs until it is killed by hand.
    // That happens whenever the game is closed during the seconds between spawning its sidecar
    // and connecting to it.
    {
        let generation = link.generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STARTUP_GRACE).await;
            if generation.load(Ordering::SeqCst) == 0 {
                tracing::info!("no game ever attached; shutting down");
                std::process::exit(0);
            }
        });
    }

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
        let instance = instance.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_game(stream, &link, commands, &instance).await {
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
    instance: &str,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, mut writer) = stream.into_split();

    // A bounded queue means a wedged game cannot make the sidecar grow without limit;
    // snapshots are disposable, so dropping the oldest is the right failure mode.
    // Deep enough for a whole save in 1KB slices plus the snapshot churn beside it.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(512);
    // Deliberately NOT attached yet. See the Hello arm below.
    let mut pending_tx = Some(tx);

    // The pump reads the channel regardless of when the sender is attached, so it is safe to
    // start it here: it simply has nothing to forward until a Hello has been accepted.
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
                // The game introducing itself. A sidecar told which game it belongs to serves
                // only that one. A port is not an identity: two sidecars can be listening on
                // a machine, a player's and a developer's or one left behind by a crash, and
                // without this a game attaches to whichever answers and is signed in as
                // somebody else's character. That is not hypothetical; it happened.
                //
                // A sidecar started by hand carries no token and still serves anyone, which
                // is what test harnesses rely on.
                Ok(GameMessage::Hello { instance: theirs }) => {
                    if !instance.is_empty() && theirs != instance {
                        tracing::warn!("refusing a game that belongs to a different sidecar");
                        anyhow::bail!("instance mismatch");
                    }
                    tracing::info!("game identified itself");

                    // Attach only now, once this really is our game.
                    //
                    // attach() bumps the generation counter, and the shutdown timer armed by
                    // detach() refuses to exit if the generation has moved -- that is how a
                    // restarting game keeps its sidecar alive. Attaching before validation
                    // meant a game that was about to be *rejected* also postponed shutdown.
                    //
                    // Since the instance token was regenerated every launch, relaunching within
                    // the grace period produced exactly that: the new game connected to the old
                    // sidecar, was refused, retried a second later, and each refusal pushed the
                    // shutdown out again. The sidecar became immortal, kept alive by the very
                    // game that could not use it, while holding the port that game needed.
                    if let Some(tx) = pending_tx.take() {
                        link.attach(tx).await;
                    }
                }
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
