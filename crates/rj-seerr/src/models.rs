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
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub jellyfin_username: Option<String>,
}

impl User {
    pub fn name(&self) -> Option<&str> {
        self.display_name.as_deref().or(self.jellyfin_username.as_deref()).filter(|n| !n.is_empty())
    }
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

/// Request approval state (`MediaRequestStatus`).
pub mod request_status {
    pub const PENDING_APPROVAL: u8 = 1;
    pub const APPROVED: u8 = 2;
    pub const DECLINED: u8 = 3;
    pub const FAILED: u8 = 4;
    pub const COMPLETED: u8 = 5;
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    #[serde(default)]
    pub status: u8,
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub media_type: Option<MediaKind>,
    /// Jellyfin item id once the title is in the library.
    #[serde(default)]
    pub jellyfin_media_id: Option<String>,
    #[serde(default)]
    pub requests: Vec<RequestInfo>,
    #[serde(default)]
    pub seasons: Vec<SeasonStatus>,
    /// Jellyseerr's id of the Radarr/Sonarr server holding it.
    #[serde(default)]
    pub service_id: Option<u32>,
    /// The movie / series id inside that Radarr/Sonarr.
    #[serde(default)]
    pub external_service_id: Option<u32>,
    /// Radarr/Sonarr queue entries, as Jellyseerr last polled them.
    #[serde(default)]
    pub download_status: Vec<DownloadItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestInfo {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub status: u8,
    /// ISO 8601.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub requested_by: Option<User>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub size_left: f64,
    /// Radarr/Sonarr queue status: downloading, queued, paused, completed,
    /// delay, warning, failed, downloadClientUnavailable, …
    #[serde(default)]
    pub status: String,
    /// `HH:MM:SS` or `D.HH:MM:SS`.
    #[serde(default)]
    pub time_left: Option<String>,
    #[serde(default)]
    pub episode: Option<EpisodeRef>,
}

impl DownloadItem {
    /// 0–1, when the size is known.
    pub fn progress(&self) -> Option<f64> {
        (self.size > 0.0).then(|| (1.0 - self.size_left / self.size).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRef {
    #[serde(default)]
    pub season_number: u32,
    #[serde(default)]
    pub episode_number: u32,
}

/// One entry of `/request`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRequest {
    pub id: u64,
    #[serde(default)]
    pub status: u8,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<MediaKind>,
    #[serde(default)]
    pub media: Option<MediaInfo>,
    #[serde(default)]
    pub requested_by: Option<User>,
}

impl MediaRequest {
    /// The media with this request folded in (the list leaves
    /// `media.requests` empty).
    pub fn media_with_request(&self) -> MediaInfo {
        let mut m = self.media.clone().unwrap_or_default();
        if m.requests.is_empty() {
            m.requests.push(RequestInfo {
                id: self.id,
                status: self.status,
                created_at: self.created_at.clone(),
                requested_by: self.requested_by.clone(),
            });
        }
        m
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestPage {
    #[serde(default)]
    pub results: Vec<MediaRequest>,
}

/// A Radarr/Sonarr server as configured in Jellyseerr (admin only).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServarrServer {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub use_ssl: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    /// The address users open in a browser; often the reachable one.
    #[serde(default)]
    pub external_url: Option<String>,
}

impl ServarrServer {
    /// Addresses to try: the one Jellyseerr uses, then the external one.
    pub fn urls(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.hostname.is_empty() {
            let scheme = if self.use_ssl { "https" } else { "http" };
            let host = self.hostname.trim_end_matches('/');
            let host = host.split("://").last().unwrap_or(host);
            v.push(format!("{scheme}://{host}:{}{}", self.port, self.base_url.as_deref().unwrap_or("").trim_end_matches('/')));
        }
        if let Some(e) = self.external_url.as_deref().map(|e| e.trim().trim_end_matches('/')).filter(|e| !e.is_empty()) {
            v.push(e.to_owned());
        }
        v
    }
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
    #[serde(default)]
    pub tagline: Option<String>,
    #[serde(default, alias = "originalName")]
    pub original_title: Option<String>,
    #[serde(default)]
    pub original_language: Option<String>,
    /// "Released", "Returning Series", "Ended", …
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub vote_count: u32,
    #[serde(default)]
    pub budget: u64,
    #[serde(default)]
    pub revenue: u64,
    /// Shows only.
    #[serde(default)]
    pub last_air_date: Option<String>,
    #[serde(default)]
    pub number_of_episodes: Option<u32>,
    #[serde(default)]
    pub production_companies: Vec<Named>,
    #[serde(default)]
    pub production_countries: Vec<Named>,
    #[serde(default)]
    pub networks: Vec<Named>,
    #[serde(default)]
    pub created_by: Vec<Named>,
    #[serde(default)]
    pub collection: Option<Named>,
    #[serde(default)]
    pub related_videos: Vec<Video>,
    #[serde(default)]
    pub credits: Credits,
    #[serde(default)]
    pub watch_providers: Vec<RegionProviders>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Named {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Video {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Trailer, Teaser, Clip, …
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Credits {
    #[serde(default)]
    pub cast: Vec<CastMember>,
    #[serde(default)]
    pub crew: Vec<CrewMember>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CastMember {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub profile_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrewMember {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub profile_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegionProviders {
    #[serde(default)]
    pub iso_3166_1: String,
    /// Subscription services.
    #[serde(default)]
    pub flatrate: Vec<WatchProvider>,
    #[serde(default)]
    pub rent: Vec<WatchProvider>,
    #[serde(default)]
    pub buy: Vec<WatchProvider>,
}

impl Details {
    /// The YouTube trailer to play, preferring official trailers.
    pub fn trailer_url(&self) -> Option<&str> {
        let find = |kind: &str| {
            self.related_videos
                .iter()
                .find(|v| v.site.as_deref() == Some("YouTube") && v.kind.as_deref() == Some(kind) && v.url.is_some())
        };
        find("Trailer").or_else(|| find("Teaser")).and_then(|v| v.url.as_deref())
    }

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
