//! PostgreSQL access and schema.
//!
//! The schema is the canonical record of player progress: characters, their bag, their
//! party and their boxes. The game client is a reporter of changes, never the owner of
//! them, so every table here is written by the server.

use anyhow::Context;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::{NoTls, Row};

pub type Db = Pool;

/// The at-rest form of a session token: its SHA-256, hex-encoded.
///
/// A 40-character random token is not brute-forceable, so the only threat is a database read
/// leak handing an attacker live sessions. Storing the hash instead of the token closes that:
/// the client still holds and sends the real token, the server hashes what it receives and
/// matches on the hash, and a leaked row is useless. Lookups also accept the raw token, so
/// sessions issued before this keep working until they expire -- a non-breaking migration.
pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

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

-- The character's save, exactly as the game's flash image.
--
-- Stored whole rather than picked apart because the game's own load path reads it back
-- byte for byte, so anything less than the real image would have to be reassembled into
-- one anyway. The projected columns on `characters` stay as they are: they answer "what
-- should the sign-in screen show" without parsing 128KB.
CREATE TABLE IF NOT EXISTS saves (
    character_id BIGINT PRIMARY KEY REFERENCES characters(id) ON DELETE CASCADE,
    image        BYTEA NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Progress counters shown on the sign-in screen.
ALTER TABLE characters ADD COLUMN IF NOT EXISTS badges         SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE characters ADD COLUMN IF NOT EXISTS pokedex_caught INTEGER  NOT NULL DEFAULT 0;
ALTER TABLE characters ADD COLUMN IF NOT EXISTS pokedex_seen   INTEGER  NOT NULL DEFAULT 0;

-- Deadman Mode: an account holds up to one character per mode ('normal' | 'deadman'), each with
-- its own save/party/boxes/money. Existing characters are 'normal'. mode must exist before the
-- name/account uniqueness below reference it. The old single-character-per-account uniqueness
-- (characters_account_id_key) is replaced by uniqueness per (account, mode).
ALTER TABLE characters ADD COLUMN IF NOT EXISTS mode TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE characters DROP CONSTRAINT IF EXISTS characters_account_id_key;
CREATE UNIQUE INDEX IF NOT EXISTS characters_account_mode_idx ON characters (account_id, mode);

-- A character name is how other players address someone in a whisper, so two players
-- cannot share one: a private message would otherwise be delivered to both, and neither
-- sender nor recipient would have any way to tell. Case-insensitively unique, so that
-- addressing someone does not depend on reproducing their capitalisation.
--
-- Deployments predating this may already hold duplicates, and creating the index while
-- they exist would fail and take the server down on startup. The earliest holder keeps the
-- name and later ones are suffixed with their id, which is unique by construction.
-- Uniqueness is per mode: the same display name may exist once in the normal world and once in
-- the deadman world (they are different characters/accounts-of-record), but not twice in one mode.
UPDATE characters c SET name = c.name || '#' || c.id
WHERE EXISTS (
    SELECT 1 FROM characters o
     WHERE lower(o.name) = lower(c.name) AND o.mode = c.mode AND o.id < c.id
);
DROP INDEX IF EXISTS characters_name_key;
CREATE UNIQUE INDEX IF NOT EXISTS characters_name_mode_idx ON characters (lower(name), mode);

-- Existing deployments were created before Littleroot became the default spawn.
ALTER TABLE characters ALTER COLUMN map_group SET DEFAULT 0;
ALTER TABLE characters ALTER COLUMN map_num   SET DEFAULT 9;
ALTER TABLE characters ALTER COLUMN pos_x     SET DEFAULT 17;
ALTER TABLE characters ALTER COLUMN pos_y     SET DEFAULT 18;

-- Hash any session token still stored in plaintext (issued before tokens were hashed at rest), so
-- a database at rest never holds a usable token. encode(sha256(token::bytea),'hex') is exactly what
-- hash_token computes in Rust, so a client's raw token still resolves after this rewrite -- nobody
-- is logged out. A hashed token is 64 lowercase hex chars; anything else is plaintext. Idempotent:
-- once rewritten a token matches the 64-hex pattern and is skipped on every later startup.
UPDATE sessions SET token = encode(sha256(token::bytea), 'hex')
WHERE token !~ '^[0-9a-f]{64}$';
"#;

/// A shared test account so pokeplanet_tester.exe signs in with no Discord login, using the fixed
/// token that build carries.
///
/// Kept OUT of SCHEMA and run only when POKEPLANET_ALLOW_TESTER is set, because the token is a
/// public constant in this repository: applying it to every database that ever runs this code would
/// seed a never-expiring, remotely usable, known-credential login on every deployment -- including
/// anyone else's production server that merely deployed the source. Opt-in keeps the convenience
/// for a server that wants it (set the env var) without making a backdoor the default for all.
///
/// Guarded on the token already existing, so a deployment that made the tester by hand (with a
/// different character id) is left exactly as it is -- no orphan account, no relink. The account
/// holds nothing of value and can be banned like any other if the shared token is abused.
const TESTER_SEED: &str = r#"
DO $$
DECLARE acct BIGINT; ch BIGINT; tok TEXT;
BEGIN
  -- Store the hash, not the raw token: character_for_token matches on the hash only now, and the
  -- client hashes the raw token before it is compared, so the two meet at the hash. sha256 hex here
  -- is the same value hash_token produces in Rust.
  tok := encode(sha256('testertoken-for-local-testing-00000000001'::bytea), 'hex');
  IF NOT EXISTS (SELECT 1 FROM sessions WHERE token = tok) THEN
    INSERT INTO accounts (discord_id, discord_username)
      VALUES ('pokeplanet-tester', 'Tester')
      ON CONFLICT (discord_id) DO NOTHING;
    SELECT id INTO acct FROM accounts WHERE discord_id = 'pokeplanet-tester';
    INSERT INTO characters (account_id, name, graphics_id)
      VALUES (acct, 'Tester', 7)
      ON CONFLICT (account_id) DO NOTHING;
    SELECT id INTO ch FROM characters WHERE account_id = acct;
    INSERT INTO sessions (token, character_id, expires_at)
      VALUES (tok, ch, now() + interval '100 years')
      ON CONFLICT (token) DO NOTHING;
  END IF;
END $$;
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

    // The shared tester login is a public constant, so it is seeded only where a server has
    // explicitly opted in -- never as a side effect of deploying this code. See TESTER_SEED.
    if std::env::var("POKEPLANET_ALLOW_TESTER").is_ok() {
        client
            .batch_execute(TESTER_SEED)
            .await
            .context("seeding the tester account")?;
        tracing::warn!(
            "POKEPLANET_ALLOW_TESTER is set: the shared tester login is enabled on this server"
        );
    }
    Ok(pool)
}

#[derive(Debug, Clone)]
pub struct Character {
    pub id: i64,
    pub account_id: i64,
    /// Which world this character belongs to: "normal" or "deadman". An account holds at most one
    /// of each; the mode is chosen at connect and decides which ruleset/economy (rates) applies.
    pub mode: String,
    pub name: String,
    pub graphics_id: u8,
    pub map_group: u8,
    pub map_num: u8,
    pub x: i16,
    pub y: i16,
    pub facing: u8,
    pub elevation: u8,
    pub play_time_s: i64,
    pub money: i32,
    pub badges: u8,
    pub pokedex_caught: u16,
    pub pokedex_seen: u16,
}

impl Character {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            account_id: row.get("account_id"),
            mode: row.get("mode"),
            name: row.get("name"),
            graphics_id: row.get::<_, i16>("graphics_id") as u8,
            map_group: row.get::<_, i16>("map_group") as u8,
            map_num: row.get::<_, i16>("map_num") as u8,
            x: row.get("pos_x"),
            y: row.get("pos_y"),
            facing: row.get::<_, i16>("facing") as u8,
            elevation: row.get::<_, i16>("elevation") as u8,
            play_time_s: row.get("play_time_s"),
            money: row.get("money"),
            badges: row.get::<_, i16>("badges") as u8,
            pokedex_caught: row.get::<_, i32>("pokedex_caught") as u16,
            pokedex_seen: row.get::<_, i32>("pokedex_seen") as u16,
        }
    }

    /// The save summary the client shows on its sign-in screen.
    pub fn profile(&self) -> pokeplanet_proto::quic::CharacterProfile {
        pokeplanet_proto::quic::CharacterProfile {
            name: self.name.clone(),
            graphics_id: self.graphics_id,
            play_time_seconds: self.play_time_s.max(0) as u32,
            badges: self.badges,
            pokedex_caught: self.pokedex_caught,
            pokedex_seen: self.pokedex_seen,
            money: self.money.max(0) as u32,
            map_group: self.map_group,
            map_num: self.map_num,
            x: self.x,
            y: self.y,
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
/// True if this error is Postgres refusing a duplicate key.
fn is_unique_violation(e: &tokio_postgres::Error) -> bool {
    e.as_db_error().map(|d| d.code().code()) == Some("23505")
}

/// The character for this account, creating it on first sign-in.
///
/// Discord names are not unique and character names have to be, so a new player whose name
/// is taken gets a number appended. Returning players keep whatever name they were given,
/// which is why this looks for an existing character before trying to make one.
pub async fn ensure_character(
    db: &Db,
    account_id: i64,
    mode: &str,
    name: &str,
    graphics_id: u8,
) -> anyhow::Result<Character> {
    let client = db.get().await?;

    if let Some(row) = client
        .query_opt(
            "SELECT * FROM characters WHERE account_id = $1 AND mode = $2",
            &[&account_id, &mode],
        )
        .await?
    {
        return Ok(Character::from_row(&row));
    }

    for suffix in 0..1000u32 {
        let candidate = if suffix == 0 {
            name.to_string()
        } else {
            format!("{name}{suffix}")
        };
        let result = client
            .query_one(
                "INSERT INTO characters (account_id, mode, name, graphics_id)
                 VALUES ($1, $2, $3, $4)
                 RETURNING *",
                &[&account_id, &mode, &candidate, &(graphics_id as i16)],
            )
            .await;

        match result {
            Ok(row) => return Ok(Character::from_row(&row)),
            Err(e) if is_unique_violation(&e) => {
                // Either the name is taken or this account just got a character on another
                // connection. Check the second before assuming the first, or two
                // simultaneous sign-ins would spend a thousand attempts renaming nobody.
                if let Some(row) = client
                    .query_opt(
                        "SELECT * FROM characters WHERE account_id = $1 AND mode = $2",
                        &[&account_id, &mode],
                    )
                    .await?
                {
                    return Ok(Character::from_row(&row));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    anyhow::bail!("no free character name for {name} after 1000 attempts")
}

/// Record the story state read out of a character's save.
///
/// Kept as the game's own flag bitfield and var array rather than interpreted: the useful
/// questions -- has this changed, could it have changed that way -- do not need to know what
/// any individual flag means, and encoding that would mean encoding every script in the game.
/// Replace this character's bag and party with what was read out of their save.
///
/// The save image is still the record; these tables are a projection of it, kept so the server
/// holds progress as data it can query rather than 128KB it can only store. Retiring the image
/// depends on these having been seen to hold everything first.
///
/// Written in one transaction and replaced wholesale rather than merged. A bag is a set, not a
/// log: working out which rows changed would be more code and more ways to be subtly wrong than
/// simply saying what it is now.
pub async fn store_inventory_and_party(
    db: &Db,
    character_id: i64,
    bag: &[(u8, u16, u16)],
    party: &[crate::save_parse::PartyMon],
) -> anyhow::Result<()> {
    let mut client = db.get().await?;
    let tx = client.transaction().await?;

    tx.execute(
        "DELETE FROM inventory WHERE character_id = $1",
        &[&character_id],
    )
    .await?;
    for (slot, (pocket, item, quantity)) in bag.iter().enumerate() {
        // The parser has already refused zero and over-99 quantities, so anything here is
        // storable; the CHECK on the column is the backstop rather than the gate.
        tx.execute(
            "INSERT INTO inventory (character_id, pocket, slot, item_id, quantity)
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &character_id,
                &(*pocket as i16),
                &(slot as i16),
                &(*item as i32),
                &(*quantity as i32),
            ],
        )
        .await?;
    }

    // box_id 0 is the party. The PC boxes are not parsed yet and are left untouched rather
    // than deleted, so this cannot quietly empty something it does not know how to fill.
    tx.execute(
        "DELETE FROM pokemon WHERE character_id = $1 AND box_id = 0",
        &[&character_id],
    )
    .await?;
    for (slot, mon) in party.iter().enumerate() {
        // A record whose decrypted bytes disagree with its own checksum is not stored. The
        // game treats one as a bad egg, and projecting invented numbers into a table that is
        // meant to become authoritative would be worse than having no row at all.
        if !mon.checksum_ok {
            continue;
        }
        tx.execute(
            "INSERT INTO pokemon
                (character_id, box_id, slot, species, level, experience, personality, ot_id, evs)
             VALUES ($1, 0, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &character_id,
                &(slot as i16),
                &(mon.species as i32),
                &(mon.level as i16),
                &(mon.experience as i32),
                &(mon.personality as i64),
                &(mon.ot_id as i64),
                &mon.evs.iter().map(|e| *e as i16).collect::<Vec<i16>>(),
            ],
        )
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn store_story_state(
    db: &Db,
    character_id: i64,
    flags: &[u8],
    vars: &[u8],
) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .execute(
            "INSERT INTO story_state (character_id, flags, vars, updated_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (character_id)
             DO UPDATE SET flags = EXCLUDED.flags, vars = EXCLUDED.vars, updated_at = now()",
            &[&character_id, &flags, &vars],
        )
        .await?;
    Ok(())
}

/// Replace this character's save with `image`.
pub async fn store_save(db: &Db, character_id: i64, image: &[u8]) -> anyhow::Result<()> {
    let client = db.get().await?;
    client
        .execute(
            "INSERT INTO saves (character_id, image, updated_at)
             VALUES ($1, $2, now())
             ON CONFLICT (character_id)
             DO UPDATE SET image = EXCLUDED.image, updated_at = now()",
            &[&character_id, &image],
        )
        .await?;
    Ok(())
}

/// This character's save, or None for one that has never saved.
pub async fn load_save(db: &Db, character_id: i64) -> anyhow::Result<Option<Vec<u8>>> {
    let client = db.get().await?;
    Ok(client
        .query_opt(
            "SELECT image FROM saves WHERE character_id = $1",
            &[&character_id],
        )
        .await?
        .map(|row| row.get::<_, Vec<u8>>("image")))
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
pub async fn save_position(
    db: &Db,
    character_id: i64,
    pose: &pokeplanet_proto::Pose,
) -> anyhow::Result<()> {
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
    let hashed = hash_token(token);
    client
        .execute(
            "INSERT INTO sessions (token, character_id, expires_at)
             VALUES ($1, $2, now() + interval '90 days')",
            &[&hashed, &character_id],
        )
        .await?;
    Ok(())
}

/// The Discord username behind an account.
///
/// Used where the character name is not the right identity: the IRC bridge posts under a single
/// bot nick, so people in the channel would otherwise have no idea which player is speaking. It
/// tags each relayed line with this instead. Returns None on any error rather than failing the
/// caller -- a missing name should cost a nicer label, not the message.
pub async fn discord_username_for_account(db: &Db, account_id: i64) -> Option<String> {
    let client = db.get().await.ok()?;
    let row = client
        .query_opt(
            "SELECT discord_username FROM accounts WHERE id = $1",
            &[&account_id],
        )
        .await
        .ok()??;
    Some(row.get::<_, String>("discord_username"))
}

/// Resolve a session token to its character, rejecting expired ones.
pub async fn character_for_token(db: &Db, token: &str) -> anyhow::Result<Option<Character>> {
    let client = db.get().await?;
    // Joins accounts and excludes banned ones so a ban takes effect at the next sign-in rather
    // than whenever the 90-day session token happens to expire. A banned account's token simply
    // resolves to no character, and the connection is turned away like an unknown token.
    // Match on the hash only. Any token issued before hashing existed was rewritten to its hash by
    // the startup backfill (see SCHEMA), so no token is ever stored or compared in plaintext.
    let hashed = hash_token(token);
    Ok(client
        .query_opt(
            "SELECT c.* FROM sessions s
               JOIN characters c ON c.id = s.character_id
               JOIN accounts a ON a.id = c.account_id
              WHERE s.token = $1 AND s.expires_at > now() AND NOT a.banned",
            &[&hashed],
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

#[cfg(test)]
mod integration {
    //! End-to-end tests against a real Postgres, for the paths the in-memory unit suite cannot
    //! reach: the save round-trip (the one bug class that permanently destroys player data) and
    //! the account/session/ban SQL.
    //!
    //! Gated on POKEPLANET_TEST_DB. When it is unset the test prints a skip line rather than
    //! passing silently -- a silent skip that looks passed is exactly how a decode bug slipped
    //! through here before. CI sets the variable, so it always runs there.
    use super::*;

    #[tokio::test]
    async fn persistence_round_trips_and_bans_take_effect() {
        let Ok(url) = std::env::var("POKEPLANET_TEST_DB") else {
            eprintln!("SKIP: set POKEPLANET_TEST_DB to a scratch database to run this test");
            return;
        };

        let db = connect(&url).await.expect("connect + schema");
        let tag = format!("itest-{}", std::process::id());
        let (account_id, _) = upsert_account(&db, &tag, "IntegrationTester")
            .await
            .expect("upsert account");
        let character = ensure_character(&db, account_id, "normal", "ITester", 7)
            .await
            .expect("ensure character");

        // 1. Save round-trip -- byte for byte. A distinctive 128KB image in, the same image out.
        let image: Vec<u8> = (0..128 * 1024).map(|i| (i as u8) ^ 0x5A).collect();
        store_save(&db, character.id, &image).await.expect("store");
        let back = load_save(&db, character.id)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(back, image, "a stored save must come back byte for byte");

        // 2. A session resolves to its character.
        let token = format!("itest-token-{}", std::process::id());
        issue_session(&db, character.id, &token)
            .await
            .expect("issue");
        let resolved = character_for_token(&db, &token)
            .await
            .expect("query")
            .expect("resolves");
        assert_eq!(resolved.id, character.id, "the session names its character");

        // 3. Banning turns the same token away. Negative control: it resolved a moment ago.
        db.get()
            .await
            .expect("client")
            .execute(
                "UPDATE accounts SET banned = true WHERE id = $1",
                &[&account_id],
            )
            .await
            .expect("ban");
        assert!(
            character_for_token(&db, &token)
                .await
                .expect("query")
                .is_none(),
            "a banned account's token must resolve to nothing"
        );

        // Clean up; the account cascades to character, save and session.
        db.get()
            .await
            .expect("client")
            .execute("DELETE FROM accounts WHERE id = $1", &[&account_id])
            .await
            .expect("cleanup");
    }
}
