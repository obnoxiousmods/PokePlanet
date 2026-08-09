//! The browser half of the Discord login, served behind nginx at pokeplanet.obby.ca.

use crate::auth;
use crate::quic::Server;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use std::sync::Arc;

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/health", get(health))
        .with_state(server)
}

async fn health(State(server): State<Arc<Server>>) -> impl IntoResponse {
    format!("ok, {} online\n", server.world.online_count().await)
}

#[derive(Deserialize)]
struct LoginQuery {
    t: String,
}

/// Entry point the game opens in the player's browser. The ticket is carried through
/// Discord as the OAuth2 `state` parameter so the callback knows which game session to
/// attach the resulting login to.
async fn login(State(server): State<Arc<Server>>, Query(q): Query<LoginQuery>) -> impl IntoResponse {
    Redirect::temporary(&auth::authorize_url(&server.cfg, &q.t))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(server): State<Arc<Server>>,
    Query(q): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(err) = q.error {
        return page("Login cancelled", &format!("Discord said: {err}")).into_response();
    }
    let (Some(code), Some(ticket)) = (q.code, q.state) else {
        return (
            StatusCode::BAD_REQUEST,
            page("Bad request", "That login link is missing its code."),
        )
            .into_response();
    };

    let user = match auth::exchange_code(&server.http, &server.cfg, &code).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "code exchange failed");
            return (
                StatusCode::BAD_GATEWAY,
                page("Discord error", "Could not verify that login with Discord."),
            )
                .into_response();
        }
    };

    let (character, token) = match auth::finish_login(&server.db, &server.http, &server.cfg, &user).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "login rejected");
            return (
                StatusCode::FORBIDDEN,
                page("Login refused", "This account cannot sign in."),
            )
                .into_response();
        }
    };

    match crate::db::complete_ticket(&server.db, &ticket, character.id, &token).await {
        Ok(true) => page(
            "You're signed in",
            &format!(
                "Welcome, {}. Head back to PokePlanet &mdash; the game is already picking this up.",
                html_escape(&character.name)
            ),
        )
        .into_response(),
        Ok(false) => (
            StatusCode::GONE,
            page(
                "Login expired",
                "That login took too long. Start it again from the game.",
            ),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not complete ticket");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                page("Server error", "Something went wrong storing that login."),
            )
                .into_response()
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} &middot; PokePlanet</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ margin:0; min-height:100vh; display:grid; place-items:center;
         font:16px/1.6 system-ui,sans-serif; background:#101a14; color:#e8f5ee; }}
  main {{ max-width:32rem; padding:2.5rem; text-align:center; }}
  h1 {{ font-size:1.5rem; margin:0 0 .75rem; color:#6ee7a8; }}
  p {{ margin:0; opacity:.85; }}
</style></head>
<body><main><h1>{title}</h1><p>{body}</p></main></body></html>"#,
        title = html_escape(title),
        body = body,
    ))
}
