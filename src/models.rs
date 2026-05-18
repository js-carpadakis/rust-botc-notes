//! Shared data models used across the scraper, BotC API, and storage layers.
//!
//! Structs in this module are serializable so they can be written to JSON via
//! `serde_json` and stored/retrieved from SQLite via `rusqlite`.

use serde::{Deserialize, Serialize};

/// A single named field extracted from a scraped HTML page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// The user-supplied field name from the `--select SELECTOR:FIELD` argument.
    pub name: String,
    /// The trimmed inner text of the first matched element.
    pub value: String,
}

/// One complete scrape result: the URL, when it was fetched, and all extracted fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeRecord {
    /// Auto-assigned database row ID (0 before insertion).
    pub id: i64,
    /// The URL that was fetched.
    pub url: String,
    /// RFC 3339 timestamp of when the scrape was performed.
    pub scraped_at: String,
    /// All fields extracted from the page by CSS selector.
    pub fields: Vec<Field>,
}

// ---------------------------------------------------------------------------
// BotC API types
// ---------------------------------------------------------------------------

/// Top-level response from the BotC data API (`/backend/data`).
///
/// The API returns all editions and roles in a single payload, which is then
/// upserted into the local SQLite database.
#[derive(Debug, Deserialize)]
pub struct BotcData {
    pub editions: Vec<Edition>,
    pub roles: Vec<Role>,
}

/// A Blood on the Clocktower edition (e.g. Trouble Brewing, Bad Moon Rising).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edition {
    /// Short identifier used as the primary key (e.g. `"tb"`, `"bmr"`, `"snv"`).
    pub id: String,
    pub name: Option<String>,
    /// Hex colour string used by the app UI for this edition.
    pub color: Option<String>,
    /// Difficulty tier (e.g. `"beginner"`, `"intermediate"`).
    pub level: Option<String>,
    /// Whether this is an officially published edition (as opposed to a custom script).
    #[serde(rename = "isOfficial")]
    pub is_official: Option<bool>,
}

/// A character role as it appears within a stored [`GameInstance`].
///
/// Role details are resolved by joining against the `roles` table at query
/// time, so only `role_id` is stored in `game_roles`; the rest may be `None`
/// if the role is not present in the local database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRole {
    /// The canonical role identifier (e.g. `"imp"`, `"washerwoman"`).
    pub role_id: String,
    pub name: Option<String>,
    pub team: Option<String>,
    pub edition: Option<String>,
    pub ability: Option<String>,
}

/// A stored game session: a creation timestamp and the ordered list of roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameInstance {
    /// Auto-assigned database ID returned by `new-game`.
    pub id: i64,
    /// RFC 3339 timestamp of when the game was created.
    pub created_at: String,
    /// Roles in the order they were supplied to `new-game`.
    pub roles: Vec<GameRole>,
}

/// A Blood on the Clocktower character role with its full metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    /// Canonical identifier used as the primary key (e.g. `"imp"`).
    pub id: String,
    pub name: Option<String>,
    /// Alignment team: `townsfolk`, `minion`, `demon`, `outsider`, or `traveller`.
    pub team: Option<String>,
    /// Edition this role belongs to (e.g. `"tb"`).
    pub edition: Option<String>,
    /// The role's in-game ability text.
    pub ability: Option<String>,
    /// Flavour/lore text shown on the physical token.
    pub flavor: Option<String>,
    /// Whether this role modifies setup (affects how many of each type are in play).
    pub setup: Option<bool>,
    /// Storyteller reminder for the role's first-night action.
    #[serde(rename = "firstNightReminder")]
    pub first_night_reminder: Option<String>,
    /// Storyteller reminder for subsequent-night actions.
    #[serde(rename = "otherNightReminder")]
    pub other_night_reminder: Option<String>,
}
