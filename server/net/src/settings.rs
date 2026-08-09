//! Sidecar configuration: command line, then `pokeemerald.cfg`, then defaults.
//!
//! Sharing `pokeemerald.cfg` with the game means a player edits one file to point at a
//! different server, and the in-game options menu can write it back.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const DEFAULT_SERVER_HOST: &str = "pokeplanet.obby.ca";
pub const DEFAULT_SERVER_PORT: u16 = 4433;
pub const DEFAULT_IPC_PORT: u16 = 38400;

#[derive(Debug, Clone)]
pub struct Settings {
    pub server_host: String,
    pub server_port: u16,
    /// Loopback address the game connects to. Always on 127.0.0.1 so the IPC channel is
    /// never reachable from the network.
    pub ipc_addr: SocketAddr,
    /// Where the session token is cached between launches.
    pub token_path: PathBuf,
    /// Skip certificate verification. For pointing a dev build at a self-signed server;
    /// never appropriate against the real one.
    pub insecure: bool,
    /// Sign in as whoever the cached token already is, and refuse to become anyone else:
    /// no browser login, and the cache is never rewritten.
    ///
    /// This is what makes a second client on the same machine a genuinely separate player.
    /// A Discord login always resolves to the account of whoever is at the keyboard, so
    /// left to itself the test client signs in as the real player, and the two connections
    /// then fight over one identity instead of seeing each other.
    pub fixed_token: bool,
    /// Where to write diagnostics. The game starts the sidecar detached and with no
    /// console, so without this its log goes nowhere and a multiplayer fault leaves no
    /// trace to look at afterwards.
    pub log_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_host: DEFAULT_SERVER_HOST.to_string(),
            server_port: DEFAULT_SERVER_PORT,
            ipc_addr: SocketAddr::from(([127, 0, 0, 1], DEFAULT_IPC_PORT)),
            token_path: PathBuf::from("pokeplanet-auth.json"),
            insecure: false,
            fixed_token: false,
            log_path: None,
        }
    }
}

impl Settings {
    /// Build settings from `pokeemerald.cfg` in the working directory, then let command
    /// line arguments override anything.
    pub fn load(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut settings = Settings::default();
        settings.apply_config_file(Path::new("pokeemerald.cfg"));
        settings.apply_args(args)?;
        Ok(settings)
    }

    /// Read the `key=value` lines the game already uses. Unknown keys are the game's own
    /// display settings and are ignored.
    fn apply_config_file(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "server" if !value.is_empty() => self.server_host = value.to_string(),
                "serverPort" => {
                    if let Ok(p) = value.parse() {
                        self.server_port = p;
                    }
                }
                "sidecarPort" => {
                    if let Ok(p) = value.parse::<u16>() {
                        self.ipc_addr.set_port(p);
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_args(&mut self, mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--server needs a host[:port]"))?;
                    // Accept "host" or "host:port"; bare IPv6 must use the config file.
                    match value.rsplit_once(':') {
                        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                            self.server_host = host.to_string();
                            self.server_port = port.parse()?;
                        }
                        _ => self.server_host = value,
                    }
                }
                "--port" => {
                    self.server_port = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--port needs a number"))?
                        .parse()?;
                }
                "--ipc-port" => {
                    let port: u16 = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--ipc-port needs a number"))?
                        .parse()?;
                    self.ipc_addr.set_port(port);
                }
                "--token" => {
                    self.token_path = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--token needs a path"))?
                        .into();
                }
                "--insecure" => self.insecure = true,
                "--fixed-token" => self.fixed_token = true,
                "--log" => {
                    self.log_path = Some(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--log needs a path"))?
                            .into(),
                    );
                }
                "--help" | "-h" => {
                    println!(
                        "pokeplanet-net — PokePlanet network sidecar\n\n\
                         Options:\n  \
                           --server HOST[:PORT]  game server (default {DEFAULT_SERVER_HOST}:{DEFAULT_SERVER_PORT})\n  \
                           --port PORT           game server port\n  \
                           --ipc-port PORT       loopback port the game connects to (default {DEFAULT_IPC_PORT})\n  \
                           --token PATH          session token cache\n  \
                           --fixed-token         stay signed in as the cached token; never\n                                                 log in through a browser, never rewrite it\n  \
                           --log PATH            also write diagnostics to PATH\n  \
                           --insecure            skip TLS verification (development only)\n"
                    );
                    std::process::exit(0);
                }
                other => anyhow::bail!("unrecognised argument {other}"),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Settings {
        let mut s = Settings::default();
        s.apply_args(args.iter().map(|a| a.to_string())).unwrap();
        s
    }

    #[test]
    fn a_host_with_a_port_is_split() {
        let s = parse(&["--server", "example.com:9999"]);
        assert_eq!(s.server_host, "example.com");
        assert_eq!(s.server_port, 9999);
    }

    #[test]
    fn a_bare_host_keeps_the_default_port() {
        let s = parse(&["--server", "example.com"]);
        assert_eq!(s.server_host, "example.com");
        assert_eq!(s.server_port, DEFAULT_SERVER_PORT);
    }

    #[test]
    fn ipc_stays_on_loopback() {
        let s = parse(&["--ipc-port", "40000"]);
        assert!(s.ipc_addr.ip().is_loopback());
        assert_eq!(s.ipc_addr.port(), 40000);
    }

    #[test]
    fn unknown_arguments_are_an_error_rather_than_ignored() {
        let mut s = Settings::default();
        assert!(s.apply_args(["--nonsense".to_string()].into_iter()).is_err());
    }
}
