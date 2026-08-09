//! PokePlanet network sidecar.
//!
//! Runs beside the game and owns everything the 32-bit game binary should not: the QUIC
//! transport, TLS, and the Discord browser handshake. The game speaks a small fixed-layout
//! protocol to this process over loopback and stays free of network dependencies.

mod browser;
mod ipc;
mod session;
mod settings;
mod tls;
mod token;

use anyhow::Context;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pokeplanet_net=debug".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("could not install the rustls crypto provider"))?;

    let settings = settings::Settings::load(std::env::args().skip(1))?;
    tracing::info!(
        server = %settings.server_host,
        port = settings.server_port,
        ipc = %settings.ipc_addr,
        "pokeplanet-net starting"
    );

    let listener = TcpListener::bind(settings.ipc_addr)
        .await
        .with_context(|| {
            format!(
                "binding the game IPC port {}. Another sidecar may already be running.",
                settings.ipc_addr
            )
        })?;

    let link = ipc::GameLink::default();
    let tokens = token::TokenStore::open(settings.token_path.clone());
    let (commands_tx, commands_rx) = mpsc::channel(256);

    let session = Arc::new(session::Session::new(
        settings,
        link.clone(),
        tokens,
    ));

    tokio::spawn(session.run(commands_rx));

    // Serving the game is the process's main job; if the listener dies, so do we.
    ipc::serve(listener, link, commands_tx).await
}
