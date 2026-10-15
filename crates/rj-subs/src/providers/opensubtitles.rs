//! OpenSubtitles.com REST API v1.
//!
//! Needs an API key (consumer key) for search and a logged-in user token for
//! downloads. Free accounts get a small daily download quota; the remaining
//! count is returned by every download and surfaced in the UI.

use std::sync::Mutex;

use rj_core::lang::Lang;
use serde::Deserialize;
use serde_json::json;

use crate::encoding;
use crate::provider::{Candidate, Error, Provider, ProviderId, Query, SubtitleFile};

const NAME: &str = "opensubtitles";
const DEFAULT_HOST: &str = "api.opensubtitles.com";

pub struct OpenSubtitles {
    http: reqwest::Client,
    api_key: String,
    user_agent: String,
    session: Mutex<Option<Session>>,
    /// Username/password, used to log in lazily on the first download.
    credentials: Option<(String, String)>,
    /// Last known remaining downloads for today.
    pub remaining: Mutex<Option<i64>>,
}

#[derive(Clone)]
struct Session {
    token: String,
    host: String,
}

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
    base_url: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    attributes: Attributes,
}

#[derive(Deserialize)]
struct Attributes {
    language: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    hearing_impaired: bool,
    #[serde(default)]
    foreign_parts_only: bool,
    #[serde(default)]
    download_count: u32,
    #[serde(default)]
    ratings: Option<f32>,
    #[serde(default)]
    moviehash_match: bool,
    #[serde(default)]
    files: Vec<OsFile>,
}

#[derive(Deserialize)]
struct OsFile {
    file_id: u64,
    #[serde(default)]
    file_name: Option<String>,
}

#[derive(Deserialize)]
struct DownloadResponse {
    link: String,
    #[serde(default)]
    remaining: Option<i64>,
}

impl OpenSubtitles {
    pub fn new(http: reqwest::Client, api_key: String, app_version: &str, credentials: Option<(String, String)>) -> Self {
        Self {
            http,
            api_key,
            user_agent: format!("redjelly v{app_version}"),
            session: Mutex::new(None),
            credentials,
            remaining: Mutex::new(None),
        }
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<(), Error> {
        let resp = self
            .http
            .post(format!("https://{DEFAULT_HOST}/api/v1/login"))
            .header("Api-Key", &self.api_key)
            .header("User-Agent", &self.user_agent)
            .json(&json!({ "username": username, "password": password }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Auth(NAME));
        }
        let login: LoginResponse = resp.error_for_status()?.json().await?;
        *self.session.lock().unwrap() = Some(Session {
            token: login.token,
            host: login.base_url.unwrap_or_else(|| DEFAULT_HOST.into()),
        });
        Ok(())
    }

    fn host(&self) -> String {
        self.session.lock().unwrap().as_ref().map_or(DEFAULT_HOST.into(), |s| s.host.clone())
    }
}

/// OpenSubtitles wants numeric IMDb ids without the `tt` prefix.
fn imdb_number(id: &str) -> String {
    id.trim_start_matches("tt").trim_start_matches('0').to_owned()
}

/// Builds the search query. The API asks for parameters sorted by name and
/// lowercase values to maximise cache hits.
fn search_params(q: &Query) -> Vec<(&'static str, String)> {
    let mut p = Vec::new();
    let mut langs: Vec<_> = q.languages.iter().map(|l| l.as_str().to_owned()).collect();
    langs.sort();
    if let Some(e) = q.episode {
        p.push(("episode_number", e.to_string()));
    }
    if let Some(id) = &q.imdb_id {
        p.push(("imdb_id", imdb_number(id)));
    }
    p.push(("languages", langs.join(",")));
    if let Some(h) = &q.movie_hash {
        p.push(("moviehash", h.clone()));
    }
    if let Some(id) = &q.parent_imdb_id {
        p.push(("parent_imdb_id", imdb_number(id)));
    }
    if q.imdb_id.is_none() && q.parent_imdb_id.is_none() {
        p.push(("query", q.title.to_lowercase()));
    }
    if let Some(s) = q.season {
        p.push(("season_number", s.to_string()));
    }
    if let Some(id) = &q.tmdb_id {
        p.push(("tmdb_id", id.clone()));
    }
    p
}

#[async_trait::async_trait]
impl Provider for OpenSubtitles {
    fn id(&self) -> ProviderId {
        ProviderId::OpenSubtitles
    }

    async fn search(&self, q: &Query) -> Result<Vec<Candidate>, Error> {
        let resp: SearchResponse = self
            .http
            .get(format!("https://{}/api/v1/subtitles", self.host()))
            .header("Api-Key", &self.api_key)
            .header("User-Agent", &self.user_agent)
            .query(&search_params(q))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp
            .data
            .into_iter()
            .filter_map(|item| {
                let a = item.attributes;
                let file = a.files.into_iter().next()?;
                Some(Candidate {
                    provider: ProviderId::OpenSubtitles,
                    id: file.file_id.to_string(),
                    lang: Lang::parse(a.language.as_deref()?)?,
                    release: a.release.or(file.file_name).unwrap_or_default(),
                    format: None,
                    hash_match: a.moviehash_match,
                    hearing_impaired: a.hearing_impaired,
                    forced: a.foreign_parts_only,
                    downloads: a.download_count,
                    rating: a.ratings,
                })
            })
            .collect())
    }

    async fn download(&self, _q: &Query, c: &Candidate) -> Result<Option<SubtitleFile>, Error> {
        let logged_in = self.session.lock().unwrap().is_some();
        if !logged_in {
            let (user, pass) = self.credentials.as_ref().ok_or(Error::Auth(NAME))?;
            self.login(user, pass).await?;
        }
        let session = self.session.lock().unwrap().clone().ok_or(Error::Auth(NAME))?;
        let resp = self
            .http
            .post(format!("https://{}/api/v1/download", session.host))
            .header("Api-Key", &self.api_key)
            .header("User-Agent", &self.user_agent)
            .bearer_auth(&session.token)
            .json(&json!({ "file_id": c.id.parse::<u64>().unwrap_or_default() }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_ACCEPTABLE {
            return Err(Error::Quota(NAME));
        }
        let dl: DownloadResponse = resp.error_for_status()?.json().await?;
        *self.remaining.lock().unwrap() = dl.remaining;

        let bytes = self.http.get(&dl.link).send().await?.error_for_status()?.bytes().await?;
        let text = encoding::to_utf8(&bytes);
        let format = encoding::detect_format(&text)
            .ok_or_else(|| Error::Protocol(NAME, "downloaded file is not a subtitle".into()))?;
        Ok(Some(SubtitleFile {
            lang: c.lang.clone(),
            format: format.into(),
            text,
            hearing_impaired: c.hearing_impaired,
            forced: c.forced,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_sorted_and_normalised() {
        let q = Query {
            languages: vec![Lang::parse("nl").unwrap(), Lang::parse("en").unwrap()],
            parent_imdb_id: Some("tt0903747".into()),
            season: Some(1),
            episode: Some(2),
            movie_hash: Some("abc".into()),
            ..Default::default()
        };
        let p = search_params(&q);
        let names: Vec<_> = p.iter().map(|(k, _)| *k).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(p.contains(&("languages", "en,nl".into())));
        assert!(p.contains(&("parent_imdb_id", "903747".into())));
    }
}
