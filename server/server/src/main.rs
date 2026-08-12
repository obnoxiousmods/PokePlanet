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
mod deadman;
mod economy;
mod http;
mod instances;
mod irc;
mod quest_flags;
mod quic;
mod rates;
mod save_parse;
mod world;
mod world_items;

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

    // Read before anything can serve a player, and fatal if it will not parse: running with
    // rates nobody chose, while believing otherwise, is worse than not starting.
    let rates_path =
        std::env::var("POKEPLANET_RATES_FILE").unwrap_or_else(|_| "rates.conf".to_string());
    let normal = rates::Rates::load(std::path::Path::new(&rates_path)).context("loading rates")?;
    // The deadman world's rates. If no file is present the deadman world just runs the normal
    // rates, so a server has to opt into the harsher economy rather than get it by surprise.
    let deadman_path = std::env::var("POKEPLANET_RATES_DEADMAN_FILE")
        .unwrap_or_else(|_| "rates.deadman.conf".to_string());
    let deadman = if std::path::Path::new(&deadman_path).exists() {
        rates::Rates::load(std::path::Path::new(&deadman_path)).context("loading deadman rates")?
    } else {
        normal.clone()
    };
    let rates = Arc::new(rates::ModeRates { normal, deadman });
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

    // The game binary, if this host has one. Named by config so a machine without it simply
    // runs without replay validation instead of failing to start.
    let instances = cfg.game_binary.as_ref().and_then(|path| {
        if std::path::Path::new(path).exists() {
            tracing::info!(%path, "replay validation enabled");
            Some(Arc::new(tokio::sync::Mutex::new(
                crate::instances::Instances::new(path),
            )))
        } else {
            tracing::warn!(%path, "game binary not found; replay validation disabled");
            None
        }
    });

    // Reap exited instances on a timer. Without this a crash-looping instance holds its slot
    // forever -- the supervisor keeps counting it as running -- and eventually every player is
    // refused a validation instance for a reason nothing surfaces. Only runs when replay
    // validation is enabled at all.
    if let Some(instances) = instances.clone() {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                instances.lock().await.reap();
            }
        });
    }

    let server = Arc::new(quic::Server {
        cfg: cfg.clone(),
        rates: rates.clone(),
        db: db.clone(),
        world: world.clone(),
        http: reqwest::Client::builder()
            .user_agent("PokePlanet/0.1 (+https://github.com/obnoxiousmods/PokePlanet)")
            .timeout(std::time::Duration::from_secs(15))
            .build()?,
        instances,
        save_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
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
