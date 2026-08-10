//! Discord OAuth2, session tokens, and the guild role grant.

use crate::config::Config;
use crate::db::{self, Db};
use anyhow::Context;
use rand::Rng;
use serde::Deserialize;

/// Overworld sprites a new character can be assigned.
///
/// Hand-picked from `include/constants/event_objects.h`: every entry is a walking human
/// NPC with a full set of directional animations. Deliberately excludes the player
/// sprites (so real players stay distinguishable from Brendan/May), and anything static
/// or non-human such as `OBJ_EVENT_GFX_RAYQUAZA_STILL` (41), which has no walk cycle.
/// The two overworld sprites a player can be.
///
/// Brendan and May, and deliberately only those two. They are the only overworld graphics with
/// complete frame sets -- walking, running, cycling, surfing, fishing and the field moves -- so
/// a player using anything else reverts to Brendan the moment they get on a bike, which is what
/// the assorted NPC sprites here used to do.
///
/// Players are told apart by colour rather than by sprite: the client recolours each character
/// from its id, which gives far more distinct looks than a handful of NPC graphics did and
/// keeps every animation intact. See src/mmo_colour.c.
pub const PLAYER_SPRITES: &[u8] = &[
    0,  // OBJ_EVENT_GFX_BRENDAN_NORMAL
    89, // OBJ_EVENT_GFX_MAY_NORMAL
];

pub fn random_sprite() -> u8 {
    let mut rng = rand::thread_rng();
    PLAYER_SPRITES[rng.gen_range(0..PLAYER_SPRITES.len())]
}

/// URL-safe random identifier used for both login tickets and session tokens.
pub fn random_token() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..40)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

pub fn authorize_url(cfg: &Config, state: &str) -> String {
    format!(
        "https://discord.com/api/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope=identify&state={}&prompt=none",
        urlencoding::encode(&cfg.discord_client_id),
        urlencoding::encode(&cfg.redirect_uri()),
        urlencoding::encode(state),
    )
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
pub struct DiscordUser {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
}

impl DiscordUser {
    /// The name other players see. Capped to the game's 16-byte name field, and stripped
    /// of control characters so it can't corrupt the text renderer.
    pub fn display_name(&self) -> String {
        let raw = self.global_name.as_deref().unwrap_or(&self.username);
        let cleaned: String = raw
            .chars()
            .filter(|c| !c.is_control())
            .take(pokeplanet_proto::ipc::NAME_LEN - 1)
            .collect();
        if cleaned.trim().is_empty() {
            self.username.chars().take(10).collect()
        } else {
            cleaned
        }
    }
}

/// Exchange the OAuth2 authorization code for the user's Discord identity.
pub async fn exchange_code(
    http: &reqwest::Client,
    cfg: &Config,
    code: &str,
) -> anyhow::Result<DiscordUser> {
    let token: TokenResponse = http
        .post("https://discord.com/api/v10/oauth2/token")
        .form(&[
            ("client_id", cfg.discord_client_id.as_str()),
            ("client_secret", cfg.discord_client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &cfg.redirect_uri()),
        ])
        .send()
        .await
        .context("requesting Discord token")?
        .error_for_status()
        .context("Discord rejected the authorization code")?
        .json()
        .await?;

    let user: DiscordUser = http
        .get("https://discord.com/api/v10/users/@me")
        .bearer_auth(&token.access_token)
        .send()
        .await
        .context("requesting Discord identity")?
        .error_for_status()?
        .json()
        .await?;

    Ok(user)
}

#[derive(Deserialize)]
struct GuildRole {
    id: String,
    name: String,
}

/// Grant the configured role in the configured guild.
///
/// Best-effort: a player who is not in the guild, or a bot without Manage Roles, must not
/// block login, so failures are logged rather than propagated.
pub async fn grant_role(http: &reqwest::Client, cfg: &Config, discord_user_id: &str) {
    let (Some(token), Some(guild)) = (&cfg.discord_bot_token, &cfg.discord_guild_id) else {
        return;
    };

    let roles: Vec<GuildRole> = match http
        .get(format!("https://discord.com/api/v10/guilds/{guild}/roles"))
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "could not parse guild roles");
                return;
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "could not list guild roles; is the bot in the guild?");
            return;
        }
    };

    // Case-insensitive: Discord role names are display strings that get renamed and
    // recapitalised, and a grant silently doing nothing over a capital letter is a
    // miserable thing to debug.
    let Some(role) = roles
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(&cfg.discord_role_name))
    else {
        tracing::warn!(
            role = %cfg.discord_role_name,
            "role does not exist in the guild; create it and put it below the bot's highest role"
        );
        return;
    };

    let url = format!(
        "https://discord.com/api/v10/guilds/{guild}/members/{discord_user_id}/roles/{}",
        role.id
    );
    match http
        .put(&url)
        .header("Authorization", format!("Bot {token}"))
        .header("Content-Length", "0")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!(user = %discord_user_id, role = %cfg.discord_role_name, "granted role");
        }
        Ok(r) => {
            // 403 here almost always means the role outranks the bot.
            tracing::warn!(
                status = %r.status(),
                "role grant refused; check Manage Roles and that the role is below the bot's highest role"
            );
        }
        Err(e) => tracing::warn!(error = %e, "role grant request failed"),
    }
}

/// Everything that happens once Discord has told us who the player is.
pub async fn finish_login(
    db: &Db,
    http: &reqwest::Client,
    cfg: &Config,
    user: &DiscordUser,
) -> anyhow::Result<(db::Character, String)> {
    let (account_id, is_new) = db::upsert_account(db, &user.id, &user.username).await?;
    if db::is_banned(db, account_id).await? {
        anyhow::bail!("account is banned");
    }

    let character =
        db::ensure_character(db, account_id, &user.display_name(), random_sprite()).await?;
    let token = random_token();
    db::issue_session(db, character.id, &token).await?;

    if is_new {
        tracing::info!(discord = %user.id, character = character.id, "new player registered");
    }
    grant_role(http, cfg, &user.id).await;

    Ok((character, token))
}
