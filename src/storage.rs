//! SQLite persistence layer.
//!
//! All database interaction is handled here. The schema is created on first
//! run by [`init_schema`] and is backwards-compatible (every statement uses
//! `CREATE TABLE IF NOT EXISTS`).
//!
//! # Schema overview
//!
//! ```text
//! scrapes     — one row per scrape job (url, timestamp)
//! fields      — key/value pairs extracted from a scrape, FK → scrapes
//! editions    — BotC editions, keyed by short ID (e.g. "tb")
//! roles       — BotC character roles, keyed by ID (e.g. "imp")
//! games       — game sessions, auto-incrementing integer PK
//! game_roles  — ordered role membership for a game, FK → games
//! ```

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use crate::models::{Edition, Field, GameInstance, GameRole, Role, ScrapeRecord};

/// Open (or create) a SQLite database at `path`.
pub fn open_db(path: &str) -> Result<Connection> {
    Ok(Connection::open(path)?)
}

/// Create all tables if they do not already exist.
///
/// Safe to call on every startup — uses `IF NOT EXISTS` throughout.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scrapes (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            url        TEXT NOT NULL,
            scraped_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS fields (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            scrape_id INTEGER NOT NULL REFERENCES scrapes(id),
            name      TEXT NOT NULL,
            value     TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS editions (
            id          TEXT PRIMARY KEY,
            name        TEXT,
            color       TEXT,
            level       TEXT,
            is_official INTEGER
        );
        CREATE TABLE IF NOT EXISTS roles (
            id                   TEXT PRIMARY KEY,
            name                 TEXT,
            team                 TEXT,
            edition              TEXT,
            ability              TEXT,
            flavor               TEXT,
            setup                INTEGER,
            first_night_reminder TEXT,
            other_night_reminder TEXT
        );
        CREATE TABLE IF NOT EXISTS games (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS game_roles (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            game_id  INTEGER NOT NULL REFERENCES games(id),
            role_id  TEXT NOT NULL,
            position INTEGER NOT NULL
        );",
    )?;
    Ok(())
}

/// Upsert a slice of editions into the database.
///
/// Uses `INSERT OR REPLACE` so re-running the `api` command is idempotent.
/// Returns the number of editions written.
pub fn upsert_editions(conn: &Connection, editions: &[Edition]) -> Result<usize> {
    let mut count = 0;
    for e in editions {
        conn.execute(
            "INSERT OR REPLACE INTO editions (id, name, color, level, is_official)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                &e.id,
                &e.name,
                &e.color,
                &e.level,
                e.is_official.map(|b| b as i32),
            ),
        )?;
        count += 1;
    }
    Ok(count)
}

/// Upsert a slice of roles into the database.
///
/// Uses `INSERT OR REPLACE` so re-running the `api` command is idempotent.
/// Returns the number of roles written.
pub fn upsert_roles(conn: &Connection, roles: &[Role]) -> Result<usize> {
    let mut count = 0;
    for r in roles {
        conn.execute(
            "INSERT OR REPLACE INTO roles
             (id, name, team, edition, ability, flavor, setup, first_night_reminder, other_night_reminder)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                &r.id,
                &r.name,
                &r.team,
                &r.edition,
                &r.ability,
                &r.flavor,
                r.setup.map(|b| b as i32),
                &r.first_night_reminder,
                &r.other_night_reminder,
            ),
        )?;
        count += 1;
    }
    Ok(count)
}

/// Retrieve roles, optionally filtered by team and/or edition.
///
/// `None` values for `team` or `edition` act as wildcards (no filter applied).
/// Results are ordered by team then name.
pub fn get_roles(conn: &Connection, team: Option<&str>, edition: Option<&str>) -> Result<Vec<Role>> {
    let sql = "SELECT id, name, team, edition, ability, flavor, setup,
                      first_night_reminder, other_night_reminder
               FROM roles
               WHERE (?1 IS NULL OR team = ?1)
                 AND (?2 IS NULL OR edition = ?2)
               ORDER BY team, name";
    let mut stmt = conn.prepare(sql)?;
    let roles = stmt.query_map([team, edition], |row| {
        Ok(Role {
            id: row.get(0)?,
            name: row.get(1)?,
            team: row.get(2)?,
            edition: row.get(3)?,
            ability: row.get(4)?,
            flavor: row.get(5)?,
            setup: row.get::<_, Option<i32>>(6)?.map(|v| v != 0),
            first_night_reminder: row.get(7)?,
            other_night_reminder: row.get(8)?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(roles)
}

/// Retrieve all stored editions, ordered by ID.
pub fn get_editions(conn: &Connection) -> Result<Vec<Edition>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, level, is_official FROM editions ORDER BY id",
    )?;
    let editions = stmt.query_map([], |row| {
        Ok(Edition {
            id: row.get(0)?,
            name: row.get(1)?,
            color: row.get(2)?,
            level: row.get(3)?,
            is_official: row.get::<_, Option<i32>>(4)?.map(|v| v != 0),
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(editions)
}

/// Create a new game session containing the given ordered role IDs.
///
/// Inserts one row into `games` and one row per role into `game_roles` with a
/// zero-based `position` column that preserves insertion order. Returns the
/// auto-assigned game ID.
pub fn create_game(conn: &Connection, role_ids: &[String]) -> Result<i64> {
    conn.execute(
        "INSERT INTO games (created_at) VALUES (?1)",
        [chrono::Utc::now().to_rfc3339()],
    )?;
    let game_id = conn.last_insert_rowid();
    for (i, role_id) in role_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO game_roles (game_id, role_id, position) VALUES (?1, ?2, ?3)",
            (game_id, role_id, i as i64),
        )?;
    }
    Ok(game_id)
}

/// Retrieve a game by its ID, with role details joined from the `roles` table.
///
/// Returns `None` if no game with that ID exists. Role fields (`name`, `team`,
/// `edition`, `ability`) will be `None` for any role ID not present in the
/// local `roles` table — run `botc api` first to populate it.
pub fn get_game(conn: &Connection, game_id: i64) -> Result<Option<GameInstance>> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM games WHERE id = ?1",
        [game_id],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !exists {
        return Ok(None);
    }
    let created_at: String = conn.query_row(
        "SELECT created_at FROM games WHERE id = ?1",
        [game_id],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT gr.role_id, r.name, r.team, r.edition, r.ability
         FROM game_roles gr
         LEFT JOIN roles r ON r.id = gr.role_id
         WHERE gr.game_id = ?1
         ORDER BY gr.position",
    )?;
    let roles = stmt.query_map([game_id], |row| {
        Ok(GameRole {
            role_id: row.get(0)?,
            name: row.get(1)?,
            team: row.get(2)?,
            edition: row.get(3)?,
            ability: row.get(4)?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(GameInstance { id: game_id, created_at, roles }))
}

/// Insert a scrape record (URL + timestamp) and all its extracted fields.
///
/// Returns the auto-assigned scrape ID.
pub fn insert_scrape(conn: &Connection, record: &ScrapeRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO scrapes (url, scraped_at) VALUES (?1, ?2)",
        (&record.url, &record.scraped_at),
    )?;
    let scrape_id = conn.last_insert_rowid();
    for field in &record.fields {
        conn.execute(
            "INSERT INTO fields (scrape_id, name, value) VALUES (?1, ?2, ?3)",
            (scrape_id, &field.name, &field.value),
        )?;
    }
    Ok(scrape_id)
}

/// Retrieve every stored scrape record with its fields.
pub fn get_all_scrapes(conn: &Connection) -> Result<Vec<ScrapeRecord>> {
    query_scrapes(conn, None)
}

/// Retrieve all scrape records whose URL exactly matches `url`.
pub fn get_scrapes_by_url(conn: &Connection, url: &str) -> Result<Vec<ScrapeRecord>> {
    query_scrapes(conn, Some(url))
}

/// Shared implementation for scrape queries with an optional URL filter.
///
/// Joins `scrapes` with `fields` in a single query and groups rows by scrape
/// ID using a `BTreeMap` to maintain insertion order.
fn query_scrapes(conn: &Connection, url_filter: Option<&str>) -> Result<Vec<ScrapeRecord>> {
    let sql = "SELECT s.id, s.url, s.scraped_at, f.name, f.value
               FROM scrapes s
               LEFT JOIN fields f ON f.scrape_id = s.id
               ORDER BY s.id, f.id";

    let filter_sql = "SELECT s.id, s.url, s.scraped_at, f.name, f.value
                      FROM scrapes s
                      LEFT JOIN fields f ON f.scrape_id = s.id
                      WHERE s.url = ?1
                      ORDER BY s.id, f.id";

    let mut map: BTreeMap<i64, ScrapeRecord> = BTreeMap::new();

    if let Some(url) = url_filter {
        let mut stmt = conn.prepare(filter_sql)?;
        let rows = stmt.query_map([url], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, url, scraped_at, name, value) = row?;
            let record = map.entry(id).or_insert(ScrapeRecord {
                id,
                url,
                scraped_at,
                fields: vec![],
            });
            if let (Some(n), Some(v)) = (name, value) {
                record.fields.push(Field { name: n, value: v });
            }
        }
    } else {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, url, scraped_at, name, value) = row?;
            let record = map.entry(id).or_insert(ScrapeRecord {
                id,
                url,
                scraped_at,
                fields: vec![],
            });
            if let (Some(n), Some(v)) = (name, value) {
                record.fields.push(Field { name: n, value: v });
            }
        }
    }

    Ok(map.into_values().collect())
}
