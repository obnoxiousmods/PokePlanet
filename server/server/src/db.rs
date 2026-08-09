//! PostgreSQL access and schema.
//!
//! The schema is the canonical record of player progress: characters, their bag, their
//! party and their boxes. The game client is a reporter of changes, never the owner of
//! them, so every table here is written by the server.

use anyhow::Context;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::{NoTls, Row};

pub type Db = Pool;

/// Applied at startup. Idempotent, so a restart against an existing database is a no-op.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    id               BIGSERIAL PRIMARY KEY,
    discord_id       TEXT NOT NULL UNIQUE,
    discord_username TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    banned           BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS characters (
    id          BIGSERIAL PRIMARY KEY,
    account_id  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    -- Overworld sprite, picked at random from the NPC set when the character is made.
    graphics_id SMALLINT NOT NULL,
    -- Littleroot Town, the centre of the map. Positions are in the game's runtime
    -- coordinate space (ObjectEvent::currentCoords), which is the map template
    -- coordinate plus MAP_OFFSET, because that is what the client reports.
    map_group   SMALLINT NOT NULL DEFAULT 0,
    map_num     SMALLINT NOT NULL DEFAULT 9,
    pos_x       SMALLINT NOT NULL DEFAULT 17,
    pos_y       SMALLINT NOT NULL DEFAULT 18,
    facing      SMALLINT NOT NULL DEFAULT 1,
    elevation   SMALLINT NOT NULL DEFAULT 3,
    money       INTEGER NOT NULL DEFAULT 3000,
    play_time_s BIGINT  NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id)
);

CREATE TABLE IF NOT EXISTS sessions (
    token        TEXT PRIMARY KEY,
    character_id BIGINT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    issued_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_character_idx ON sessions(character_id);

-- Short-lived handoff between the game and the browser half of the Discord flow.
CREATE TABLE IF NOT EXISTS login_tickets (
    ticket       TEXT PRIMARY KEY,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    character_id BIGINT REFERENCES characters(id) ON DELETE CASCADE,
    token        TEXT,
    consumed     BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS inventory (
    character_id BIGINT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    pocket       SMALLINT NOT NULL,
    slot         SMALLINT NOT NULL,
    item_id      INTEGER  NOT NULL,
    quantity     INTEGER  NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (character_id, pocket, slot)
);

-- box_id 0 is the party; 1..14 are the PC boxes.
CREATE TABLE IF NOT EXISTS pokemon (
    id            BIGSERIAL PRIMARY KEY,
    character_id  BIGINT   NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    box_id        SMALLINT NOT NULL,
    slot          SMALLINT NOT NULL,
    species       INTEGER  NOT NULL,
    nickname      TEXT,
    level         SMALLINT NOT NULL CHECK (level BETWEEN 1 AND 100),
    experience    INTEGER  NOT NULL DEFAULT 0 CHECK (experience >= 0),
    held_item     INTEGER  NOT NULL DEFAULT 0,
    personality   BIGINT   NOT NULL,
    ot_id         BIGINT   NOT NULL,
    ot_name       TEXT     NOT NULL DEFAULT '',
    friendship    SMALLINT NOT NULL DEFAULT 70,
    met_level     SMALLINT NOT NULL DEFAULT 5,
    met_location  SMALLINT NOT NULL DEFAULT 0,
    poke_ball     SMALLINT NOT NULL DEFAULT 4,
    is_egg        BOOLEAN  NOT NULL DEFAULT false,
    current_hp    SMALLINT NOT NULL DEFAULT 0,
    status        INTEGER  NOT NULL DEFAULT 0,
    -- Effort and individual values, in the game's HP/Atk/Def/Spe/SpA/SpD order.
    evs           SMALLINT[6] NOT NULL DEFAULT '{0,0,0,0,0,0}',
    ivs           SMALLINT[6] NOT NULL DEFAULT '{0,0,0,0,0,0}',
    moves         INTEGER[4]  NOT NULL DEFAULT '{0,0,0,0}',
    move_pp       SMALLINT[4] NOT NULL DEFAULT '{0,0,0,0}',
    pp_bonuses    SMALLINT NOT NULL DEFAULT 0,
    caught_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (character_id, box_id, slot)
);
CREATE INDEX IF NOT EXISTS pokemon_character_idx ON pokemon(character_id);

-- Story progression. The game tracks quests as a flag bitfield and a var array in
-- SaveBlock1; mirroring them verbatim keeps the server authoritative over where a player
-- is in the story without reinterpreting every script in the game.
CREATE TABLE IF NOT EXISTS story_state (
    character_id BIGINT PRIMARY KEY REFERENCES characters(id) ON DELETE CASCADE,
    flags        BYTEA NOT NULL DEFAULT ''::bytea,
    vars         BYTEA NOT NULL DEFAULT ''::bytea,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Existing deployments were created before Littleroot became the default spawn.
ALTER TABLE characters ALTER COLUMN map_group SET DEFAULT 0;
ALTER TABLE characters ALTER COLUMN map_num   SET DEFAULT 9;
ALTER TABLE characters ALTER COLUMN pos_x     SET DEFAULT 17;
ALTER TABLE characters ALTER COLUMN pos_y     SET DEFAULT 18;
"#;

pub async fn connect(url: &str) -> anyhow::Result<Db> {
    let pg_config: tokio_postgres::Config = url
        .parse()
        .with_context(|| format!("parsing POKEPLANET_DB ({url})"))?;
    let mgr = Manager::from_config(
        pg_config,
        NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    let pool = Pool::builder(mgr).max_size(16).build()?;

    let client = pool.get().await.context("connecting to postgres")?;
    client
        .batch_execute(SCHEMA)
        .await
        .context("applying schema")?;
    tracing::info!("database schema is up to date");
    Ok(pool)
}

#[derive(Debug, Clone)]
pub struct Character {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub graphics_id: u8,
    pub map_group: u8,
    pub map_num: u8,
    pub x: i16,
    pub y: i16,
    pub facing: u8,
    pub elevation: u8,
}

impl Character {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            account_id: row.get("account_id"),
            name: row.get("name"),
            graphics_id: row.get::<_, i16>("graphics_id") as u8,
            map_group: row.get::<_, i16>("map_group") as u8,
            map_num: row.get::<_, i16>("map_num") as u8,
            x: row.get("pos_x"),
            y: row.get("pos_y"),
            facing: row.get::<_, i16>("facing") as u8,
            elevation: row.get::<_, i16>("elevation") as u8,
        }
    }
}

/// Create the account if this Discord user is new, and return it.
pub async fn upsert_account(
    db: &Db,
    discord_id: &str,
    username: &str,
) -> anyhow::Result<(i64, bool)> {
    let client = db.get().await?;
    let row = client
        .query_one(
            "INSERT INTO accounts (discord_id, discord_username)
             VALUES ($1, $2)
             ON CONFLICT (discord_id) DO UPDATE
               SET discord_username = EXCLUDED.discord_username,
                   last_seen_at     = now()
             RETURNING id, (xmax = 0) AS inserted",
            &[&discord_id, &username],
        )
        .await?;
    Ok((row.get("id"), row.get("inserted")))
}

pub async fn is_banned(db: &Db, account_id: i64) -> anyhow::Result<bool> {
    let client = db.get().await?;
    let row = client
        .query_one("SELECT banned FROM accounts WHERE id = $1", &[&account_id])
        .await?;
    Ok(row.get("banned"))
}

/// Fetch this account's character, creating one on first login.
///
/// `graphics_id` is only used for a brand new character; an existing one keeps the sprite
/// it was created with so other players see a stable avatar.
pub async fn ensure_character(
    db: &Db,
    account_id: i64,
    name: &str,
    graphics_id: u8,
) -> anyhow::Result<Character> {
    let client = db.get().await?;
    let row = client
        .query_one(
            "INSERT INTO characters (account_id, name, graphics_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (account_id) DO UPDATE SET name = characters.name
             RETURNING *",
            &[&account_id, &name, &(graphics_id as i16)],
        )
        .await?;
    Ok(Character::from_row(&row))
}

pub async fn character_by_id(db: &Db, id: i64) -> anyhow::Result<Option<Character>> {
    let client = db.get().await?;
    Ok(client
        .query_opt("SELECT * FROM characters WHERE id = $1", &[&id])
        .await?
        .as_ref()
        .map(Character::from_row))
}

/// Persist the character's last known overworld position so they resume where they left off.
pub async fn save_position(db: &Db, character_id: i64, pose: &pokeplanet_proto::Pose) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .execute(
            "UPDATE characters
                SET map_group = $2, map_num = $3, pos_x = $4, pos_y = $5,
                    facing = $6, elevation = $7
              WHERE id = $1",
            &[
                &character_id,
                &(pose.map.group as i16),
                &(pose.map.num as i16),
                &pose.x,
                &pose.y,
                &(pose.facing as i16),
                &(pose.elevation as i16),
            ],
        )
        .await?;
    Ok(())
}

pub async fn issue_session(db: &Db, character_id: i64, token: &str) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .execute(
            "INSERT INTO sessions (token, character_id, expires_at)
             VALUES ($1, $2, now() + interval '90 days')",
            &[&token, &character_id],
        )
        .await?;
    Ok(())
}

/// Resolve a session token to its character, rejecting expired ones.
pub async fn character_for_token(db: &Db, token: &str) -> anyhow::Result<Option<Character>> {
    let client = db.get().await?;
    Ok(client
        .query_opt(
            "SELECT c.* FROM sessions s
               JOIN characters c ON c.id = s.character_id
              WHERE s.token = $1 AND s.expires_at > now()",
            &[&token],
        )
        .await?
        .as_ref()
        .map(Character::from_row))
}

pub async fn create_ticket(db: &Db, ticket: &str) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .execute(
            "INSERT INTO login_tickets (ticket, expires_at)
             VALUES ($1, now() + interval '10 minutes')",
            &[&ticket],
        )
        .await?;
    Ok(())
}

/// Bind a completed browser login to the ticket the game is polling on.
pub async fn complete_ticket(
    db: &Db,
    ticket: &str,
    character_id: i64,
    token: &str,
) -> anyhow::Result<bool> {
    let client = db.get().await?;
    let n = client
        .execute(
            "UPDATE login_tickets
                SET character_id = $2, token = $3
              WHERE ticket = $1 AND expires_at > now() AND NOT consumed",
            &[&ticket, &character_id, &token],
        )
        .await?;
    Ok(n == 1)
}

/// Claim a finished ticket exactly once. Returns the session token when ready.
pub async fn claim_ticket(db: &Db, ticket: &str) -> anyhow::Result<Option<String>> {
    let client = db.get().await?;
    Ok(client
        .query_opt(
            "UPDATE login_tickets
                SET consumed = true
              WHERE ticket = $1 AND token IS NOT NULL AND NOT consumed
                    AND expires_at > now()
             RETURNING token",
            &[&ticket],
        )
        .await?
        .map(|r| r.get("token")))
}

/// Remove expired tickets and sessions. Called periodically.
pub async fn prune(db: &Db) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .batch_execute(
            "DELETE FROM login_tickets WHERE expires_at < now();
             DELETE FROM sessions      WHERE expires_at < now();",
        )
        .await?;
    Ok(())
}
