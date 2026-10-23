use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::epg::Guide;
use crate::models::{Page, Section, Tile, tiles};

pub const SITE: &str = "https://www.vrt.be";
const GRAPHQL: &str = "https://www.vrt.be/vrtnu-api/graphql/v1";
const CLIENT_VERSION: &str = "1.5.15";

const TILE: &str = include_str!("queries/tile.graphql");
const PAGE: &str = include_str!("queries/page.graphql");
const LIST: &str = include_str!("queries/list.graphql");
const EPG: &str = include_str!("queries/epg.graphql");
const FAVORITE: &str = include_str!("queries/favorite.graphql");

/// The account lists, as VRT's site (and the Kodi add-on) name them.
const MY_LIST: &str = "o%25|o%9||p%/mijn-lijst/%|video-program|b%1%";
const CONTINUE: &str = "o%35|p%/|list_1744022689|b%0%";

pub const PAGE_SIZE: u32 = 24;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("not signed in to VRT MAX")]
    LoginRequired,
    /// VRT only streams to Belgian IP addresses.
    #[error("VRT MAX is only available in Belgium")]
    GeoBlocked,
    #[error("VRT returned {0}")]
    Status(StatusCode),
    #[error("VRT: {0}")]
    Api(String),
}

/// Cheap to clone.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
    /// `vrtnu-site_profile_at` cookie: the account's access token.
    token: Option<String>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) redjelly")
            .build()
            .unwrap_or_default();
        Self { http, token: None }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    async fn gql(&self, query: &str, variables: Value) -> Result<Value, Error> {
        let mut rb = self
            .http
            .post(GRAPHQL)
            .header("x-vrt-client-name", "WEB")
            .header("x-vrt-client-version", CLIENT_VERSION)
            .json(&json!({ "query": format!("{query}\n{TILE}"), "variables": variables }));
        if let Some(t) = &self.token {
            rb = rb.bearer_auth(t);
        }
        let resp = rb.send().await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(Error::LoginRequired);
        }
        let mut body: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) if !status.is_success() => return Err(Error::Status(status)),
            Err(e) => return Err(e.into()),
        };
        if let Some(e) = body["errors"].as_array().and_then(|e| e.first()) {
            let msg = e["message"].as_str().unwrap_or("error").to_owned();
            let code = e["extensions"]["code"].as_str().unwrap_or_default();
            if code == "UNAUTHENTICATED" || code == "FORBIDDEN" {
                return Err(Error::LoginRequired);
            }
            if body["data"].is_null() {
                return Err(Error::Api(msg));
            }
            tracing::debug!("VRT GraphQL partial error: {msg}");
        }
        if !status.is_success() {
            return Err(Error::Status(status));
        }
        Ok(body["data"].take())
    }

    /// Any VRT MAX page by its site path (`/vrtmax/`, `/vrtmax/a-z/thuis/`).
    pub async fn page(&self, path: &str) -> Result<Option<Page>, Error> {
        let data = self.gql(PAGE, json!({ "pageId": path, "first": PAGE_SIZE })).await?;
        Ok(Page::parse(&data["page"]))
    }

    /// Search results, one section per kind ("Kijk", "Video-afleveringen").
    pub async fn search(&self, term: &str) -> Result<Vec<Section>, Error> {
        let path = format!("/vrtmax/zoeken/?q={}", encode(term.trim()));
        Ok(self.page(&path).await?.map(|p| p.without_summary_tab().sections).unwrap_or_default())
    }

    /// One page of a list; `after` is the previous page's cursor. Returns
    /// the tiles and the next cursor (`None` at the end).
    pub async fn list(&self, list_id: &str, after: Option<&str>) -> Result<(Vec<Tile>, Option<String>), Error> {
        let data = self.gql(LIST, json!({ "listId": list_id, "first": PAGE_SIZE, "after": after })).await?;
        if data["list"].is_null() {
            return Ok((vec![], None));
        }
        Ok(tiles(&data["list"]))
    }

    pub async fn my_list(&self) -> Result<Vec<Tile>, Error> {
        self.account_list(MY_LIST).await
    }

    pub async fn continue_watching(&self) -> Result<Vec<Tile>, Error> {
        self.account_list(CONTINUE).await
    }

    async fn account_list(&self, id: &str) -> Result<Vec<Tile>, Error> {
        if self.token.is_none() {
            return Err(Error::LoginRequired);
        }
        Ok(self.list(&b64_list_id(id), None).await?.0)
    }

    /// Adds a programme to, or removes it from, Mijn lijst. `id` is the
    /// page's favourite action id ([`Page::favorite`]).
    pub async fn set_favorite(&self, id: &str, on: bool) -> Result<(), Error> {
        if self.token.is_none() {
            return Err(Error::LoginRequired);
        }
        self.gql(FAVORITE, json!({ "input": { "id": id, "favorite": on } })).await?;
        Ok(())
    }

    /// `date` is `YYYY-MM-DD` (Belgian time).
    pub async fn guide(&self, slug: &str, date: &str, now: i64) -> Result<Guide, Error> {
        let path = format!("/vrtmax/tv-gids/{slug}/{date}/");
        let data = self.gql(EPG, json!({ "pageId": path, "first": 100 })).await?;
        Ok(Guide::parse(&data["page"], now))
    }
}

/// Expiry (Unix seconds) of a JWT such as the `vrtnu-site_profile_at`
/// cookie.
pub fn token_expiry(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Value>(&raw).ok()?["exp"].as_f64().map(|e| e as i64)
}

/// List ids are `$` + base64 of VRT's internal list name.
fn b64_list_id(name: &str) -> String {
    use base64::Engine;
    format!("${}", base64::engine::general_purpose::STANDARD.encode(name))
}

/// Percent-encodes a search term for the page path.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(encode("de  slimste mens?"), "de++slimste+mens%3F");
        assert_eq!(encode("café"), "caf%C3%A9");
        assert!(b64_list_id(CONTINUE).starts_with("$byUzNXxwJS98"));
        use base64::Engine;
        let b = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        assert_eq!(token_expiry(&format!("{}.{}.sig", b("{}"), b(r#"{"exp":1792780000}"#))), Some(1_792_780_000));
        assert_eq!(token_expiry("garbage"), None);
    }

    // Live against vrt.be: `cargo test -p rj-vrt -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_catalogue() {
        let c = Client::new();
        let home = c.page("/vrtmax/").await.unwrap().unwrap();
        assert!(!home.sections.is_empty());
        let s = home.sections.iter().find(|s| s.cursor.is_some()).unwrap();
        let (more, _) = c.list(s.list_id.as_deref().unwrap(), s.cursor.as_deref()).await.unwrap();
        assert!(!more.is_empty());
        assert!(!c.search("thuis").await.unwrap().is_empty());
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let g = c.guide("vrt1", &today, chrono::Utc::now().timestamp()).await.unwrap();
        assert!(g.current().is_some());
        assert!(matches!(c.my_list().await, Err(Error::LoginRequired)));
    }
}
