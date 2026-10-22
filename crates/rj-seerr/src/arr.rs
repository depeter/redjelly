//! Radarr / Sonarr v3 API: interactive release search and grab, the
//! download queue with its real status messages, and blocklist + re-search.
//! Connection details come from Jellyseerr (`Client::servarr_servers`).

use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::models::{MediaKind, ServarrServer};

#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("bad Radarr/Sonarr address: {0}")]
    Url(#[from] url::ParseError),
    #[error("{0}")]
    Http(#[from] reqwest::Error),
    #[error("Radarr/Sonarr returned {status} for {path}{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Status { status: StatusCode, path: String, message: Option<String> },
    #[error("couldn't reach {name} at {tried}")]
    Unreachable { name: String, tried: String },
}

/// What a manual search covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchScope {
    Movie(u32),
    /// Sonarr series id + season (season packs and single episodes).
    Season(u32, u32),
    /// Sonarr episode id.
    Episode(u32),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub guid: String,
    pub indexer_id: u32,
    #[serde(default)]
    pub indexer: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub size: u64,
    /// Days.
    #[serde(default)]
    pub age: u32,
    #[serde(default)]
    pub seeders: Option<u32>,
    #[serde(default)]
    pub leechers: Option<u32>,
    /// "torrent" or "usenet".
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub quality: QualityWrap,
    #[serde(default)]
    pub languages: Vec<Named>,
    #[serde(default)]
    pub custom_format_score: i32,
    #[serde(default)]
    pub rejected: bool,
    /// Strings in Radarr/Sonarr v3, objects with `reason` in newer ones.
    #[serde(default)]
    pub rejections: Vec<serde_json::Value>,
    /// The *arr's own ranking (0 = what it would pick).
    #[serde(default)]
    pub release_weight: Option<i64>,
    #[serde(default)]
    pub full_season: bool,
}

impl Release {
    pub fn quality_name(&self) -> &str {
        &self.quality.quality.name
    }

    pub fn rejection_reasons(&self) -> Vec<String> {
        self.rejections
            .iter()
            .filter_map(|r| match r {
                serde_json::Value::String(s) => Some(s.clone()),
                v => v.get("reason").and_then(|r| r.as_str()).map(str::to_owned),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct QualityWrap {
    #[serde(default)]
    pub quality: Named,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Named {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    /// downloading, queued, paused, completed, delay, warning, failed, …
    #[serde(default)]
    pub status: String,
    /// ok, warning, error.
    #[serde(default)]
    pub tracked_download_status: Option<String>,
    /// downloading, importPending, importBlocked, importing, failedPending, …
    #[serde(default)]
    pub tracked_download_state: Option<String>,
    #[serde(default)]
    pub status_messages: Vec<StatusMessage>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub sizeleft: f64,
    #[serde(default)]
    pub timeleft: Option<String>,
    #[serde(default)]
    pub download_client: Option<String>,
    #[serde(default)]
    pub indexer: Option<String>,
    #[serde(default)]
    pub episode_id: Option<u32>,
    #[serde(default)]
    pub season_number: Option<u32>,
}

impl QueueItem {
    /// Every message the *arr attached, flattened.
    pub fn messages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.error_message.iter().filter(|m| !m.is_empty()).cloned().collect();
        for m in &self.status_messages {
            v.extend(m.messages.iter().filter(|m| !m.is_empty()).cloned());
        }
        v.dedup();
        v
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusMessage {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: u32,
    #[serde(default)]
    pub season_number: u32,
    #[serde(default)]
    pub episode_number: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub has_file: bool,
}

/// A connected Radarr (movies) or Sonarr (shows).
#[derive(Debug, Clone)]
pub struct ArrClient {
    http: reqwest::Client,
    base: Url,
    key: String,
    pub kind: MediaKind,
    pub name: String,
}

impl ArrClient {
    pub fn new(base: &str, key: &str, kind: MediaKind, name: &str) -> Result<Self, ArrError> {
        let mut base = Url::parse(base)?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self { http: reqwest::Client::new(), base: base.join("api/v3/")?, key: key.into(), kind, name: name.into() })
    }

    /// Tries each of the server's addresses and keeps the first that answers.
    pub async fn connect(server: &ServarrServer, kind: MediaKind) -> Result<Self, ArrError> {
        let urls = server.urls();
        for url in &urls {
            let Ok(c) = Self::new(url, &server.api_key, kind, &server.name) else { continue };
            let ping = c.build(Method::GET, "system/status").map(|rb| rb.timeout(std::time::Duration::from_secs(4)));
            match ping {
                Ok(rb) => match Self::send(rb, "system/status").await {
                    Ok(_) => return Ok(c),
                    Err(e) => tracing::info!("{} not reachable at {url}: {e}", server.name),
                },
                Err(e) => tracing::info!("{} address {url}: {e}", server.name),
            }
        }
        Err(ArrError::Unreachable { name: server.name.clone(), tried: urls.join(" or ") })
    }

    fn build(&self, method: Method, path: &str) -> Result<RequestBuilder, ArrError> {
        Ok(self.http.request(method, self.base.join(path)?).header("X-Api-Key", &self.key))
    }

    async fn send(rb: RequestBuilder, path: &str) -> Result<reqwest::Response, ArrError> {
        let resp = rb.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let message = resp.text().await.ok().and_then(|t| {
            let v: serde_json::Value = serde_json::from_str(&t).ok()?;
            let m = v.get("message").or_else(|| v.get(0).and_then(|e| e.get("errorMessage")))?;
            m.as_str().map(str::to_owned)
        });
        Err(ArrError::Status { status, path: path.into(), message })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, ArrError> {
        let rb = self.build(Method::GET, path)?.query(query);
        Ok(Self::send(rb, path).await?.json().await?)
    }

    /// Asks the indexers now (can take a minute), best first.
    pub async fn releases(&self, scope: SearchScope) -> Result<Vec<Release>, ArrError> {
        let q: Vec<(&str, String)> = match scope {
            SearchScope::Movie(id) => vec![("movieId", id.to_string())],
            SearchScope::Season(series, season) => vec![("seriesId", series.to_string()), ("seasonNumber", season.to_string())],
            SearchScope::Episode(id) => vec![("episodeId", id.to_string())],
        };
        let rb = self.build(Method::GET, "release")?.query(&q).timeout(std::time::Duration::from_secs(180));
        let mut releases: Vec<Release> = Self::send(rb, "release").await?.json().await?;
        if releases.iter().all(|r| r.release_weight.is_some()) {
            releases.sort_by_key(|r| r.release_weight);
        }
        // Grabbable ones first, keeping the *arr's order within each group.
        releases.sort_by_key(|r| r.rejected);
        Ok(releases)
    }

    pub async fn grab(&self, release: &Release) -> Result<(), ArrError> {
        let rb = self.build(Method::POST, "release")?.json(&json!({ "guid": release.guid, "indexerId": release.indexer_id }));
        Self::send(rb, "release").await.map(drop)
    }

    /// Queue entries for a movie (Radarr) or series (Sonarr).
    pub async fn queue(&self, id: u32) -> Result<Vec<QueueItem>, ArrError> {
        let key = match self.kind {
            MediaKind::Movie => "movieId",
            MediaKind::Tv => "seriesId",
        };
        self.get("queue/details", &[(key, id.to_string())]).await
    }

    /// Removes a download from the client and blocklists the release.
    pub async fn remove_and_blocklist(&self, queue_id: u64) -> Result<(), ArrError> {
        let path = format!("queue/{queue_id}");
        let rb = self.build(Method::DELETE, &path)?.query(&[("removeFromClient", "true"), ("blocklist", "true")]);
        Self::send(rb, &path).await.map(drop)
    }

    /// Automatic search for the whole movie / series.
    pub async fn search(&self, id: u32) -> Result<(), ArrError> {
        let body = match self.kind {
            MediaKind::Movie => json!({ "name": "MoviesSearch", "movieIds": [id] }),
            MediaKind::Tv => json!({ "name": "SeriesSearch", "seriesId": id }),
        };
        Self::send(self.build(Method::POST, "command")?.json(&body), "command").await.map(drop)
    }

    /// Sonarr: all episodes of a series.
    pub async fn episodes(&self, series_id: u32) -> Result<Vec<Episode>, ArrError> {
        self.get("episode", &[("seriesId", series_id.to_string())]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server(urls: (&str, Option<&str>)) -> ServarrServer {
        ServarrServer {
            id: 0,
            name: "Radarr".into(),
            hostname: urls.0.into(),
            port: 7878,
            api_key: "k".into(),
            use_ssl: false,
            base_url: Some("/radarr".into()),
            external_url: urls.1.map(Into::into),
        }
    }

    #[test]
    fn server_urls() {
        assert_eq!(
            server(("radarr", Some("https://radarr.example.com/"))).urls(),
            ["http://radarr:7878/radarr", "https://radarr.example.com"]
        );
        assert_eq!(server(("http://10.0.0.5", None)).urls(), ["http://10.0.0.5:7878/radarr"]);
    }

    #[tokio::test]
    async fn connect_falls_back_and_searches() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "5.0" })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/release"))
            .and(query_param("movieId", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "guid": "b", "indexerId": 1, "title": "Rejected.2160p", "rejected": true, "releaseWeight": 0,
                  "rejections": [{ "reason": "Not an upgrade", "type": "permanent" }], "quality": { "quality": { "name": "Bluray-2160p" } } },
                { "guid": "c", "indexerId": 1, "title": "Second", "releaseWeight": 2, "size": 4000000000u64 },
                { "guid": "a", "indexerId": 2, "title": "Best.1080p", "releaseWeight": 1, "seeders": 50,
                  "rejections": ["Unknown"], "quality": { "quality": { "name": "WEBDL-1080p" } }, "languages": [{ "name": "English" }] }
            ])))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/release"))
            .and(body_json(json!({ "guid": "a", "indexerId": 2 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&mock)
            .await;

        // The internal hostname doesn't resolve; the external URL works.
        let s = ServarrServer { external_url: Some(mock.uri()), hostname: "no-such-host.invalid".into(), base_url: None, ..server(("", None)) };
        let c = ArrClient::connect(&s, MediaKind::Movie).await.unwrap();
        let r = c.releases(SearchScope::Movie(7)).await.unwrap();
        let titles: Vec<&str> = r.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Best.1080p", "Second", "Rejected.2160p"]);
        assert_eq!(r[0].quality_name(), "WEBDL-1080p");
        assert_eq!(r[0].rejection_reasons(), ["Unknown"]);
        assert_eq!(r[2].rejection_reasons(), ["Not an upgrade"]);
        c.grab(&r[0]).await.unwrap();
    }

    #[tokio::test]
    async fn queue_and_blocklist() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/queue/details"))
            .and(query_param("seriesId", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": 99, "title": "Show.S01", "status": "completed", "trackedDownloadStatus": "warning",
                "trackedDownloadState": "importBlocked", "size": 10.0, "sizeleft": 0.0,
                "statusMessages": [{ "title": "Show.S01", "messages": ["No files found are eligible for import"] }]
            }])))
            .mount(&mock)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/queue/99"))
            .and(query_param("blocklist", "true"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/command"))
            .and(body_json(json!({ "name": "SeriesSearch", "seriesId": 3 })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .mount(&mock)
            .await;
        let c = ArrClient::new(&mock.uri(), "k", MediaKind::Tv, "Sonarr").unwrap();
        let q = c.queue(3).await.unwrap();
        assert_eq!(q[0].tracked_download_state.as_deref(), Some("importBlocked"));
        assert_eq!(q[0].messages(), ["No files found are eligible for import"]);
        c.remove_and_blocklist(99).await.unwrap();
        c.search(3).await.unwrap();
    }
}
