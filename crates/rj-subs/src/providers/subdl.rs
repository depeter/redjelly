//! Subdl (subdl.com). Free API key from a subdl.com account.
//!
//! Search: `GET https://api.subdl.com/api/v1/subtitles`. Downloads are ZIP
//! files (sometimes whole-season packs) from `https://dl.subdl.com`.

use std::io::Read;

use rj_core::lang::Lang;
use serde::Deserialize;

use crate::encoding;
use crate::provider::{Candidate, Error, Provider, ProviderId, Query, SubtitleFile};

const NAME: &str = "subdl";

pub struct Subdl {
    http: reqwest::Client,
    api_key: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    status: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    subtitles: Vec<SubdlSubtitle>,
}

#[derive(Deserialize)]
struct SubdlSubtitle {
    #[serde(default)]
    release_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Two-letter code, upper case ("NL").
    #[serde(default)]
    language: Option<String>,
    url: String,
    #[serde(default)]
    hi: bool,
}

impl Subdl {
    pub fn new(http: reqwest::Client, api_key: String) -> Self {
        Self { http, api_key }
    }
}

fn search_params(q: &Query, api_key: &str) -> Vec<(&'static str, String)> {
    let mut p = vec![("api_key", api_key.to_owned()), ("subs_per_page", "30".into())];
    let langs: Vec<String> = q.languages.iter().map(|l| l.as_str().to_uppercase()).collect();
    p.push(("languages", langs.join(",")));
    let episode = q.season.is_some() && q.episode.is_some();
    p.push(("type", if episode { "tv" } else { "movie" }.into()));
    match (episode, &q.parent_imdb_id, &q.imdb_id) {
        (true, Some(id), _) => p.push(("imdb_id", id.clone())),
        (false, _, Some(id)) => p.push(("imdb_id", id.clone())),
        _ => match (episode, &q.parent_tmdb_id, &q.tmdb_id) {
            (true, Some(id), _) | (false, _, Some(id)) => p.push(("tmdb_id", id.clone())),
            _ => p.push(("film_name", q.title.clone())),
        },
    }
    if let (Some(s), Some(e)) = (q.season, q.episode) {
        p.push(("season_number", s.to_string()));
        p.push(("episode_number", e.to_string()));
    }
    if let Some(f) = &q.file_name {
        p.push(("file_name", f.clone()));
    }
    p
}

/// Picks the subtitle file from a ZIP: for season packs the one matching
/// SxxEyy, else the first subtitle file; SRT preferred.
fn pick_from_zip(bytes: &[u8], season: Option<u32>, episode: Option<u32>) -> Result<(String, Vec<u8>), Error> {
    let bad = |e: zip::result::ZipError| Error::Protocol(NAME, format!("bad zip: {e}"));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(bad)?;
    let names: Vec<String> = archive.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let is_sub = |n: &str| {
        let n = n.to_ascii_lowercase();
        [".srt", ".ass", ".ssa", ".vtt"].iter().any(|ext| n.ends_with(ext))
    };
    let tag = season.zip(episode).map(|(s, e)| format!("s{s:02}e{e:02}"));
    let mut candidates: Vec<&String> = names.iter().filter(|n| is_sub(n)).collect();
    if let Some(tag) = &tag {
        let matching: Vec<&String> =
            candidates.iter().copied().filter(|n| n.to_ascii_lowercase().replace(['.', ' ', '_'], "").contains(tag.as_str())).collect();
        if !matching.is_empty() {
            candidates = matching;
        }
    }
    candidates.sort_by_key(|n| !n.to_ascii_lowercase().ends_with(".srt"));
    let name = candidates.first().ok_or_else(|| Error::Protocol(NAME, "no subtitle in zip".into()))?.to_string();
    let mut file = archive.by_name(&name).map_err(bad)?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).map_err(|e| Error::Protocol(NAME, e.to_string()))?;
    Ok((name, out))
}

#[async_trait::async_trait]
impl Provider for Subdl {
    fn id(&self) -> ProviderId {
        ProviderId::Subdl
    }

    async fn search(&self, q: &Query) -> Result<Vec<Candidate>, Error> {
        let resp: SearchResponse = self
            .http
            .get("https://api.subdl.com/api/v1/subtitles")
            .query(&search_params(q, &self.api_key))
            .send()
            .await?
            .json()
            .await?;
        if !resp.status {
            let msg = resp.error.unwrap_or_default();
            // "can't find movie or tv" is just an empty result.
            if msg.to_ascii_lowercase().contains("can't find") || msg.to_ascii_lowercase().contains("not found") {
                return Ok(Vec::new());
            }
            if msg.to_ascii_lowercase().contains("api") {
                return Err(Error::Auth(NAME));
            }
            return Err(Error::Protocol(NAME, msg));
        }
        Ok(resp
            .subtitles
            .into_iter()
            .filter_map(|s| {
                Some(Candidate {
                    provider: ProviderId::Subdl,
                    lang: Lang::parse(s.language.as_deref()?)?,
                    release: s.release_name.or(s.name).unwrap_or_default(),
                    format: None,
                    hash_match: false,
                    hearing_impaired: s.hi,
                    forced: false,
                    downloads: 0,
                    rating: None,
                    id: s.url,
                })
            })
            .collect())
    }

    async fn download(&self, q: &Query, c: &Candidate) -> Result<Option<SubtitleFile>, Error> {
        let url = format!("https://dl.subdl.com{}", c.id);
        let bytes = self.http.get(&url).send().await?.error_for_status()?.bytes().await?;
        let (_name, raw) = pick_from_zip(&bytes, q.season, q.episode)?;
        let text = encoding::to_utf8(&raw);
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
    use std::io::Write;

    use super::*;

    fn zip_with(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        for (name, body) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn picks_episode_from_season_pack() {
        let z = zip_with(&[
            ("readme.txt", "x"),
            ("Show.S01E01.srt", "one"),
            ("Show.S01E02.WEB.srt", "two"),
        ]);
        let (name, body) = pick_from_zip(&z, Some(1), Some(2)).unwrap();
        assert_eq!(name, "Show.S01E02.WEB.srt");
        assert_eq!(body, b"two");
    }

    #[test]
    fn prefers_srt_for_movies() {
        let z = zip_with(&[("Film.ass", "a"), ("Film.srt", "s")]);
        assert_eq!(pick_from_zip(&z, None, None).unwrap().0, "Film.srt");
    }

    #[test]
    fn parses_search_response() {
        let json = r#"{"status":true,"results":[{"name":"X"}],"subtitles":[
            {"release_name":"Film.2023.1080p.WEB","name":"SUBDL::x.zip","lang":"dutch","language":"NL","url":"/subtitle/1-2.zip","hi":false},
            {"release_name":"Bad","language":"","url":"/subtitle/3.zip"}]}"#;
        let r: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.subtitles.len(), 2);
        assert_eq!(r.subtitles[0].url, "/subtitle/1-2.zip");
    }

    #[test]
    fn episode_query_uses_series_id() {
        let q = Query {
            languages: vec![Lang::parse("nl").unwrap()],
            parent_imdb_id: Some("tt0903747".into()),
            imdb_id: Some("tt1".into()),
            season: Some(1),
            episode: Some(2),
            ..Default::default()
        };
        let p = search_params(&q, "k");
        assert!(p.contains(&("imdb_id", "tt0903747".into())));
        assert!(p.contains(&("type", "tv".into())));
        assert!(p.contains(&("languages", "NL".into())));
    }
}
