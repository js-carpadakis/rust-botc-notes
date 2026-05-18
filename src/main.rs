//! `rustscraper` — general-purpose static/JS HTML web scraper CLI.
//!
//! # Subcommands
//!
//! | Command  | Description |
//! |----------|-------------|
//! | `scrape` | Fetch a URL and extract fields by CSS selector |
//! | `list`   | Print stored scrape results, optionally filtered by URL |
//! | `export` | Serialize all results to JSON (file or stdout) |
//!
//! # Quick start
//!
//! ```text
//! # Static page
//! rustscraper scrape https://example.com --select "h1:title"
//!
//! # JavaScript SPA with auth cookies
//! rustscraper scrape https://botc.app/play --js --cookie "session=abc" --select ".player:name"
//!
//! # Dump rendered HTML for selector discovery
//! rustscraper scrape https://botc.app/play --js --dump-html | Out-File -Encoding utf8 rendered.html
//! ```

mod models;
mod scraper;
mod storage;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use models::{Field, ScrapeRecord};
use rusqlite::Connection;

/// Top-level CLI configuration parsed by clap.
#[derive(Parser)]
#[command(name = "rustscraper", version, about = "A static HTML web scraping CLI tool")]
struct Cli {
    /// Path to the SQLite database file
    #[arg(long, default_value = "scrapes.db")]
    db: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch a URL and extract fields by CSS selector
    Scrape {
        /// The URL to fetch
        url: String,
        /// CSS selector and field name, e.g. "h1:title". Repeatable.
        #[arg(long = "select", value_name = "SELECTOR:FIELD", action = clap::ArgAction::Append)]
        select: Vec<String>,
        /// Print the raw HTML received instead of extracting (useful for debugging selectors)
        #[arg(long)]
        dump_html: bool,
        /// Use headless Chrome to render JavaScript before extracting (requires Chrome installed)
        #[arg(long)]
        js: bool,
        /// Milliseconds to wait after page load before capturing the DOM (default: 3000, only used with --js)
        #[arg(long, default_value = "3000", value_name = "MS")]
        wait_ms: u64,
        /// Cookie header value to inject for authenticated scraping, e.g. "session=abc; cf=xyz" (only used with --js)
        #[arg(long, value_name = "COOKIE")]
        cookie: Option<String>,
        /// localStorage key=value to inject before navigation, e.g. "authToken=eyJ..." (repeatable, only used with --js)
        #[arg(long = "local-storage", value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        local_storage: Vec<String>,
    },
    /// List stored scrape results
    List {
        /// Filter by exact URL
        #[arg(long)]
        url: Option<String>,
    },
    /// Export all results to JSON
    Export {
        /// Output file path (defaults to stdout)
        #[arg(long, value_name = "FILE")]
        output: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = storage::open_db(&cli.db)?;
    storage::init_schema(&conn)?;

    match cli.command {
        Commands::Scrape { url, select, dump_html, js, wait_ms, cookie, local_storage } => {
            run_scrape(&conn, &url, &select, dump_html, js, wait_ms, cookie.as_deref(), &local_storage)?
        }
        Commands::List { url } => run_list(&conn, url.as_deref())?,
        Commands::Export { output } => run_export(&conn, output.as_deref())?,
    }
    Ok(())
}

/// Parse `--select SELECTOR:FIELD` arguments into `(css, field_name)` pairs.
///
/// Splits on the **last** `:` so that CSS pseudo-selectors such as
/// `a:first-child` are preserved in the selector portion.
fn parse_selectors(select_args: &[String]) -> Result<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for arg in select_args {
        match arg.rsplit_once(':') {
            Some((css, field)) if !css.is_empty() && !field.is_empty() => {
                pairs.push((css.to_string(), field.to_string()));
            }
            _ => bail!("Invalid --select value '{arg}'. Expected format: SELECTOR:FIELD"),
        }
    }
    Ok(pairs)
}

/// Fetch `url`, extract CSS-selected fields, store the result, and print a summary.
///
/// Uses headless Chrome when `js` is `true`; falls back to a plain HTTP GET
/// otherwise. When `dump_html` is set, the raw (or rendered) HTML is printed
/// to stdout and no database write is performed.
fn run_scrape(
    conn: &Connection,
    url: &str,
    select: &[String],
    dump_html: bool,
    js: bool,
    wait_ms: u64,
    cookie: Option<&str>,
    local_storage: &[String],
) -> Result<()> {
    println!("Fetching {url} ...");
    let (document, raw_html) = if js {
        scraper::fetch_js(url, wait_ms, cookie, local_storage)?
    } else {
        scraper::fetch(url)?
    };

    if dump_html {
        println!("{raw_html}");
        return Ok(());
    }

    if select.is_empty() {
        bail!("Provide at least one --select SELECTOR:FIELD argument");
    }
    let selector_pairs = parse_selectors(select)?;
    let extracted = scraper::extract(&document, &selector_pairs);
    let record = ScrapeRecord {
        id: 0,
        url: url.to_string(),
        scraped_at: chrono::Utc::now().to_rfc3339(),
        fields: extracted
            .into_iter()
            .map(|(name, value)| Field { name, value })
            .collect(),
    };
    let id = storage::insert_scrape(conn, &record)?;
    println!(
        "Stored record #{id} with {} field(s):",
        record.fields.len()
    );
    for f in &record.fields {
        println!("  {}: {}", f.name, f.value);
    }
    Ok(())
}

/// Print all stored scrape records as an aligned table.
///
/// Pass `url_filter` to restrict output to records from a specific URL.
fn run_list(conn: &Connection, url_filter: Option<&str>) -> Result<()> {
    let records = match url_filter {
        Some(url) => storage::get_scrapes_by_url(conn, url)?,
        None => storage::get_all_scrapes(conn)?,
    };

    if records.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    println!(
        "{:<6} {:<50} {:<30} {}",
        "ID", "URL", "SCRAPED AT", "FIELDS"
    );
    println!("{}", "-".repeat(110));
    for r in &records {
        let url_display = if r.url.len() > 48 {
            format!("{}...", &r.url[..45])
        } else {
            r.url.clone()
        };
        let fields_display = r
            .fields
            .iter()
            .map(|f| format!("{}={}", f.name, f.value))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{:<6} {:<50} {:<30} {}",
            r.id, url_display, r.scraped_at, fields_display
        );
    }
    Ok(())
}

/// Serialize all stored scrape records to pretty-printed JSON.
///
/// Writes to `output` if provided, otherwise to stdout. The record count is
/// always printed to stderr so it doesn't pollute piped JSON output.
fn run_export(conn: &Connection, output: Option<&str>) -> Result<()> {
    let records = storage::get_all_scrapes(conn)?;
    match output {
        Some(path) => {
            let file = std::fs::File::create(path)?;
            serde_json::to_writer_pretty(file, &records)?;
            eprintln!("Exported {} record(s) to {path}.", records.len());
        }
        None => {
            serde_json::to_writer_pretty(std::io::stdout(), &records)?;
            println!();
            eprintln!("Exported {} record(s).", records.len());
        }
    }
    Ok(())
}
