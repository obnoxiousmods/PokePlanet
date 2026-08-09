//! Pymerald client sidecar. Speaks QUIC to the server and exposes loopback IPC to the game.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        protocol = pymerald_proto::PROTOCOL_VERSION,
        "pymerald-net scaffolding"
    );
    Ok(())
}
