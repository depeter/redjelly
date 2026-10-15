use rj_core::lang::Lang;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    OpenSubtitles,
    JellyfinServer,
    Subdl,
}

/// What we know about the item being played, from Jellyfin metadata.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub languages: Vec<Lang>,
    pub title: String,
    pub year: Option<u32>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<String>,
    /// For episodes: the series' ids.
    pub parent_imdb_id: Option<String>,
    pub parent_tmdb_id: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// File name without directories, used for release matching.
    pub file_name: Option<String>,
    pub movie_hash: Option<String>,
    /// Jellyfin item id, for the server-side provider.
    pub jellyfin_item_id: String,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub provider: ProviderId,
    /// Provider-specific id used for download.
    pub id: String,
    pub lang: Lang,
    pub release: String,
    pub format: Option<String>,
    pub hash_match: bool,
    pub hearing_impaired: bool,
    pub forced: bool,
    pub downloads: u32,
    pub rating: Option<f32>,
}

/// A downloaded subtitle, already converted to UTF-8.
#[derive(Debug, Clone)]
pub struct SubtitleFile {
    pub lang: Lang,
    /// `srt`, `ass` or `vtt`.
    pub format: String,
    pub text: String,
    pub hearing_impaired: bool,
    pub forced: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}: not signed in or API key missing")]
    Auth(&'static str),
    #[error("{0}: daily download limit reached")]
    Quota(&'static str),
    #[error("{0}: unexpected response: {1}")]
    Protocol(&'static str, String),
    #[error("{0}: not implemented yet")]
    NotImplemented(&'static str),
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    async fn search(&self, q: &Query) -> Result<Vec<Candidate>, Error>;

    /// Downloads a candidate. Returns `None` for providers that store the
    /// file on the Jellyfin server themselves (no upload step needed).
    async fn download(&self, q: &Query, c: &Candidate) -> Result<Option<SubtitleFile>, Error>;
}

impl ProviderId {
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::OpenSubtitles => "OpenSubtitles",
            ProviderId::JellyfinServer => "Jellyfin",
            ProviderId::Subdl => "Subdl",
        }
    }

    /// Downloads count against a limited (or paid) daily quota.
    pub fn costs_quota(self) -> bool {
        self == ProviderId::OpenSubtitles
    }
}
