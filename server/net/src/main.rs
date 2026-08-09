//! PokePlanet client sidecar. Speaks QUIC to the server and exposes loopback IPC to the game.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        protocol = pokeplanet_proto::PROTOCOL_VERSION,
        "pokeplanet-net scaffolding"
    );
    Ok(())
}
