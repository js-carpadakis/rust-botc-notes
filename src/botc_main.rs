//! `botc` — Blood on the Clocktower role and game management CLI.
//!
//! Fetches edition and role data from the BotC JSON API and stores it locally,
//! then provides query and game-session management commands against that data.
//!
//! # Subcommands
//!
//! | Command    | Description |
//! |------------|-------------|
//! | `api`      | Fetch the BotC backend API and upsert editions + roles |
//! | `roles`    | List stored roles, optionally filtered by team/edition |
//! | `editions` | List stored editions |
//! | `new-game` | Create a game session with an ordered list of role IDs |
//! | `get-game` | Retrieve a game session and its full role details |
//!
//! # Quick start
//!
//! ```text
//! # Populate the database (JWT token expires every ~30 min; re-run when it does)
//! botc api https://botc.app/backend/data --token <JWT> --cookie <CF_COOKIE>
//!
//! # Query roles
//! botc roles --team demon
//! botc roles --edition tb
//!
//! # Create and retrieve a game
//! botc new-game --role imp --role spy --role washerwoman --role chef
//! botc get-game 1
//! ```

mod models;
mod scraper;
mod storage;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use clap::{Parser, Subcommand};
use models::BotcData;
use storage::{create_game, get_game};
use rusqlite::Connection;

/// Top-level CLI configuration parsed by clap.
#[derive(Parser)]
#[command(name = "botc", version, about = "Blood on the Clocktower role & game management CLI")]
struct Cli {
    /// Path to the SQLite database file
    #[arg(long, default_value = "scrapes.db")]
    db: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch a JSON API endpoint and store BotC editions & roles
    Api {
        /// The API URL to fetch
        url: String,
        /// Bearer token for Authorization header
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
        /// Cookie header value
        #[arg(long, value_name = "COOKIE")]
        cookie: Option<String>,
    },
    /// List stored BotC roles
    Roles {
        /// Filter by team (townsfolk, minion, demon, outsider, traveller)
        #[arg(long)]
        team: Option<String>,
        /// Filter by edition (tb, bmr, snv, carousel)
        #[arg(long)]
        edition: Option<String>,
    },
    /// List stored BotC editions
    Editions,
    /// Create a new game with a list of role IDs (3–20 roles)
    NewGame {
        /// Role IDs to include (repeatable, e.g. --role imp --role spy)
        #[arg(long = "role", value_name = "ROLE_ID", action = clap::ArgAction::Append)]
        roles: Vec<String>,
    },
    /// Retrieve a game by its ID
    GetGame {
        /// Game ID returned by new-game
        id: i64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = storage::open_db(&cli.db)?;
    storage::init_schema(&conn)?;

    match cli.command {
        Commands::Api { url, token, cookie } => run_api(&conn, &url, token.as_deref(), cookie.as_deref())?,
        Commands::Roles { team, edition } => run_roles(&conn, team.as_deref(), edition.as_deref())?,
        Commands::Editions => run_editions(&conn)?,
        Commands::NewGame { roles } => run_new_game(&conn, &roles)?,
        Commands::GetGame { id } => run_get_game(&conn, id)?,
    }
    Ok(())
}

/// Fetch the BotC data API and upsert all editions and roles into the database.
///
/// Sends a GET request with browser-like headers to avoid bot-detection blocks.
/// The `token` argument is sent as a `Bearer` value in the `Authorization`
/// header; the JWT expires every ~30 minutes and must be re-copied from Chrome
/// DevTools when it does. The `cookie` argument carries the Cloudflare
/// `__cf_bm` cookie required by the CDN.
fn run_api(conn: &Connection, url: &str, token: Option<&str>, cookie: Option<&str>) -> Result<()> {
    println!("Fetching {url} ...");
    let client = Client::new();
    let mut req = client
        .get(url)
        .header("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36")
        .header("accept", "*/*")
        .header("referer", "https://botc.app/play");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let body = req
        .send()
        .context("Failed to send request")?
        .error_for_status()
        .context("API returned error status — token may have expired")?
        .text()
        .context("Failed to read response")?;
    let data: BotcData = serde_json::from_str(&body).context("Failed to parse JSON")?;
    let edition_count = storage::upsert_editions(conn, &data.editions)?;
    let role_count = storage::upsert_roles(conn, &data.roles)?;
    println!("Stored {edition_count} editions and {role_count} roles.");
    Ok(())
}

/// Print all stored roles as an aligned table, optionally filtered by team or edition.
fn run_roles(conn: &Connection, team: Option<&str>, edition: Option<&str>) -> Result<()> {
    let roles = storage::get_roles(conn, team, edition)?;
    if roles.is_empty() {
        println!("No roles found.");
        return Ok(());
    }
    println!("{:<20} {:<12} {:<10} {}", "NAME", "TEAM", "EDITION", "ABILITY");
    println!("{}", "-".repeat(100));
    for r in &roles {
        let name = r.name.as_deref().unwrap_or("?");
        let team = r.team.as_deref().unwrap_or("?");
        let edition = r.edition.as_deref().unwrap_or("?");
        let ability = r.ability.as_deref().unwrap_or("");
        // Truncate long ability strings to keep the table readable.
        let ability_short = if ability.len() > 55 {
            format!("{}...", &ability[..52])
        } else {
            ability.to_string()
        };
        println!("{:<20} {:<12} {:<10} {}", name, team, edition, ability_short);
    }
    println!("\n{} role(s) total.", roles.len());
    Ok(())
}

/// Print all stored editions as an aligned table.
fn run_editions(conn: &Connection) -> Result<()> {
    let editions = storage::get_editions(conn)?;
    if editions.is_empty() {
        println!("No editions found. Run `api` first.");
        return Ok(());
    }
    println!("{:<10} {:<25} {:<12} {}", "ID", "NAME", "LEVEL", "OFFICIAL");
    println!("{}", "-".repeat(60));
    for e in &editions {
        println!(
            "{:<10} {:<25} {:<12} {}",
            e.id,
            e.name.as_deref().unwrap_or("?"),
            e.level.as_deref().unwrap_or("?"),
            if e.is_official.unwrap_or(false) { "yes" } else { "no" }
        );
    }
    Ok(())
}

/// Create a new game session from an ordered list of role IDs and print its ID.
///
/// Enforces the BotC player count constraint: 3–20 roles per game.
/// Role IDs are stored as-is and do not need to exist in the `roles` table,
/// but `get-game` will only resolve full details for IDs that do.
fn run_new_game(conn: &Connection, roles: &[String]) -> Result<()> {
    if roles.len() < 3 || roles.len() > 20 {
        bail!("A game requires 3–20 roles, got {}.", roles.len());
    }
    let game_id = create_game(conn, roles)?;
    println!("Game #{game_id} created with {} roles:", roles.len());
    for (i, r) in roles.iter().enumerate() {
        println!("  {}. {}", i + 1, r);
    }
    Ok(())
}

/// Retrieve and display a game session with full role details.
///
/// Role columns (`name`, `team`, `edition`, `ability`) are populated by joining
/// against the `roles` table. If a role ID is not in the database, the raw ID
/// is shown in the NAME column and other fields display `?`.
fn run_get_game(conn: &Connection, id: i64) -> Result<()> {
    match get_game(conn, id)? {
        None => println!("No game found with ID {id}."),
        Some(game) => {
            println!("Game #{} — created {}", game.id, game.created_at);
            println!("{}", "-".repeat(70));
            println!("{:<4} {:<20} {:<12} {:<10} {}", "#", "NAME", "TEAM", "EDITION", "ABILITY");
            println!("{}", "-".repeat(70));
            for (i, r) in game.roles.iter().enumerate() {
                let name = r.name.as_deref().unwrap_or(&r.role_id);
                let team = r.team.as_deref().unwrap_or("?");
                let edition = r.edition.as_deref().unwrap_or("?");
                let ability = r.ability.as_deref().unwrap_or("");
                println!("{:<4} {:<20} {:<12} {:<10} {}", i + 1, name, team, edition, ability);
            }
        }
    }
    Ok(())
}
