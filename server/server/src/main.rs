//! Pymerald authoritative server. QUIC listener plus the HTTP endpoints backing the
//! Discord OAuth2 flow. See `server/README.md`.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        protocol = pymerald_proto::PROTOCOL_VERSION,
        "pymerald-server scaffolding"
    );
    Ok(())
}
