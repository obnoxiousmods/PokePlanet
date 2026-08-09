//! Session token cache, so a returning player skips the browser entirely.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize, Default)]
struct StoredToken {
    token: String,
}

/// Reads once at startup and writes through on change. Cheap to clone.
#[derive(Clone)]
pub struct TokenStore {
    path: PathBuf,
    cached: Arc<Mutex<Option<String>>>,
}

impl TokenStore {
    pub fn open(path: PathBuf) -> Self {
        let cached = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoredToken>(&text).ok())
            .map(|s| s.token)
            .filter(|t| !t.is_empty());

        if cached.is_some() {
            tracing::info!(path = %path.display(), "using cached session token");
        }
        Self {
            path,
            cached: Arc::new(Mutex::new(cached)),
        }
    }

    pub fn load(&self) -> Option<String> {
        self.cached.lock().unwrap().clone()
    }

    pub fn store(&self, token: &str) {
        *self.cached.lock().unwrap() = Some(token.to_string());
        let body = match serde_json::to_string_pretty(&StoredToken {
            token: token.to_string(),
        }) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "could not serialise token");
                return;
            }
        };
        if let Err(e) = std::fs::write(&self.path, body) {
            // Not fatal: the player stays signed in for this run and re-authorises next
            // launch, which is far better than refusing to start.
            tracing::warn!(error = %e, path = %self.path.display(), "could not persist token");
        }
    }

    /// Forget the token, so the next connection starts a fresh login.
    pub fn clear(&self) {
        *self.cached.lock().unwrap() = None;
        let _ = std::fs::remove_file(&self.path);
    }
}
