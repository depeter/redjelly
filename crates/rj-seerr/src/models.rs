//! Response types. Only the fields redjelly reads; everything is optional or
//! defaulted because TMDb data is patchy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Movie,
    Tv,
}

impl MediaKind {
    /// Path segment and request `mediaType`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Tv => "tv",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: u64,
    #[serde(default, alias = "jellyfinUsername")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub total_pages: u32,
    #[serde(default)]
    pub total_results: u32,
    #[serde(default)]
    pub results: Vec<Title>,
}

/// Availability as Jellyseerr tracks it (`MediaStatus`).
pub mod status {
    pub const PENDING: u8 = 2;
    pub const PROCESSING: u8 = 3;
    pub const PARTIALLY_AVAILABLE: u8 = 4;
    pub const AVAILABLE: u8 = 5;
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    #[serde(default)]
    pub status: u8,
    /// Jellyfin item id once the title is in the library.
    #[serde(default)]
    pub jellyfin_media_id: Option<String>,
    #[serde(default)]
    pub requests: Vec<serde_json::Value>,
    #[serde(default)]
    pub seasons: Vec<SeasonStatus>,
}

impl MediaInfo {
    pub fn in_library(&self) -> bool {
        self.status == status::AVAILABLE || self.status == status::PARTIALLY_AVAILABLE
    }

    /// Requested but not (fully) available yet.
    pub fn requested(&self) -> bool {
        matches!(self.status, status::PENDING | status::PROCESSING) || (!self.in_library() && !self.requests.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatus {
    pub season_number: u32,
    #[serde(default)]
    pub status: u8,
}

/// A movie or show in a discover list. Shows use `name`/`firstAirDate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    pub id: u32,
    #[serde(default, alias = "name")]
    pub title: String,
    #[serde(default, alias = "firstAirDate")]
    pub release_date: Option<String>,
    #[serde(default)]
    pub poster_path: Option<String>,
    #[serde(default)]
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub popularity: f32,
    #[serde(default)]
    pub vote_average: f32,
    #[serde(default)]
    pub vote_count: u32,
    #[serde(default)]
    pub genre_ids: Vec<u32>,
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
}

impl Title {
    pub fn year(&self) -> Option<u32> {
        year_of(self.release_date.as_deref())
    }
}

pub(crate) fn year_of(date: Option<&str>) -> Option<u32> {
    date?.get(..4)?.parse().ok()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Genre {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchProvider {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub display_priority: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Season {
    pub season_number: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub episode_count: u32,
}

/// `/movie/{id}` and `/tv/{id}` merged: shows fill `name`, `firstAirDate`,
/// `episodeRunTime` and `seasons`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    pub id: u32,
    #[serde(default, alias = "name")]
    pub title: String,
    #[serde(default, alias = "firstAirDate")]
    pub release_date: Option<String>,
    #[serde(default)]
    pub runtime: Option<u32>,
    #[serde(default)]
    pub episode_run_time: Vec<u32>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub genres: Vec<Genre>,
    #[serde(default)]
    pub vote_average: f32,
    #[serde(default)]
    pub poster_path: Option<String>,
    #[serde(default)]
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub seasons: Vec<Season>,
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
}

impl Details {
    pub fn year(&self) -> Option<u32> {
        year_of(self.release_date.as_deref())
    }
}

/// IMDb + Rotten Tomatoes; any part may be missing.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ratings {
    /// 0–10.
    pub imdb: Option<f32>,
    /// Tomatometer, 0–100.
    pub rt_critics: Option<u32>,
    /// Popcornmeter, 0–100.
    pub rt_audience: Option<u32>,
}

impl Ratings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtRating {
    #[serde(default)]
    pub critics_score: Option<f32>,
    #[serde(default)]
    pub audience_score: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImdbRating {
    #[serde(default)]
    pub critics_score: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CombinedRatings {
    #[serde(default)]
    pub rt: Option<RtRating>,
    #[serde(default)]
    pub imdb: Option<ImdbRating>,
}

impl CombinedRatings {
    pub fn into_ratings(self) -> Ratings {
        // RT scores of 0 mean "no score yet" in practice.
        let pct = |v: Option<f32>| v.filter(|v| *v > 0.0).map(|v| v.round() as u32);
        let rt = self.rt.unwrap_or_default();
        Ratings {
            imdb: self.imdb.and_then(|i| i.critics_score).filter(|v| *v > 0.0),
            rt_critics: pct(rt.critics_score),
            rt_audience: pct(rt.audience_score),
        }
    }
}
