//! Headless game instances, owned by the server.
//!
//! The rules the server enforces today are all rules about a *result*: a level it will accept,
//! a rate of money it will not. Those catch a careless forgery. They cannot catch a careful one,
//! because a careful forgery reports only results that are individually plausible.
//!
//! Running the game is what closes that. An instance is the same binary the player runs, with
//! its display and audio replaced by SDL's dummy drivers, fed the inputs the client reports. The
//! server is then not checking the answer -- it has the answer, and disagreement is the signal.
//!
//! This module is the supervisor: what starts, what stops, and what is refused. It deliberately
//! does not know how to compare states; that belongs with the rules that already exist.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use std::os::unix::fs::OpenOptionsExt;

use tokio::process::{Child, Command};

/// Create a FIFO, returning whether it now exists.
fn make_fifo(path: &std::path::Path) -> bool {
    use std::ffi::CString;

    let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // 0o600: this is one character's input channel and nobody else's business.
    unsafe { libc::mkfifo(c.as_ptr(), 0o600) == 0 }
}

/// How many instances may run at once.
///
/// A real bound rather than a hope. Each instance is a whole game -- its own thread, its own
/// 128KB save mirror, its own decompressed graphics -- so an unbounded supervisor turns a busy
/// evening into an out-of-memory kill that takes the server down with it. Refusing to start the
/// twenty-first instance costs one player their replay validation; running it might cost
/// everyone the server.
const DEFAULT_MAX: usize = 20;

/// One running instance and the pipe that drives it.
struct Running {
    child: Child,
    /// Write end of the instance's input pipe, opened once and kept.
    ///
    /// Held open deliberately. A FIFO reports end-of-file to its reader the moment the last
    /// writer closes, so opening per message would hand the instance a stream that ended after
    /// every keypress.
    input: Option<tokio::fs::File>,
    pipe: PathBuf,
}

pub struct Instances {
    binary: PathBuf,
    max: usize,
    running: HashMap<i64, Running>,
}

impl Instances {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self { binary: binary.into(), max: DEFAULT_MAX, running: HashMap::new() }
    }

    pub fn with_max(mut self, max: usize) -> Self {
        self.max = max;
        self
    }

    pub fn count(&self) -> usize {
        self.running.len()
    }

    pub fn is_running(&self, character_id: i64) -> bool {
        self.running.contains_key(&character_id)
    }

    /// Feed one frame of key state to a character's instance.
    ///
    /// The bits are the game's own, passed through unaltered: the whole value of replay is that
    /// the instance receives what the player's client received, so anything this function
    /// interpreted would be a place the two could differ for a reason that is not cheating.
    ///
    /// A failed write is dropped rather than raised. The instance is an extra check; losing a
    /// frame of it must never become a reason a player's own move fails.
    pub async fn send_input(&mut self, character_id: i64, keys: u16) -> bool {
        let Some(running) = self.running.get_mut(&character_id) else {
            return false;
        };
        let Some(input) = running.input.as_mut() else {
            return false;
        };

        match input.write_all(&keys.to_le_bytes()).await {
            Ok(()) => input.flush().await.is_ok(),
            Err(e) => {
                tracing::debug!(character = character_id, error = %e,
                                "could not drive a validation instance");
                false
            }
        }
    }

    /// Start an instance for a character, if there is room and one is not already up.
    ///
    /// Returns whether an instance is now running for them. A refusal is not an error the caller
    /// has to handle -- validation by replay is an additional check, and the server is no worse
    /// off without it than it was before this existed. It is logged rather than propagated so a
    /// full supervisor cannot stop anybody signing in.
    pub fn start(&mut self, character_id: i64) -> bool {
        if self.running.contains_key(&character_id) {
            return true;
        }
        if self.running.len() >= self.max {
            tracing::warn!(
                character = character_id,
                running = self.running.len(),
                "not starting a validation instance: at capacity"
            );
            return false;
        }

        // A pipe per instance, named after the character so a leftover from a crash is
        // identifiable rather than anonymous.
        let pipe = std::env::temp_dir().join(format!("pokeplanet-input-{character_id}"));
        let _ = std::fs::remove_file(&pipe);
        if !make_fifo(&pipe) {
            tracing::warn!(character = character_id, "could not create an input pipe");
            return false;
        }

        // Dummy drivers rather than a bespoke headless build. The game already runs unmodified
        // this way, which means the instance is the same code the player runs -- the moment it
        // is a *different* build, a disagreement stops being evidence of anything.
        let child = Command::new(&self.binary)
            .env("SDL_VIDEODRIVER", "dummy")
            .env("SDL_AUDIODRIVER", "dummy")
            .env("POKEPLANET_INPUT_PIPE", &pipe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();

        match child {
            Ok(child) => {
                tracing::info!(character = character_id, "started a validation instance");
                // The write end is opened lazily by drive(); opening it here would block until
                // the instance opens its read end, which it does not do until it has booted.
                self.running.insert(character_id, Running { child, input: None, pipe });
                true
            }
            Err(e) => {
                let _ = std::fs::remove_file(&pipe);
                tracing::warn!(character = character_id, error = %e,
                               "could not start a validation instance");
                false
            }
        }
    }

    /// Open the write end for any instance that has started reading.
    ///
    /// Separate from `start` because opening a FIFO for writing blocks until a reader arrives,
    /// and the instance does not open its end until it has booted. Doing it in `start` would
    /// stall the whole server on one instance's startup.
    pub async fn connect_ready(&mut self) {
        for (character, running) in self.running.iter_mut() {
            if running.input.is_some() {
                continue;
            }
            match tokio::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&running.pipe)
                .await
            {
                Ok(file) => {
                    tracing::info!(character = character, "driving a validation instance");
                    running.input = Some(file);
                }
                // ENXIO simply means the instance has not opened its end yet.
                Err(_) => {}
            }
        }
    }

    /// Stop a character's instance. Safe to call when there is none.
    pub async fn stop(&mut self, character_id: i64) {
        if let Some(Running { mut child, pipe, .. }) = self.running.remove(&character_id) {
            let _ = std::fs::remove_file(&pipe);
            // Ask, then insist. `kill_on_drop` would handle it eventually, but an instance that
            // outlives its player is exactly the leak the sidecar taught us to close deliberately
            // rather than leave to a destructor.
            let _ = child.start_kill();
            let _ = child.wait().await;
            tracing::info!(character = character_id, "stopped a validation instance");
        }
    }

    /// Drop instances whose process has already exited, so a crashed one is not counted forever.
    ///
    /// Without this a crash loop silently consumes the capacity above: the map still holds the
    /// entry, `count()` keeps returning the cap, and every subsequent player is refused for a
    /// reason nobody can see.
    pub fn reap(&mut self) {
        self.running.retain(|character, running| match running.child.try_wait() {
            Ok(Some(status)) => {
                let _ = std::fs::remove_file(&running.pipe);
                tracing::warn!(character = character, ?status, "a validation instance exited");
                false
            }
            Ok(None) => true,
            Err(e) => {
                let _ = std::fs::remove_file(&running.pipe);
                tracing::warn!(character = character, error = %e,
                               "could not check a validation instance; dropping it");
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap is real, and it is not simply refusing everything.
    #[tokio::test]
    async fn the_cap_is_enforced_and_is_not_a_blanket_refusal() {
        // `true` exists everywhere and exits immediately, which is all this needs: the point is
        // the bookkeeping, not the game.
        let mut instances = Instances::new("/bin/true").with_max(2);

        assert!(instances.start(1), "the first should start");
        assert!(instances.start(2), "the second should start");
        assert_eq!(instances.count(), 2);

        assert!(!instances.start(3), "the third is over the cap and must be refused");
        assert_eq!(instances.count(), 2, "a refusal must not be recorded as running");

        // Asking again for one already running is not a second instance.
        assert!(instances.start(1), "an existing instance counts as running");
        assert_eq!(instances.count(), 2, "and does not start another");

        instances.stop(1).await;
        assert_eq!(instances.count(), 1, "stopping frees a slot");
        assert!(instances.start(3), "which the next character can then use");
    }

    /// Key bits reach the instance's pipe unaltered.
    ///
    /// `cat` stands in for the game: it opens the read end, which is all the supervisor needs to
    /// see before it can connect, and it lets the bytes be read back and compared. The point is
    /// that what arrives is exactly what was sent -- anything this layer reinterpreted would be a
    /// place the instance and the player's client could disagree for a reason that is not
    /// cheating, which would make every disagreement meaningless.
    #[tokio::test]
    async fn input_reaches_the_pipe_unaltered() {
        let mut instances = Instances::new("/usr/bin/cat");
        assert!(instances.start(4242), "cat should start");

        let pipe = std::env::temp_dir().join("pokeplanet-input-4242");
        assert!(pipe.exists(), "starting an instance must create its pipe");

        // Open a reader, since a FIFO write end cannot be opened until one exists.
        let reader = tokio::task::spawn_blocking({
            let pipe = pipe.clone();
            move || {
                use std::io::Read;
                let mut f = std::fs::File::open(&pipe).expect("read end");
                let mut got = [0u8; 4];
                f.read_exact(&mut got).map(|()| got)
            }
        });

        // connect_ready is retried because the reader may not have opened its end yet.
        for _ in 0..50 {
            instances.connect_ready().await;
            if instances.send_input(4242, 0x0201).await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(instances.send_input(4242, 0x0403).await, "the pipe should accept input");

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), reader)
            .await
            .expect("reader should not hang")
            .expect("reader task")
            .expect("four bytes");

        assert_eq!(
            got,
            [0x01, 0x02, 0x03, 0x04],
            "key bits must arrive little-endian and unchanged"
        );

        // Negative control: input for a character with no instance is refused rather than
        // silently swallowed, or a supervisor that started nothing would look like it was working.
        assert!(!instances.send_input(9999, 0x1234).await, "no instance means no delivery");

        instances.stop(4242).await;
        assert!(!pipe.exists(), "stopping must not leave the pipe behind");
    }

    /// Stopping something that was never started is not an error.
    #[tokio::test]
    async fn stopping_an_absent_instance_is_harmless() {
        let mut instances = Instances::new("/bin/true");
        instances.stop(99).await;
        assert_eq!(instances.count(), 0);
    }

    /// An exited instance stops occupying a slot.
    #[tokio::test]
    async fn reaping_frees_capacity() {
        let mut instances = Instances::new("/bin/true").with_max(1);
        assert!(instances.start(1));

        // /bin/true exits immediately; give it a moment, then reap.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        instances.reap();

        assert_eq!(instances.count(), 0, "an exited instance must not hold a slot");
        assert!(instances.start(2), "and its capacity must be reusable");
    }
}
