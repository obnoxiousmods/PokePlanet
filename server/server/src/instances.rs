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

use tokio::process::{Child, Command};

/// How many instances may run at once.
///
/// A real bound rather than a hope. Each instance is a whole game -- its own thread, its own
/// 128KB save mirror, its own decompressed graphics -- so an unbounded supervisor turns a busy
/// evening into an out-of-memory kill that takes the server down with it. Refusing to start the
/// twenty-first instance costs one player their replay validation; running it might cost
/// everyone the server.
const DEFAULT_MAX: usize = 20;

pub struct Instances {
    binary: PathBuf,
    max: usize,
    running: HashMap<i64, Child>,
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

        // Dummy drivers rather than a bespoke headless build. The game already runs unmodified
        // this way, which means the instance is the same code the player runs -- the moment it
        // is a *different* build, a disagreement stops being evidence of anything.
        let child = Command::new(&self.binary)
            .env("SDL_VIDEODRIVER", "dummy")
            .env("SDL_AUDIODRIVER", "dummy")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();

        match child {
            Ok(child) => {
                tracing::info!(character = character_id, "started a validation instance");
                self.running.insert(character_id, child);
                true
            }
            Err(e) => {
                tracing::warn!(character = character_id, error = %e,
                               "could not start a validation instance");
                false
            }
        }
    }

    /// Stop a character's instance. Safe to call when there is none.
    pub async fn stop(&mut self, character_id: i64) {
        if let Some(mut child) = self.running.remove(&character_id) {
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
        self.running.retain(|character, child| match child.try_wait() {
            Ok(Some(status)) => {
                tracing::warn!(character = character, ?status, "a validation instance exited");
                false
            }
            Ok(None) => true,
            Err(e) => {
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
