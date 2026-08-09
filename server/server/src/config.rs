//! Runtime configuration, all from the environment so secrets stay in a root-only
//! systemd `EnvironmentFile` and never touch the repository.

use anyhow::Context;
use std::net::SocketAddr;
use std::path::PathBuf;

pub struct Config {
    pub quic_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub cert_chain: PathBuf,
    pub private_key: PathBuf,
    pub database_url: String,
    /// Externally reachable base URL, used to build the Discord redirect.
    pub public_url: String,

    pub discord_client_id: String,
    pub discord_client_secret: String,
    pub discord_bot_token: Option<String>,
    pub discord_guild_id: Option<String>,
    /// Role granted to a player on first successful login.
    pub discord_role_name: String,

    pub irc_host: String,
    pub irc_port: u16,
    pub irc_channel: String,
    pub irc_enabled: bool,
}

fn var(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("{key} must be set"))
}

fn var_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            quic_addr: var_or("POKEPLANET_QUIC_ADDR", "0.0.0.0:4433").parse()?,
            http_addr: var_or("POKEPLANET_HTTP_ADDR", "127.0.0.1:8790").parse()?,
            cert_chain: var_or(
                "POKEPLANET_CERT",
                "/etc/letsencrypt/live/obby.ca/fullchain.pem",
            )
            .into(),
            private_key: var_or("POKEPLANET_KEY", "/etc/letsencrypt/live/obby.ca/privkey.pem")
                .into(),
            // Default to the local unix socket; '/' is percent-encoded in the host field.
            database_url: var_or(
                "POKEPLANET_DB",
                "postgres://pokeplanet@%2Frun%2Fpostgresql/pokeplanet",
            ),
            public_url: var_or("POKEPLANET_PUBLIC_URL", "https://pokeplanet.obby.ca"),

            discord_client_id: var("DISCORD_CLIENT_ID")?,
            discord_client_secret: var("DISCORD_CLIENT_SECRET")?,
            discord_bot_token: opt("DISCORD_BOT_TOKEN"),
            discord_guild_id: opt("DISCORD_GUILD_ID"),
            discord_role_name: var_or("DISCORD_ROLE_NAME", "PokePlanet"),

            irc_host: var_or("POKEPLANET_IRC_HOST", "127.0.0.1"),
            irc_port: var_or("POKEPLANET_IRC_PORT", "6697").parse()?,
            irc_channel: var_or("POKEPLANET_IRC_CHANNEL", "#pokeplanet"),
            irc_enabled: var_or("POKEPLANET_IRC_ENABLED", "1") != "0",
        })
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.public_url.trim_end_matches('/'))
    }

    pub fn login_url(&self, ticket: &str) -> String {
        format!(
            "{}/login?t={}",
            self.public_url.trim_end_matches('/'),
            urlencoding::encode(ticket)
        )
    }
}
