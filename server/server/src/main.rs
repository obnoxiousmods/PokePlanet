//! PokePlanet authoritative server.
//!
//! Three surfaces:
//!   * QUIC on UDP 4433 for game clients (via the `pokeplanet-net` sidecar).
//!   * HTTP on loopback, behind nginx, for the Discord OAuth2 browser flow.
//!   * An IRC bridge to the Solanum daemon, which carries chat.
//!
//! Durable player progress lives in PostgreSQL; presence is in memory.

mod auth;
mod collision;
mod config;
mod db;
mod http;
mod irc;
mod quic;
mod world;

use anyhow::Context;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pokeplanet_server=debug".into()),
        )
        .init();

    // quinn and reqwest both want a process-wide crypto provider installed first.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("could not install the rustls crypto provider"))?;

    let cfg = Arc::new(config::Config::from_env().context("loading configuration")?);
    let db = db::connect(&cfg.database_url).await?;
    // A missing table is not fatal: the server still refuses teleports, it just cannot
    // tell a wall from a path. Say so loudly rather than silently allowing it.
    let collision_path = std::path::PathBuf::from(
        std::env::var("POKEPLANET_COLLISION").unwrap_or_else(|_| "collision.bin".into()),
    );
    let world = match collision::Collision::load(&collision_path) {
        Ok(c) => {
            tracing::info!(maps = c.len(), "loaded map collision");
            world::World::with_collision(c)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %collision_path.display(),
                "no map collision; players will not be stopped by walls"
            );
            world::World::new()
        }
    };

    let server = Arc::new(quic::Server {
        cfg: cfg.clone(),
        db: db.clone(),
        world: world.clone(),
        http: reqwest::Client::builder()
            .user_agent("PokePlanet/0.1 (+https://github.com/obnoxiousmods/PokePlanet)")
            .timeout(std::time::Duration::from_secs(15))
            .build()?,
    });

    let endpoint = quic::endpoint(&cfg).context("binding the QUIC endpoint")?;

    let listener = tokio::net::TcpListener::bind(cfg.http_addr)
        .await
        .with_context(|| format!("binding HTTP on {}", cfg.http_addr))?;
    tracing::info!(addr = %cfg.http_addr, "HTTP listener ready");

    // Expired tickets and sessions accumulate otherwise.
    tokio::spawn({
        let db = db.clone();
        async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                ticker.tick().await;
                if let Err(e) = db::prune(&db).await {
                    tracing::warn!(error = %e, "prune failed");
                }
            }
        }
    });

    tokio::spawn(irc::run(cfg.clone(), world.clone()));
    tokio::spawn(quic::run(server.clone(), endpoint));

    let app = http::router(server);
    let serve = axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
    });
    serve.await?;
    Ok(())
}
