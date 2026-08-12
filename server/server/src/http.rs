//! The browser half of the Discord login, served behind nginx at pokeplanet.obby.ca.

use crate::auth;
use crate::db;
use crate::quic::Server;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use std::sync::Arc;

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/ladder/:mode", get(ladder))
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/health", get(health))
        .with_state(server)
}

async fn health(State(server): State<Arc<Server>>) -> impl IntoResponse {
    format!("ok, {} online\n", server.world.online_count().await)
}

/// The landing page: what PokePlanet is, who's online, and the way into each world's ladder.
async fn home(State(server): State<Arc<Server>>) -> impl IntoResponse {
    let online = server.world.online_count().await;
    let body = format!(
        r#"<p class="lede">A server-authoritative Pokemon MMO. Explore one world together --
or step into <strong>Deadman</strong>, where a fainted Pokemon dies forever, progress is capped to
your next gym, and everything you carry is on the line.</p>
<p class="online">{online} trainer{s} online right now.</p>
<div class="cards">
  <a class="card deadman" href="/ladder/deadman"><h2>Deadman ladder</h2><p>The survivors, by how far
they have pushed a life they can lose in an instant.</p></a>
  <a class="card normal" href="/ladder/normal"><h2>Standard ladder</h2><p>The trainers of the open
world, ranked by badges and Pokedex.</p></a>
</div>"#,
        s = if online == 1 { "" } else { "s" },
    );
    page("PokePlanet", &body, None).into_response()
}

/// A world's ladder: the top trainers by badges, Pokedex and time played.
async fn ladder(State(server): State<Arc<Server>>, Path(mode): Path<String>) -> impl IntoResponse {
    let mode = if mode == "deadman" {
        "deadman"
    } else {
        "normal"
    };
    let deadman = mode == "deadman";
    let rows = match db::leaderboard(&server.db, mode, 100).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "leaderboard query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                page(
                    "Ladder",
                    "<p>The ladder is having a moment. Try again shortly.</p>",
                    None,
                ),
            )
                .into_response();
        }
    };

    let mut table = String::from(
        "<table class=\"ladder\"><thead><tr><th>#</th><th>Trainer</th><th>Combat</th>\
         <th>Badges</th><th>Pokedex</th><th>Hours</th></tr></thead><tbody>",
    );
    if rows.is_empty() {
        table.push_str("<tr><td colspan=\"6\" class=\"empty\">No one has set out in this world yet. Be the first.</td></tr>");
    }
    for r in &rows {
        table.push_str(&format!(
            "<tr><td class=\"rank\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            r.rank,
            html_escape(&r.name),
            r.combat_level,
            r.badges,
            r.pokedex_caught,
            r.play_hours,
        ));
    }
    table.push_str("</tbody></table>");

    let title = if deadman {
        "Deadman ladder"
    } else {
        "Standard ladder"
    };
    let body = format!(
        r#"<p class="lede">{}</p><p class="nav"><a href="/">&larr; home</a> &middot; <a href="/ladder/{}">{}</a></p>{table}"#,
        if deadman {
            "Every name here is a life still going. Progress is capped to the next gym; a single death can end a run."
        } else {
            "The open world's trainers, by how far they've come."
        },
        if deadman { "normal" } else { "deadman" },
        if deadman {
            "standard ladder"
        } else {
            "deadman ladder"
        },
    );
    page(title, &body, Some(deadman)).into_response()
}

#[derive(Deserialize)]
struct LoginQuery {
    t: String,
}

/// Entry point the game opens in the player's browser. The ticket is carried through
/// Discord as the OAuth2 `state` parameter so the callback knows which game session to
/// attach the resulting login to.
async fn login(
    State(server): State<Arc<Server>>,
    Query(q): Query<LoginQuery>,
) -> impl IntoResponse {
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
        return page(
            "Login cancelled",
            &format!("<p>Discord said: {}</p>", html_escape(&err)),
            None,
        )
        .into_response();
    }
    let (Some(code), Some(ticket)) = (q.code, q.state) else {
        return (
            StatusCode::BAD_REQUEST,
            page(
                "Bad request",
                "<p>That login link is missing its code.</p>",
                None,
            ),
        )
            .into_response();
    };

    let user = match auth::exchange_code(&server.http, &server.cfg, &code).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "code exchange failed");
            return (
                StatusCode::BAD_GATEWAY,
                page(
                    "Discord error",
                    "<p>Could not verify that login with Discord.</p>",
                    None,
                ),
            )
                .into_response();
        }
    };

    let (character, token) =
        match auth::finish_login(&server.db, &server.http, &server.cfg, &user).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "login rejected");
                return (
                    StatusCode::FORBIDDEN,
                    page("Login refused", "<p>This account cannot sign in.</p>", None),
                )
                    .into_response();
            }
        };

    match crate::db::complete_ticket(&server.db, &ticket, character.id, &token).await {
        Ok(true) => page(
            "You're signed in",
            &format!(
                "<p>Welcome, {}. Head back to PokePlanet &mdash; the game is already picking this up.</p>",
                html_escape(&character.name)
            ),
            None,
        )
        .into_response(),
        Ok(false) => (
            StatusCode::GONE,
            page(
                "Login expired",
                "<p>That login took too long. Start it again from the game.</p>",
                None,
            ),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not complete ticket");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                page(
                    "Server error",
                    "<p>Something went wrong storing that login.</p>",
                    None,
                ),
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

/// The shared site shell. `deadman` selects the palette: `Some(true)` the blood-red Deadman theme,
/// `Some(false)` the standard green, `None` a neutral shell for login/status pages. `body` is placed
/// raw inside `<main>` (callers escape any user data), so it may contain block markup like tables.
fn page(title: &str, body: &str, deadman: Option<bool>) -> Html<String> {
    let (bg, panel, accent, accent2) = match deadman {
        Some(true) => ("#160a0a", "#20100f", "#ef5350", "#ff8a80"),
        Some(false) | None => ("#101a14", "#132018", "#6ee7a8", "#34d399"),
    };
    Html(format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} &middot; PokePlanet</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing:border-box; }}
  body {{ margin:0; min-height:100vh; font:16px/1.6 system-ui,sans-serif;
         background:{bg}; color:#e8f5ee; display:flex; flex-direction:column; }}
  header {{ padding:1rem 1.5rem; border-bottom:1px solid #ffffff14; }}
  header a.brand {{ color:{accent}; font-weight:700; text-decoration:none; letter-spacing:.02em; }}
  main {{ width:100%; max-width:44rem; margin:0 auto; padding:2.5rem 1.5rem; flex:1; }}
  footer {{ padding:1.25rem 1.5rem; border-top:1px solid #ffffff14; opacity:.5; font-size:.85rem; }}
  h1 {{ font-size:1.6rem; margin:0 0 1rem; color:{accent}; }}
  h2 {{ font-size:1.1rem; margin:0 0 .35rem; color:{accent2}; }}
  a {{ color:{accent2}; }}
  p {{ opacity:.9; }}
  .lede {{ font-size:1.1rem; opacity:.95; }}
  .online {{ color:{accent}; font-weight:600; }}
  .nav {{ font-size:.9rem; opacity:.8; }}
  .cards {{ display:grid; gap:1rem; grid-template-columns:1fr; margin-top:1.5rem; }}
  @media (min-width:34rem) {{ .cards {{ grid-template-columns:1fr 1fr; }} }}
  .card {{ display:block; padding:1.25rem; border-radius:.75rem; background:{panel};
          border:1px solid #ffffff14; text-decoration:none; color:inherit; transition:border-color .15s; }}
  .card:hover {{ border-color:{accent}; }}
  .card.deadman h2 {{ color:#ff8a80; }}
  .card p {{ margin:0; opacity:.75; font-size:.92rem; }}
  table.ladder {{ width:100%; border-collapse:collapse; margin-top:1.25rem; }}
  table.ladder th, table.ladder td {{ padding:.5rem .6rem; text-align:left; border-bottom:1px solid #ffffff12; }}
  table.ladder th {{ color:{accent2}; font-size:.8rem; text-transform:uppercase; letter-spacing:.04em; opacity:.8; }}
  table.ladder td.rank {{ color:{accent}; font-variant-numeric:tabular-nums; font-weight:600; }}
  table.ladder td.empty {{ text-align:center; opacity:.6; padding:2rem; }}
</style></head>
<body>
<header><a class="brand" href="/">PokePlanet</a></header>
<main><h1>{title}</h1>{body}</main>
<footer>PokePlanet &middot; a server-authoritative Pokemon MMO</footer>
</body></html>"#,
        title = html_escape(title),
        body = body,
    ))
}
