//! HTTP fetching and CSS extraction helpers.
//!
//! Two fetch strategies are provided:
//!
//! * [`fetch`] — plain blocking HTTP via `reqwest`. Fast; works on static HTML.
//! * [`fetch_js`] — headless Chrome via `chromiumoxide`. Required for JavaScript
//!   single-page apps (Vue, React, etc.) where content is injected after load.
//!
//! Both functions return the parsed [`scraper::Html`] document alongside the raw
//! HTML string so callers can either extract fields or dump the raw source for
//! debugging.

use anyhow::{Context, Result};
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, SetCookiesParams};
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use scraper::{Html, Selector};

/// Fetch a static HTML page with a plain HTTP GET request.
///
/// Returns the parsed document and the raw response body. Errors if the server
/// returns a non-2xx status code.
pub fn fetch(url: &str) -> Result<(Html, String)> {
    let client = reqwest::blocking::Client::new();
    let body = client
        .get(url)
        .send()
        .context("Failed to send HTTP request")?
        .error_for_status()
        .context("HTTP request returned an error status")?
        .text()
        .context("Failed to read response body as text")?;
    Ok((Html::parse_document(&body), body))
}

/// Fetch a JavaScript-rendered page using headless Chrome.
///
/// Launches a Chrome instance via the Chrome DevTools Protocol, navigates to
/// `url`, waits `wait_ms` milliseconds for JS to mount and render, then
/// captures the live DOM. Increase `wait_ms` for slow-loading SPAs.
///
/// # Authentication
///
/// Two optional auth injection mechanisms are supported. Both are applied
/// before the final navigation so the page loads already authenticated:
///
/// * `cookie` — full `Cookie` header string (e.g. `"session=abc; cf=xyz"`).
///   Parsed on `;` boundaries and injected via the CDP `Network.setCookies`
///   command. Required for server-side session cookies.
///
/// * `local_storage` — list of `"key=value"` strings written into
///   `window.localStorage` after navigating to the site origin. Required for
///   apps that store JWT/Firebase tokens in localStorage instead of cookies.
///
/// When either is provided the function first navigates to `about:blank` (to
/// get a page object), sets cookies, navigates to the site origin (to scope
/// localStorage correctly), writes localStorage entries, then performs the
/// final navigation to `url`.
///
/// # Anti-detection
///
/// Launches Chrome with `--disable-blink-features=AutomationControlled` and
/// `--headless=new` so that `navigator.webdriver` is suppressed and the newer
/// headless engine (which matches headed rendering more closely) is used.
pub fn fetch_js(
    url: &str,
    wait_ms: u64,
    cookie: Option<&str>,
    local_storage: &[String],
) -> Result<(Html, String)> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let (browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .arg("--disable-blink-features=AutomationControlled")
                .arg("--headless=new")
                .build()
                .map_err(|s| anyhow::anyhow!("{s}"))?,
        )
        .await?;

        // The handler must be polled continuously or the browser stalls.
        tokio::spawn(async move { while handler.next().await.is_some() {} });

        let needs_pre_nav = cookie.is_some() || !local_storage.is_empty();
        let page = if needs_pre_nav {
            // Derive the site origin (scheme + host) for cookie domain scoping
            // and for the intermediate localStorage navigation.
            let origin: String = url.split('/').take(3).collect::<Vec<_>>().join("/");

            let page = browser.new_page("about:blank").await?;

            if let Some(cookie_str) = cookie {
                let cookies: Vec<CookieParam> = cookie_str
                    .split(';')
                    .filter_map(|part| {
                        let part = part.trim();
                        part.split_once('=').map(|(name, value)| {
                            let mut c = CookieParam::new(name.trim(), value.trim());
                            // Bind the cookie to the target origin so Chrome
                            // sends it on all requests to that domain.
                            c.url = Some(origin.clone());
                            c
                        })
                    })
                    .collect();
                page.execute(SetCookiesParams { cookies }).await?;
            }

            if !local_storage.is_empty() {
                // localStorage is origin-scoped, so we must navigate to the
                // real origin before writing to it.
                page.goto(&origin).await?;
                for entry in local_storage {
                    if let Some((key, value)) = entry.split_once('=') {
                        let script = format!(
                            "window.localStorage.setItem({}, {})",
                            serde_json::to_string(key)?,
                            serde_json::to_string(value)?
                        );
                        page.evaluate(script).await?;
                    }
                }
            }

            page.goto(url).await?;
            page
        } else {
            browser.new_page(url).await?
        };

        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        let content = page.content().await?;
        Ok((Html::parse_document(&content), content))
    })
}

/// Extract named fields from a parsed HTML document using CSS selectors.
///
/// For each `(css_selector, field_name)` pair, the first matching element's
/// inner text is collected, whitespace-trimmed, and returned as
/// `(field_name, text)`. Entries with empty text are silently dropped.
/// Invalid selectors emit a warning to stderr and are skipped.
pub fn extract(document: &Html, selectors: &[(String, String)]) -> Vec<(String, String)> {
    let mut results = Vec::new();
    for (css, field_name) in selectors {
        let selector = match Selector::parse(css) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: invalid CSS selector '{css}': {e}");
                continue;
            }
        };
        if let Some(element) = document.select(&selector).next() {
            let text = element.text().collect::<Vec<_>>().join(" ");
            let text = text.trim().to_string();
            if !text.is_empty() {
                results.push((field_name.clone(), text));
            }
        }
    }
    results
}
