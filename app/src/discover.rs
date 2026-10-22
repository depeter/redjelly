//! Discover (SPEC §8): a wizard over Jellyseerr's TMDb discovery, a results
//! grid with optional IMDb / Rotten Tomatoes post-filters, title pages and
//! requests. Signs in to Jellyseerr with the current Jellyfin user's name
//! and their password from the credential store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use rj_core::config::{DiscoverFilters, DiscoverKind, DiscoverSort, LibraryFilter, RuntimeBucket};
use rj_seerr::models::{Details, DownloadItem, Genre, Ratings, Title, WatchProvider, status};
use rj_seerr::{Client as SeerrClient, DiscoverQuery, Error as SeerrError, MediaKind, Sort};

use crate::App;
use crate::images::ImageSrc;
use crate::layout::{self, STYLE_BUTTON, STYLE_SETTING};
use crate::nav::Placement;
use crate::screens::{CardContent, Content, HeaderContent, PageSource, Paging, RowContent, Source, Target, button, row};

const STEPS: [&str; 7] = ["What", "Genres", "Era", "Length", "Ratings", "Extras", "Sort"];
const DECADES: [u16; 7] = [2020, 2010, 2000, 1990, 1980, 1970, 0];
const TMDB_MINS: [f32; 6] = [0.0, 6.0, 6.5, 7.0, 7.5, 8.0];
const VOTE_MINS: [u32; 5] = [0, 100, 500, 1000, 5000];
const IMDB_MINS: [f32; 6] = [0.0, 6.0, 6.5, 7.0, 7.5, 8.0];
const RT_MINS: [u32; 5] = [0, 60, 70, 80, 90];
/// Rating sort without a vote floor is topped by obscure 10/10s.
const RATING_SORT_VOTES: u32 = 300;
/// Results stop after this many fetch rounds in a row without a match.
const MAX_DRY_ROUNDS: u32 = 5;
/// Shuffle draws from the first N pages (TMDb's most popular matches).
const SHUFFLE_PAGES: u32 = 20;
/// TMDb never serves pages past 500.
const MAX_PAGE: u32 = 500;
/// TMDb names the combined TV genres differently from the movie ones.
const TV_ALIASES: [(&str, &str); 5] = [
    ("Action", "Action & Adventure"),
    ("Adventure", "Action & Adventure"),
    ("Science Fiction", "Sci-Fi & Fantasy"),
    ("Fantasy", "Sci-Fi & Fantasy"),
    ("War", "War & Politics"),
];
const TMDB_IMAGES: &str = "https://image.tmdb.org/t/p";

type RatingsCache = Arc<Mutex<HashMap<(MediaKind, u32), Ratings>>>;
type SharedClient = Arc<Mutex<Option<SeerrClient>>>;

/// What a Discover card does.
#[derive(Debug, Clone)]
pub enum DiscoverAction {
    Set(Choice),
    Reset,
    Results,
    Request { kind: MediaKind, id: u32, title: String },
    /// Radarr/Sonarr interactive search for the title page's title.
    ManualSearch,
    /// Blocklist its downloads (if any) and search automatically.
    SearchAgain,
    ToggleSeason(u32),
    RequestSeasons,
}

/// One wizard answer to change.
#[derive(Debug, Clone)]
pub enum Choice {
    Kind(DiscoverKind),
    /// Cycles off → include → exclude.
    Genre(String),
    AllGenres,
    ClearGenres,
    Decade(u16),
    AnyDecade,
    Runtime(RuntimeBucket),
    TmdbMin,
    VotesMin,
    ImdbMin,
    RtCriticsMin,
    RtAudienceMin,
    Language(Option<String>),
    Provider(u32),
    AnyProvider,
    HideRequested,
    Library(LibraryFilter),
    Sort(DiscoverSort),
}

#[derive(Debug, Clone)]
struct PickSeason {
    number: u32,
    name: String,
    episodes: u32,
    /// Jellyseerr status; ≥ PENDING means requested or available already.
    status: u8,
    on: bool,
}

#[derive(Debug, Clone)]
struct SeasonPick {
    id: u32,
    title: String,
    seasons: Vec<PickSeason>,
}

/// Discover state on `App`.
#[derive(Default)]
pub struct State {
    /// Shared with async work (the Home row loads off the UI thread).
    client: SharedClient,
    genres: RefCell<Option<(Vec<Genre>, Vec<Genre>)>>,
    /// (region, providers) for the Extras step.
    providers: RefCell<Option<(String, Vec<WatchProvider>)>>,
    ratings: RatingsCache,
    seasons: RefCell<Option<SeasonPick>>,
    /// Last sign-in result, for the settings card.
    pub status: RefCell<String>,
}

impl State {
    /// Forget the session and cached lists (URL or password changed).
    pub fn reset(&self) {
        forget_session(&self.client);
        self.genres.borrow_mut().take();
        self.providers.borrow_mut().take();
    }
}

/// Everything needed to (re)sign in off the UI thread.
#[derive(Clone)]
struct Login {
    jellyfin: rj_jellyfin::Client,
    password: String,
}

async fn sign_in(c: &mut SeerrClient, login: &Login) -> anyhow::Result<String> {
    let me = login.jellyfin.me().await?;
    match c.login_jellyfin(&me.name, &login.password).await {
        Ok(_) => Ok(me.name),
        Err(SeerrError::BadCredentials) => {
            anyhow::bail!("Jellyseerr didn't accept the password for {}; update it in Settings → Discover", me.name)
        }
        Err(e) => Err(e.into()),
    }
}

fn forget_session(shared: &SharedClient) {
    if let Ok(mut c) = shared.lock() {
        c.take();
    }
}

/// Runs `op` signed in (signing in first, and once more if the session
/// expired), keeping the session in `shared` for next time.
async fn with_session<T, Fut>(
    shared: SharedClient,
    mut c: SeerrClient,
    login: Login,
    op: impl Fn(SeerrClient) -> Fut,
) -> anyhow::Result<T>
where
    Fut: Future<Output = Result<T, SeerrError>>,
{
    let mut fresh = false;
    if !c.has_session() {
        if let Err(e) = sign_in(&mut c, &login).await {
            forget_session(&shared);
            return Err(e);
        }
        fresh = true;
    }
    let r = match op(c.clone()).await {
        Err(SeerrError::Unauthorized) if !fresh => match sign_in(&mut c, &login).await {
            Ok(_) => op(c.clone()).await,
            Err(e) => {
                forget_session(&shared);
                return Err(e);
            }
        },
        r => r,
    };
    if let Ok(mut s) = shared.lock() {
        *s = Some(c);
    }
    Ok(r?)
}

/// `YYYY-MM-DD`, `days` before today (UTC).
fn date_days_ago(days: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    civil_date(now / 86_400 - days)
}

/// Days since 1970-01-01 → `YYYY-MM-DD` (Howard Hinnant's algorithm).
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---- Genres ---------------------------------------------------------------------

/// Genre names offered for `kind`; Both lists movie genres plus TV-only ones.
fn genre_names(kind: DiscoverKind, movie: &[Genre], tv: &[Genre]) -> Vec<String> {
    let mut names: Vec<String> = match kind {
        DiscoverKind::Shows => tv.iter().map(|g| g.name.clone()).collect(),
        _ => movie.iter().map(|g| g.name.clone()).collect(),
    };
    if kind == DiscoverKind::Both {
        let combined: Vec<&str> = TV_ALIASES.iter().map(|(_, t)| *t).collect();
        for g in tv {
            if !names.contains(&g.name) && !combined.contains(&g.name.as_str()) {
                names.push(g.name.clone());
            }
        }
    }
    names
}

/// TMDb ids meaning `name` for one media kind (may be several or none).
fn genre_ids(name: &str, kind: MediaKind, movie: &[Genre], tv: &[Genre]) -> Vec<u32> {
    let own = match kind {
        MediaKind::Movie => movie,
        MediaKind::Tv => tv,
    };
    if let Some(g) = own.iter().find(|g| g.name == name) {
        return vec![g.id];
    }
    let names: Vec<&str> = match kind {
        // "Action" for shows = "Action & Adventure".
        MediaKind::Tv => TV_ALIASES.iter().filter(|(m, _)| *m == name).map(|(_, t)| *t).collect(),
        // "Sci-Fi & Fantasy" for movies = Science Fiction or Fantasy.
        MediaKind::Movie => TV_ALIASES.iter().filter(|(_, t)| *t == name).map(|(m, _)| *m).collect(),
    };
    own.iter().filter(|g| names.contains(&g.name.as_str())).map(|g| g.id).collect()
}

// ---- Cursor: paged discovery with client-side filters -------------------------------

#[derive(Debug, Clone)]
struct KindCursor {
    kind: MediaKind,
    query: DiscoverQuery,
    exclude: Vec<u32>,
    /// Next page to fetch (sequential sorts).
    next: u32,
    /// Remaining pages, for shuffle (filled after page 1).
    order: Option<Vec<u32>>,
    done: bool,
}

impl KindCursor {
    fn page(&self) -> Option<u32> {
        if self.done {
            return None;
        }
        match &self.order {
            Some(order) => order.last().copied(),
            None => Some(self.next),
        }
    }

    /// `seed` is non-zero in shuffle mode.
    fn advance(&mut self, total_pages: u32, seed: &mut u64) {
        let last = total_pages.min(MAX_PAGE);
        match &mut self.order {
            Some(order) => {
                order.pop();
                self.done = order.is_empty();
            }
            None if self.next == 1 && *seed != 0 => {
                let mut order: Vec<u32> = (2..=last.min(SHUFFLE_PAGES)).collect();
                shuffle(&mut order, seed);
                self.done = order.is_empty();
                self.order = Some(order);
            }
            None => {
                self.next += 1;
                self.done = self.next > last;
            }
        }
    }
}

fn next_rand(seed: &mut u64) -> u64 {
    // xorshift64
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn shuffle<T>(v: &mut [T], seed: &mut u64) {
    for i in (1..v.len()).rev() {
        let j = (next_rand(seed) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

/// Where Discover results are in the TMDb lists, plus the filters TMDb
/// can't apply.
#[derive(Debug, Clone)]
pub struct Cursor {
    kinds: Vec<KindCursor>,
    filters: DiscoverFilters,
    /// Non-zero = shuffle.
    seed: u64,
    checked: u32,
    matched: u32,
    dry_rounds: u32,
}

impl Cursor {
    fn new(f: &DiscoverFilters, region: Option<&str>, movie: &[Genre], tv: &[Genre]) -> Self {
        let kinds: &[MediaKind] = match f.kind {
            DiscoverKind::Movies => &[MediaKind::Movie],
            DiscoverKind::Shows => &[MediaKind::Tv],
            DiscoverKind::Both => &[MediaKind::Movie, MediaKind::Tv],
        };
        let (date_from, date_to) = date_span(&f.decades);
        let (runtime_min, runtime_max) = match f.runtime {
            RuntimeBucket::Any => (None, None),
            RuntimeBucket::Short => (None, Some(99)),
            RuntimeBucket::Medium => (Some(100), Some(130)),
            RuntimeBucket::Long => (Some(131), None),
        };
        let mut votes = f.votes_min;
        if f.sort == DiscoverSort::Rating {
            votes = votes.max(RATING_SORT_VOTES);
        }
        let mut cursors = Vec::new();
        for &kind in kinds {
            let per_name: Vec<Vec<u32>> = f.include_genres.iter().map(|n| genre_ids(n, kind, movie, tv)).collect();
            // A kind that can't match the included genres is left out.
            let unmatched = per_name.iter().filter(|ids| ids.is_empty()).count();
            if !per_name.is_empty() && (unmatched == per_name.len() || (f.all_genres && unmatched > 0)) {
                continue;
            }
            let mut genres: Vec<u32> = per_name.into_iter().flatten().collect();
            genres.dedup();
            let exclude = f.exclude_genres.iter().flat_map(|n| genre_ids(n, kind, movie, tv)).collect();
            let query = DiscoverQuery {
                genres,
                all_genres: f.all_genres,
                sort: match f.sort {
                    DiscoverSort::Popular | DiscoverSort::Shuffle => Sort::Popularity,
                    DiscoverSort::Rating => Sort::Rating,
                    DiscoverSort::Newest => Sort::Newest,
                },
                date_from: date_from.clone(),
                date_to: date_to.clone(),
                runtime_min,
                runtime_max,
                vote_average_min: (f.tmdb_min > 0.0).then_some(f.tmdb_min),
                vote_count_min: (votes > 0).then_some(votes),
                watch_region: region.map(str::to_owned).filter(|_| !f.providers.is_empty()),
                watch_providers: f.providers.clone(),
            };
            cursors.push(KindCursor { kind, query, exclude, next: 1, order: None, done: false });
        }
        let seed = if f.sort == DiscoverSort::Shuffle {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
            nanos | 1
        } else {
            0
        };
        Self { kinds: cursors, filters: f.clone(), seed, checked: 0, matched: 0, dry_rounds: 0 }
    }

    fn exhausted(&self) -> bool {
        self.kinds.iter().all(|k| k.done) || self.dry_rounds >= MAX_DRY_ROUNDS
    }

    /// Filters TMDb couldn't apply (all free: no extra requests).
    fn keep(&self, t: &Title, exclude: &[u32]) -> bool {
        let f = &self.filters;
        if t.genre_ids.iter().any(|g| exclude.contains(g)) {
            return false;
        }
        if !f.decades.is_empty() {
            let Some(y) = t.year() else { return false };
            let decade = if y < 1970 { 0 } else { (y / 10 * 10) as u16 };
            if !f.decades.contains(&decade) {
                return false;
            }
        }
        if let Some(lang) = &f.language
            && t.original_language.as_deref() != Some(lang.as_str())
        {
            return false;
        }
        let info = t.media_info.clone().unwrap_or_default();
        match f.library {
            LibraryFilter::Only if !info.in_library() => return false,
            LibraryFilter::Hide if info.status == status::AVAILABLE => return false,
            _ => {}
        }
        !(f.hide_requested && info.requested())
    }

    /// IMDb has no show ratings, so shows skip the IMDb minimum.
    fn passes_ratings(&self, kind: MediaKind, r: &Ratings) -> bool {
        let f = &self.filters;
        let imdb_ok = f.imdb_min <= 0.0 || kind == MediaKind::Tv || r.imdb.is_some_and(|v| v >= f.imdb_min);
        let rt_ok = f.rt_critics_min == 0 || r.rt_critics.is_some_and(|v| v >= f.rt_critics_min);
        let aud_ok = f.rt_audience_min == 0 || r.rt_audience.is_some_and(|v| v >= f.rt_audience_min);
        imdb_ok && rt_ok && aud_ok
    }

    fn progress(&self) -> String {
        if self.exhausted() && self.matched == 0 {
            return format!("Checked {} titles · nothing matches. Try loosening the filters.", self.checked);
        }
        if self.filters.needs_ratings() || self.checked > self.matched {
            format!("Checked {} titles · {} match", self.checked, self.matched)
        } else {
            format!("{} titles", self.matched)
        }
    }
}

/// Query date range spanning the chosen decades (exact decades are
/// filtered afterwards).
fn date_span(decades: &[u16]) -> (Option<String>, Option<String>) {
    if decades.is_empty() {
        return (None, None);
    }
    let from = decades.iter().min().copied().filter(|d| *d > 0).map(|d| format!("{d}-01-01"));
    let max = decades.iter().max().copied().unwrap_or(0);
    let to = if max == 0 { "1969-12-31".to_owned() } else { format!("{}-12-31", max + 9) };
    (from, Some(to))
}

/// A matching title from one fetch round.
#[derive(Debug, Clone)]
pub struct Found {
    kind: MediaKind,
    title: Title,
    ratings: Option<Ratings>,
}

async fn cached_ratings(c: &SeerrClient, cache: &RatingsCache, kind: MediaKind, id: u32) -> Ratings {
    if let Some(r) = cache.lock().ok().and_then(|m| m.get(&(kind, id)).copied()) {
        return r;
    }
    match c.ratings(kind, id).await {
        Ok(r) => {
            if let Ok(mut m) = cache.lock() {
                m.insert((kind, id), r);
            }
            r
        }
        Err(e) => {
            tracing::debug!("ratings {kind:?} {id}: {e}");
            Ratings::default()
        }
    }
}

/// One fetch round: the next page of every media kind, filtered.
async fn fetch_round(c: SeerrClient, mut cur: Cursor, cache: RatingsCache) -> Result<(Vec<Found>, Cursor), SeerrError> {
    let mut handles = Vec::new();
    for (i, k) in cur.kinds.iter().enumerate() {
        let Some(page) = k.page() else { continue };
        let (c, q, kind) = (c.clone(), k.query.clone(), k.kind);
        handles.push((i, tokio::spawn(async move { c.discover(kind, &q, page).await })));
    }
    let mut candidates: Vec<(MediaKind, Title)> = Vec::new();
    for (i, h) in handles {
        let page = match h.await {
            Ok(r) => r?,
            Err(e) => {
                tracing::warn!("discover task: {e}");
                cur.kinds[i].done = true;
                continue;
            }
        };
        let mut seed = cur.seed;
        cur.kinds[i].advance(page.total_pages, &mut seed);
        cur.seed = seed;
        if page.results.is_empty() {
            cur.kinds[i].done = true;
        }
        cur.checked += page.results.len() as u32;
        let (kind, exclude) = (cur.kinds[i].kind, cur.kinds[i].exclude.clone());
        candidates.extend(page.results.into_iter().filter(|t| cur.keep(t, &exclude)).map(|t| (kind, t)));
    }

    // Movies and shows side by side in sort order.
    let f = &cur.filters;
    match f.sort {
        DiscoverSort::Popular => candidates.sort_by(|a, b| b.1.popularity.total_cmp(&a.1.popularity)),
        DiscoverSort::Rating => candidates.sort_by(|a, b| b.1.vote_average.total_cmp(&a.1.vote_average)),
        DiscoverSort::Newest => candidates.sort_by(|a, b| b.1.release_date.cmp(&a.1.release_date)),
        DiscoverSort::Shuffle => shuffle(&mut candidates, &mut cur.seed),
    }

    let found: Vec<Found> = if f.needs_ratings() {
        let sem = Arc::new(tokio::sync::Semaphore::new(4));
        let handles: Vec<_> = candidates
            .into_iter()
            .map(|(kind, t)| {
                let (c, cache, sem) = (c.clone(), cache.clone(), sem.clone());
                tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    let r = cached_ratings(&c, &cache, kind, t.id).await;
                    Found { kind, title: t, ratings: Some(r) }
                })
            })
            .collect();
        let mut out = Vec::new();
        for h in handles {
            if let Ok(found) = h.await
                && cur.passes_ratings(found.kind, found.ratings.as_ref().unwrap_or(&Ratings::default()))
            {
                out.push(found);
            }
        }
        out
    } else {
        candidates.into_iter().map(|(kind, title)| Found { kind, title, ratings: None }).collect()
    };
    cur.matched += found.len() as u32;
    cur.dry_rounds = if found.is_empty() { cur.dry_rounds + 1 } else { 0 };
    Ok((found, cur))
}

// ---- Formatting ---------------------------------------------------------------------

/// "IMDb 7.8 · RT 92% · Audience 88%", empty when nothing is known.
pub fn ratings_line(r: &Ratings) -> String {
    let mut parts = Vec::new();
    if let Some(v) = r.imdb {
        parts.push(format!("IMDb {v:.1}"));
    }
    if let Some(v) = r.rt_critics {
        parts.push(format!("RT {v}%"));
    }
    if let Some(v) = r.rt_audience {
        parts.push(format!("Audience {v}%"));
    }
    parts.join(" · ")
}

pub fn tmdb_image(path: Option<&str>, size: &str) -> Option<ImageSrc> {
    let path = path?;
    Some(ImageSrc { url: format!("{TMDB_IMAGES}/{size}{path}"), key: format!("tmdb-{size}-{}", path.trim_start_matches('/')) })
}

fn status_label(info: Option<&rj_seerr::models::MediaInfo>) -> &'static str {
    match info {
        Some(i) if i.status == status::AVAILABLE => "In library",
        Some(i) if i.status == status::PARTIALLY_AVAILABLE => "Partly in library",
        Some(i) if i.requested() => "Requested",
        _ => "",
    }
}

/// Where a title leads: straight to the Jellyfin item when it's fully there.
fn title_target(kind: MediaKind, t: &Title) -> Target {
    if let Some(info) = &t.media_info
        && info.status == status::AVAILABLE
        && let Some(id) = &info.jellyfin_media_id
    {
        return Target::Open(match kind {
            MediaKind::Movie => Source::Detail { id: id.clone() },
            MediaKind::Tv => Source::Series { id: id.clone() },
        });
    }
    Target::Open(Source::SeerrTitle { kind, id: t.id })
}

fn found_card(f: &Found) -> CardContent {
    let t = &f.title;
    let mut parts: Vec<String> = Vec::new();
    if let Some(y) = t.year() {
        parts.push(y.to_string());
    }
    match f.ratings.as_ref().and_then(|r| r.imdb.map(|v| format!("IMDb {v:.1}")).or(r.rt_critics.map(|v| format!("RT {v}%")))) {
        Some(r) => parts.push(r),
        None if t.vote_average > 0.0 => parts.push(format!("★ {:.1}", t.vote_average)),
        None => {}
    }
    match t.media_info.as_ref().and_then(|i| crate::requests::request_status(i, crate::requests::today())) {
        Some(s) => parts.push(s),
        None => {
            let label = status_label(t.media_info.as_ref());
            if !label.is_empty() {
                parts.push(label.into());
            }
        }
    }
    if f.kind == MediaKind::Tv {
        parts.push("Show".into());
    }
    CardContent {
        title: t.title.clone(),
        subtitle: parts.join(" · "),
        image: tmdb_image(t.poster_path.as_deref(), "w342"),
        progress: t
            .media_info
            .as_ref()
            .and_then(|i| i.download_status.first())
            .and_then(DownloadItem::progress)
            .map_or(-1.0, |p| p as f32),
        played: t.media_info.as_ref().is_some_and(|i| i.status == status::AVAILABLE),
        mark: 0,
        target: title_target(f.kind, t),
    }
}

/// One-line summary of the wizard answers.
fn summary(f: &DiscoverFilters, providers: &[WatchProvider]) -> String {
    let mut parts: Vec<String> = vec![
        match f.kind {
            DiscoverKind::Movies => "Movies",
            DiscoverKind::Shows => "Shows",
            DiscoverKind::Both => "Movies & shows",
        }
        .into(),
    ];
    if !f.include_genres.is_empty() {
        parts.push(f.include_genres.join(if f.all_genres { " + " } else { " / " }));
    }
    if !f.exclude_genres.is_empty() {
        parts.push(format!("no {}", f.exclude_genres.join(", ")));
    }
    if !f.decades.is_empty() {
        let mut d = f.decades.clone();
        d.sort_unstable_by(|a, b| b.cmp(a));
        parts.push(d.iter().map(|d| decade_label(*d)).collect::<Vec<_>>().join(", "));
    }
    if f.kind != DiscoverKind::Shows && f.runtime != RuntimeBucket::Any {
        parts.push(runtime_label(f.runtime).into());
    }
    if f.tmdb_min > 0.0 {
        parts.push(format!("TMDb {:.1}+", f.tmdb_min));
    }
    if f.votes_min > 0 {
        parts.push(format!("{}+ votes", f.votes_min));
    }
    if f.imdb_min > 0.0 {
        parts.push(format!("IMDb {:.1}+", f.imdb_min));
    }
    if f.rt_critics_min > 0 {
        parts.push(format!("RT {}%+", f.rt_critics_min));
    }
    if f.rt_audience_min > 0 {
        parts.push(format!("Audience {}%+", f.rt_audience_min));
    }
    if let Some(l) = f.language.as_deref().and_then(rj_core::lang::Lang::parse) {
        parts.push(l.name());
    }
    if !f.providers.is_empty() {
        let names: Vec<&str> =
            providers.iter().filter(|p| f.providers.contains(&p.id)).map(|p| p.name.as_str()).collect();
        parts.push(if names.is_empty() { format!("{} services", f.providers.len()) } else { names.join(", ") });
    }
    match f.library {
        LibraryFilter::Only => parts.push("in my library".into()),
        LibraryFilter::Hide => parts.push("not in my library".into()),
        LibraryFilter::Any => {}
    }
    if f.hide_requested {
        parts.push("not requested".into());
    }
    parts.push(sort_label(f.sort).to_lowercase());
    parts.join(" · ")
}

fn decade_label(d: u16) -> String {
    if d == 0 { "Before 1970".into() } else { format!("{d}s") }
}

fn runtime_label(r: RuntimeBucket) -> &'static str {
    match r {
        RuntimeBucket::Any => "Any length",
        RuntimeBucket::Short => "Under 100 min",
        RuntimeBucket::Medium => "100–130 min",
        RuntimeBucket::Long => "Over 130 min",
    }
}

fn sort_label(s: DiscoverSort) -> &'static str {
    match s {
        DiscoverSort::Popular => "Popular",
        DiscoverSort::Rating => "Highest rated",
        DiscoverSort::Newest => "Newest",
        DiscoverSort::Shuffle => "Surprise me",
    }
}

fn next_in<T: PartialEq + Copy>(list: &[T], current: T) -> T {
    let i = list.iter().position(|v| *v == current).map_or(0, |i| (i + 1) % list.len());
    list[i]
}

fn toggle<T: PartialEq>(list: &mut Vec<T>, v: T) {
    match list.iter().position(|x| *x == v) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(v),
    }
}

fn apply(f: &mut DiscoverFilters, choice: Choice) {
    match choice {
        Choice::Kind(k) => f.kind = k,
        Choice::Genre(name) => {
            if let Some(i) = f.include_genres.iter().position(|g| *g == name) {
                f.include_genres.remove(i);
                f.exclude_genres.push(name);
            } else if let Some(i) = f.exclude_genres.iter().position(|g| *g == name) {
                f.exclude_genres.remove(i);
            } else {
                f.include_genres.push(name);
            }
        }
        Choice::AllGenres => f.all_genres ^= true,
        Choice::ClearGenres => {
            f.include_genres.clear();
            f.exclude_genres.clear();
        }
        Choice::Decade(d) => toggle(&mut f.decades, d),
        Choice::AnyDecade => f.decades.clear(),
        Choice::Runtime(r) => f.runtime = r,
        Choice::TmdbMin => f.tmdb_min = next_in(&TMDB_MINS, f.tmdb_min),
        Choice::VotesMin => f.votes_min = next_in(&VOTE_MINS, f.votes_min),
        Choice::ImdbMin => f.imdb_min = next_in(&IMDB_MINS, f.imdb_min),
        Choice::RtCriticsMin => f.rt_critics_min = next_in(&RT_MINS, f.rt_critics_min),
        Choice::RtAudienceMin => f.rt_audience_min = next_in(&RT_MINS, f.rt_audience_min),
        Choice::Language(l) => f.language = l,
        Choice::Provider(id) => toggle(&mut f.providers, id),
        Choice::AnyProvider => f.providers.clear(),
        Choice::HideRequested => f.hide_requested ^= true,
        Choice::Library(l) => f.library = l,
        Choice::Sort(s) => f.sort = s,
    }
}

fn setting_card(title: impl Into<String>, subtitle: impl Into<String>, mark: i32, target: Target) -> CardContent {
    CardContent { title: title.into(), subtitle: subtitle.into(), image: None, progress: -1.0, played: false, mark, target }
}

fn choice(title: impl Into<String>, on: bool, c: Choice) -> CardContent {
    setting_card(title, if on { "Selected" } else { "" }, i32::from(on), Target::Discover(DiscoverAction::Set(c)))
}

fn cycle(title: &str, value: String, set: bool, c: Choice) -> CardContent {
    setting_card(title, value, i32::from(set), Target::Discover(DiscoverAction::Set(c)))
}

fn settings_row(title: &str, cards: Vec<CardContent>) -> RowContent {
    row(title, STYLE_SETTING, cards)
}

/// Splits cards into rows that fit the window, the first one titled.
fn wrapped(title: &str, cards: Vec<CardContent>, per_row: usize) -> Vec<RowContent> {
    let mut rows = Vec::new();
    let mut it = cards.into_iter().peekable();
    while it.peek().is_some() {
        let chunk: Vec<_> = it.by_ref().take(per_row.max(1)).collect();
        rows.push(settings_row(if rows.is_empty() { title } else { "" }, chunk));
    }
    rows
}

/// Steps shown for these answers (Length only applies to movies).
fn steps(f: &DiscoverFilters) -> Vec<u8> {
    (0..STEPS.len() as u8).filter(|s| !(*s == 3 && f.kind == DiscoverKind::Shows)).collect()
}

// ---- App glue -----------------------------------------------------------------------

impl App {
    fn seerr_url(&self) -> Option<String> {
        self.config.borrow().discover.seerr_url.clone()
    }

    fn seerr_password_account(&self) -> Option<String> {
        let user = self.client.borrow().as_ref()?.user_id()?.to_owned();
        Some(crate::secrets::provider_account("jellyseerr", &user))
    }

    pub fn set_seerr_password(&self, password: &str) -> bool {
        let Some(account) = self.seerr_password_account() else { return false };
        self.tokens.set_secret(&account, password);
        self.discover.reset();
        true
    }

    /// Runs `op` with a signed-in Jellyseerr client (signing in first, and
    /// once more if the session expired), then `then` on the UI thread.
    fn seerr_call<T, Fut>(
        &self,
        op: impl Fn(SeerrClient) -> Fut + Send + Sync + 'static,
        then: impl FnOnce(&Rc<App>, anyhow::Result<T>) + Send + 'static,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T, SeerrError>> + Send + 'static,
    {
        match self.seerr_setup() {
            Ok((shared, client, login)) => self.spawn_ui(with_session(shared, client, login, op), then),
            Err(e) => crate::with_app(|app| then(app, Err(e))),
        }
    }

    /// The pieces `with_session` needs, or why Jellyseerr can't be used.
    fn seerr_setup(&self) -> anyhow::Result<(SharedClient, SeerrClient, Login)> {
        let url = self.seerr_url().ok_or_else(|| anyhow::anyhow!("Set the Jellyseerr address in Settings → Discover"))?;
        let jellyfin = self.client.borrow().clone().ok_or_else(|| anyhow::anyhow!("Not signed in"))?;
        let password = self
            .seerr_password_account()
            .and_then(|a| self.tokens.get_secret(&a))
            .ok_or_else(|| anyhow::anyhow!("Enter your Jellyfin password for Jellyseerr in Settings → Discover"))?;
        let shared = self.discover.client.clone();
        let client = match shared.lock().ok().and_then(|c| c.clone()) {
            Some(c) => c,
            None => SeerrClient::new(&url)?,
        };
        Ok((shared, client, Login { jellyfin, password }))
    }

    /// Home's Jellyseerr rows: "Requests" (open requests and their
    /// download state) and "Popular & well rated" (recent, well rated
    /// movies). None when Jellyseerr isn't set up; rows that fail or take
    /// too long are left out, so Home never fails because of them.
    pub fn home_seerr_rows(&self) -> Option<impl Future<Output = Vec<RowContent>> + Send + 'static> {
        self.seerr_url()?;
        let (shared, client, login) = self.seerr_setup().ok()?;
        let query = DiscoverQuery {
            sort: Sort::Popularity,
            date_from: Some(date_days_ago(2 * 365)),
            vote_average_min: Some(7.0),
            vote_count_min: Some(200),
            ..Default::default()
        };
        Some(async move {
            let requests = with_session(shared.clone(), client.clone(), login.clone(), crate::requests::requests_row);
            let popular = with_session(shared, client, login, move |c| {
                let q = query.clone();
                async move { c.discover(MediaKind::Movie, &q, 1).await }
            });
            let (requests, popular) = tokio::join!(
                tokio::time::timeout(std::time::Duration::from_secs(8), requests),
                tokio::time::timeout(std::time::Duration::from_secs(5), popular),
            );
            fn ok<T>(what: &str, r: Result<anyhow::Result<T>, tokio::time::error::Elapsed>) -> Option<T> {
                match r {
                    Ok(Ok(v)) => Some(v),
                    Ok(Err(e)) => {
                        tracing::warn!("home {what} row: {e:#}");
                        None
                    }
                    Err(_) => {
                        tracing::warn!("home {what} row: Jellyseerr took too long");
                        None
                    }
                }
            }
            let requests: Vec<CardContent> = ok("requests", requests).unwrap_or_default();
            let popular: Vec<CardContent> = ok("popular", popular)
                .map(|page: rj_seerr::models::Page| {
                    page.results
                        .into_iter()
                        .map(|title| found_card(&Found { kind: MediaKind::Movie, title, ratings: None }))
                        .collect()
                })
                .unwrap_or_default();
            crate::requests::home_rows(requests, popular)
        })
    }

    /// Settings → Discover → Test: signs in afresh and reports who.
    pub fn test_seerr(&self) {
        forget_session(&self.discover.client);
        *self.discover.status.borrow_mut() = "Signing in…".into();
        self.refresh_settings();
        self.seerr_call(
            |c| async move { c.me().await },
            |app, r| {
                let msg = match r {
                    Ok(u) => format!("Signed in as {}", u.name().unwrap_or("you")),
                    Err(e) => format!("{e:#}"),
                };
                app.toast(msg.clone());
                *app.discover.status.borrow_mut() = msg;
                app.refresh_settings();
            },
        );
    }

    /// Like `load`, for screens that need Jellyseerr.
    fn seerr_load<T, Fut>(
        &self,
        placement: Placement,
        op: impl Fn(SeerrClient) -> Fut + Send + Sync + 'static,
        build: impl FnOnce(&Rc<App>, T) -> Content + Send + 'static,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T, SeerrError>> + Send + 'static,
    {
        if placement == Placement::Push {
            if self.busy.get() {
                return;
            }
            self.busy.set(true);
            self.toast("Loading…");
        }
        self.seerr_call(op, move |app, r| {
            if placement == Placement::Push {
                app.busy.set(false);
                app.clear_toast();
            }
            match r {
                Ok(v) => {
                    let content = build(app, v);
                    app.present(content, placement);
                    // Results start empty and fill themselves.
                    app.maybe_load_more();
                }
                Err(e) if placement == Placement::Refresh => tracing::debug!("discover refresh: {e:#}"),
                Err(e) => {
                    tracing::warn!("discover: {e:#}");
                    app.toast(format!("{e:#}"));
                }
            }
        });
    }

    pub fn open_discover(&self, source: Source, placement: Placement) {
        let region = self.config.borrow().discover.region.clone().filter(|r| !r.is_empty());
        let have_genres = self.discover.genres.borrow().is_some();
        let have_providers = region.is_none()
            || self.discover.providers.borrow().as_ref().is_some_and(|(r, _)| Some(r) == region.as_ref());
        match source {
            Source::Discover { .. } | Source::DiscoverResults if have_genres && have_providers => {
                let content = self.discover_content(&source);
                self.present(content, placement);
                self.maybe_load_more();
            }
            Source::Discover { .. } | Source::DiscoverResults => {
                let src = source.clone();
                self.seerr_load(
                    placement,
                    move |c| {
                        let region = region.clone();
                        async move {
                            let (movie, tv) = tokio::try_join!(c.genres(MediaKind::Movie), c.genres(MediaKind::Tv))?;
                            let providers = match &region {
                                Some(r) => {
                                    let (mut m, t) =
                                        tokio::try_join!(c.watch_providers(MediaKind::Movie, r), c.watch_providers(MediaKind::Tv, r))?;
                                    for p in t {
                                        if !m.iter().any(|x| x.id == p.id) {
                                            m.push(p);
                                        }
                                    }
                                    m.sort_by_key(|p| p.display_priority);
                                    Some((r.clone(), m))
                                }
                                None => None,
                            };
                            Ok(((movie, tv), providers))
                        }
                    },
                    move |app, (genres, providers)| {
                        *app.discover.genres.borrow_mut() = Some(genres);
                        *app.discover.providers.borrow_mut() = providers;
                        app.discover_content(&src)
                    },
                );
            }
            Source::SeerrTitle { kind, id } => {
                let cache = self.requests.arr.clone();
                self.seerr_load(
                    placement,
                    move |c| {
                        let cache = cache.clone();
                        async move {
                            let d = c.details(kind, id).await?;
                            // Radarr/Sonarr only matter until it's all there.
                            let info = d.media_info.as_ref().filter(|i| i.status != status::AVAILABLE);
                            let arr = crate::requests::arr_info(&c, &cache, kind, info).await;
                            Ok((d, arr))
                        }
                    },
                    move |app, (d, arr)| {
                        let content = title_content(kind, &d, app.config.borrow().discover.region.as_deref(), &arr);
                        app.set_title_arr(kind, &d, arr);
                        content
                    },
                )
            }
            Source::SeasonPicker { id } => {
                if placement == Placement::ReplaceTop && self.discover.seasons.borrow().as_ref().is_some_and(|p| p.id == id) {
                    return self.present(self.season_content(), placement);
                }
                self.seerr_load(
                    placement,
                    move |c| async move { c.details(MediaKind::Tv, id).await },
                    move |app, d| {
                        let statuses = d.media_info.as_ref().map(|i| i.seasons.clone()).unwrap_or_default();
                        let seasons = d
                            .seasons
                            .iter()
                            .map(|s| {
                                let status = statuses.iter().find(|x| x.season_number == s.season_number).map_or(0, |x| x.status);
                                PickSeason {
                                    number: s.season_number,
                                    name: if s.name.is_empty() { format!("Season {}", s.season_number) } else { s.name.clone() },
                                    episodes: s.episode_count,
                                    status,
                                    on: s.season_number > 0 && status < status::PENDING,
                                }
                            })
                            .collect();
                        *app.discover.seasons.borrow_mut() = Some(SeasonPick { id, title: d.title.clone(), seasons });
                        app.season_content()
                    },
                );
            }
            _ => {}
        }
    }

    fn discover_content(&self, source: &Source) -> Content {
        match source {
            Source::Discover { step } => self.wizard_content(*step),
            _ => self.results_content(),
        }
    }

    fn wizard_content(&self, step: u8) -> Content {
        let f = self.config.borrow().discover.last.clone();
        let genres = self.discover.genres.borrow().clone().unwrap_or_default();
        let providers = self.discover.providers.borrow().clone();
        let per_row = layout::visible_cards(self.window_width() - layout::SIDE_PAD, STYLE_SETTING);
        let set = |c: Choice| Target::Discover(DiscoverAction::Set(c));
        let mut rows: Vec<RowContent> = Vec::new();
        let mut info = "Enter picks · Esc goes back a step".to_owned();
        match step {
            0 => rows.push(settings_row(
                "What do you feel like?",
                vec![
                    choice("Movies", f.kind == DiscoverKind::Movies, Choice::Kind(DiscoverKind::Movies)),
                    choice("Shows", f.kind == DiscoverKind::Shows, Choice::Kind(DiscoverKind::Shows)),
                    choice("Movies & shows", f.kind == DiscoverKind::Both, Choice::Kind(DiscoverKind::Both)),
                ],
            )),
            1 => {
                info = "Enter cycles a genre: include → exclude → off".into();
                rows.push(settings_row(
                    "",
                    vec![
                        setting_card(
                            "Included genres",
                            if f.all_genres { "Must have all" } else { "Any of them" },
                            0,
                            set(Choice::AllGenres),
                        ),
                        setting_card("Clear genres", "", 0, set(Choice::ClearGenres)),
                    ],
                ));
                let cards = genre_names(f.kind, &genres.0, &genres.1)
                    .into_iter()
                    .map(|name| {
                        let (label, mark) = if f.include_genres.contains(&name) {
                            ("Include", 1)
                        } else if f.exclude_genres.contains(&name) {
                            ("Exclude", 2)
                        } else {
                            ("", 0)
                        };
                        setting_card(name.clone(), label, mark, set(Choice::Genre(name)))
                    })
                    .collect();
                rows.extend(wrapped("Genres", cards, per_row));
            }
            2 => {
                let mut cards = vec![choice("Any era", f.decades.is_empty(), Choice::AnyDecade)];
                cards.extend(DECADES.iter().map(|d| choice(decade_label(*d), f.decades.contains(d), Choice::Decade(*d))));
                rows.extend(wrapped("When was it made? (pick several)", cards, per_row));
            }
            3 => rows.push(settings_row(
                "How long?",
                [RuntimeBucket::Any, RuntimeBucket::Short, RuntimeBucket::Medium, RuntimeBucket::Long]
                    .into_iter()
                    .map(|r| choice(runtime_label(r), f.runtime == r, Choice::Runtime(r)))
                    .collect(),
            )),
            4 => {
                let any_or = |set: bool, v: String| if set { v } else { "Any".into() };
                rows.push(settings_row(
                    "TMDb (fast)",
                    vec![
                        cycle("TMDb score", any_or(f.tmdb_min > 0.0, format!("{:.1} or more", f.tmdb_min)), f.tmdb_min > 0.0, Choice::TmdbMin),
                        cycle("Votes on TMDb", any_or(f.votes_min > 0, format!("{} or more", f.votes_min)), f.votes_min > 0, Choice::VotesMin),
                    ],
                ));
                rows.push(settings_row(
                    "IMDb & Rotten Tomatoes (checked per title, slower)",
                    vec![
                        cycle("IMDb", any_or(f.imdb_min > 0.0, format!("{:.1} or more", f.imdb_min)), f.imdb_min > 0.0, Choice::ImdbMin),
                        cycle(
                            "Rotten Tomatoes critics",
                            any_or(f.rt_critics_min > 0, format!("{}% or more", f.rt_critics_min)),
                            f.rt_critics_min > 0,
                            Choice::RtCriticsMin,
                        ),
                        cycle(
                            "Rotten Tomatoes audience",
                            any_or(f.rt_audience_min > 0, format!("{}% or more", f.rt_audience_min)),
                            f.rt_audience_min > 0,
                            Choice::RtAudienceMin,
                        ),
                    ],
                ));
                info = "Enter cycles a minimum · IMDb only exists for movies".into();
            }
            5 => {
                let mut langs: Vec<String> = self.config.borrow().languages.preferred.clone();
                if !langs.iter().any(|l| l == "en") {
                    langs.push("en".into());
                }
                let mut cards = vec![choice("Any language", f.language.is_none(), Choice::Language(None))];
                cards.extend(langs.iter().filter_map(|l| rj_core::lang::Lang::parse(l)).map(|l| {
                    let code = l.as_str().to_owned();
                    choice(l.name(), f.language.as_deref() == Some(code.as_str()), Choice::Language(Some(code)))
                }));
                rows.extend(wrapped("Original language", cards, per_row));
                rows.push(settings_row(
                    "My library & requests",
                    vec![
                        choice("Anything", f.library == LibraryFilter::Any, Choice::Library(LibraryFilter::Any)),
                        choice("Only what I have", f.library == LibraryFilter::Only, Choice::Library(LibraryFilter::Only)),
                        choice("Only what I don't have", f.library == LibraryFilter::Hide, Choice::Library(LibraryFilter::Hide)),
                        cycle("Already requested", if f.hide_requested { "Hidden" } else { "Shown" }.into(), f.hide_requested, Choice::HideRequested),
                    ],
                ));
                match providers {
                    Some((region, list)) => {
                        let mut cards = vec![choice("Any service", f.providers.is_empty(), Choice::AnyProvider)];
                        cards.extend(list.iter().take(23).map(|p| choice(p.name.clone(), f.providers.contains(&p.id), Choice::Provider(p.id))));
                        rows.extend(wrapped(&format!("Streaming on ({region})"), cards, per_row));
                    }
                    None => rows.push(settings_row(
                        "Streaming on",
                        vec![setting_card(
                            "Set your streaming region",
                            "Settings → Discover",
                            0,
                            Target::Setting(crate::settings::Setting::SeerrRegion),
                        )],
                    )),
                }
            }
            _ => rows.push(settings_row(
                "Order",
                [DiscoverSort::Popular, DiscoverSort::Rating, DiscoverSort::Newest, DiscoverSort::Shuffle]
                    .into_iter()
                    .map(|s| choice(sort_label(s), f.sort == s, Choice::Sort(s)))
                    .collect(),
            )),
        }

        let order = steps(&f);
        let pos = order.iter().position(|s| *s == step).unwrap_or(0);
        let mut nav = Vec::new();
        if let Some(next) = order.get(pos + 1) {
            nav.push(button(format!("Next: {} ›", STEPS[*next as usize]), Target::Open(Source::Discover { step: *next })));
        }
        nav.push(button("Show results", Target::Discover(DiscoverAction::Results)));
        nav.push(button("Start over", Target::Discover(DiscoverAction::Reset)));
        rows.push(row("", STYLE_BUTTON, nav));

        let providers = self.discover.providers.borrow().as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        Content {
            source: Source::Discover { step },
            header: Some(HeaderContent {
                title: format!("Discover · {} ({}/{})", STEPS[step as usize], pos + 1, order.len()),
                meta: summary(&f, &providers),
                info,
                ..Default::default()
            }),
            rows,
            focus: (0, 0),
            paging: None,
        }
    }

    fn results_content(&self) -> Content {
        let f = self.config.borrow().discover.last.clone();
        let region = self.config.borrow().discover.region.clone();
        let genres = self.discover.genres.borrow().clone().unwrap_or_default();
        let providers = self.discover.providers.borrow().as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        let cursor = Cursor::new(&f, region.as_deref(), &genres.0, &genres.1);
        let columns = layout::grid_columns(self.window_width());
        let empty = cursor.kinds.is_empty();
        Content {
            source: Source::DiscoverResults,
            header: Some(HeaderContent {
                title: "Discover".into(),
                meta: summary(&f, &providers),
                info: if empty { "No genre matches both movies and shows; pick other genres.".into() } else { "Searching…".into() },
                ..Default::default()
            }),
            rows: Vec::new(),
            focus: (0, 0),
            paging: (!empty).then(|| Paging { source: PageSource::Discover(Box::new(cursor)), loaded: 0, total: 0, columns }),
        }
    }

    /// Fetches the next round of results into the grid with `generation`.
    pub fn discover_more(&self, generation: u64, cursor: Cursor) {
        let cache = self.discover.ratings.clone();
        self.seerr_call(
            move |c| fetch_round(c, cursor.clone(), cache.clone()),
            move |app, r| match r {
                Ok((found, cursor)) => {
                    let done = cursor.exhausted();
                    let progress = cursor.progress();
                    app.update_paging(generation, |p| {
                        if let Some(p) = p {
                            p.source = PageSource::Discover(Box::new(cursor));
                        }
                    });
                    app.update_header(generation, |h| h.info = progress.into());
                    app.append_grid(generation, found.iter().map(found_card).collect(), done);
                    if app.top_generation() == Some(generation) {
                        app.maybe_load_more();
                    }
                }
                Err(e) => {
                    app.toast(format!("Couldn't load more: {e:#}"));
                    app.update_header(generation, |h| h.info = format!("{e:#}").into());
                    app.update_paging(generation, |p| *p = None);
                }
            },
        );
    }

    /// Appends IMDb/RT to the header meta of the screen with `generation`.
    pub fn load_header_ratings(&self, generation: u64, kind: MediaKind, id: u32) {
        if self.seerr_url().is_none() {
            return;
        }
        let cache = self.discover.ratings.clone();
        self.seerr_call(
            move |c| {
                let cache = cache.clone();
                async move { Ok(cached_ratings(&c, &cache, kind, id).await) }
            },
            move |app, r| match r {
                Ok(r) if !r.is_empty() => {
                    let line = ratings_line(&r);
                    app.update_header(generation, |h| h.meta = format!("{} · {line}", h.meta).into());
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("header ratings: {e:#}"),
            },
        );
    }

    fn season_content(&self) -> Content {
        let pick = self.discover.seasons.borrow().clone();
        let Some(pick) = pick else {
            return Content { source: Source::SeasonPicker { id: 0 }, header: None, rows: Vec::new(), focus: (0, 0), paging: None };
        };
        let per_row = layout::visible_cards(self.window_width() - layout::SIDE_PAD, STYLE_SETTING);
        let cards = pick
            .seasons
            .iter()
            .map(|s| {
                let eps = format!("{} episode{}", s.episodes, if s.episodes == 1 { "" } else { "s" });
                if s.status >= status::PENDING {
                    let state = match s.status {
                        status::AVAILABLE => "In library",
                        status::PARTIALLY_AVAILABLE => "Partly in library",
                        _ => "Requested",
                    };
                    setting_card(s.name.clone(), format!("{eps} · {state}"), 0, Target::None)
                } else {
                    setting_card(
                        s.name.clone(),
                        if s.on { format!("{eps} · Selected") } else { eps },
                        i32::from(s.on),
                        Target::Discover(DiscoverAction::ToggleSeason(s.number)),
                    )
                }
            })
            .collect();
        let n = pick.seasons.iter().filter(|s| s.on).count();
        let mut rows = wrapped("Seasons", cards, per_row);
        rows.insert(
            0,
            row(
                "",
                STYLE_BUTTON,
                vec![button(
                    format!("Request {n} season{}", if n == 1 { "" } else { "s" }),
                    Target::Discover(DiscoverAction::RequestSeasons),
                )],
            ),
        );
        Content {
            source: Source::SeasonPicker { id: pick.id },
            header: Some(HeaderContent {
                title: format!("Request · {}", pick.title),
                info: "Enter toggles a season".into(),
                ..Default::default()
            }),
            rows,
            focus: (1, 0),
            paging: None,
        }
    }

    pub fn discover_action(&self, action: DiscoverAction) {
        match action {
            DiscoverAction::ManualSearch => self.manual_search(),
            DiscoverAction::SearchAgain => self.search_again(),
            DiscoverAction::Set(c) => {
                apply(&mut self.config.borrow_mut().discover.last, c);
                self.save_config();
                if let Some(src @ Source::Discover { .. }) = self.top_source() {
                    self.present(self.discover_content(&src), Placement::ReplaceTop);
                }
            }
            DiscoverAction::Reset => {
                self.config.borrow_mut().discover.last = DiscoverFilters::default();
                self.save_config();
                // Back to the first step.
                let first = self.nav.borrow().iter().position(|s| matches!(s.source, Source::Discover { .. }));
                if let Some(i) = first {
                    self.nav.borrow_mut().truncate(i + 1);
                    self.present(self.wizard_content(0), Placement::ReplaceTop);
                }
            }
            DiscoverAction::Results => self.open(Source::DiscoverResults, Placement::Push),
            DiscoverAction::Request { kind, id, title } => self.seerr_call(
                move |c| async move { c.request(kind, id, &[]).await },
                move |app, r| match r {
                    Ok(()) => {
                        app.toast(format!("Requested {title}"));
                        app.reload_top();
                    }
                    Err(e) => app.toast(format!("Couldn't request: {e:#}")),
                },
            ),
            DiscoverAction::ToggleSeason(n) => {
                if let Some(p) = self.discover.seasons.borrow_mut().as_mut()
                    && let Some(s) = p.seasons.iter_mut().find(|s| s.number == n)
                {
                    s.on ^= true;
                }
                self.present(self.season_content(), Placement::ReplaceTop);
            }
            DiscoverAction::RequestSeasons => {
                let Some(pick) = self.discover.seasons.borrow().clone() else { return };
                let seasons: Vec<u32> = pick.seasons.iter().filter(|s| s.on).map(|s| s.number).collect();
                if seasons.is_empty() {
                    return self.toast("Pick at least one season");
                }
                self.seerr_call(
                    move |c| {
                        let seasons = seasons.clone();
                        async move { c.request(MediaKind::Tv, pick.id, &seasons).await }
                    },
                    move |app, r| match r {
                        Ok(()) => {
                            app.toast("Requested");
                            app.back();
                            app.reload_top();
                        }
                        Err(e) => app.toast(format!("Couldn't request: {e:#}")),
                    },
                );
            }
        }
    }
}

/// "$160M" / "$825M" / "$1.2B".
fn money(v: u64) -> String {
    let v = v as f64;
    if v >= 1e9 { format!("${:.1}B", v / 1e9) } else { format!("${:.0}M", (v / 1e6).max(1.0)) }
}

/// Facts for a TMDb title's About row, skipping unknowns.
fn title_about(kind: MediaKind, d: &Details) -> Vec<CardContent> {
    use crate::screens::{format_date, info_card};
    let mut cards = Vec::new();
    let date = |d: Option<&str>| d.and_then(format_date);
    match kind {
        MediaKind::Movie => {
            if let Some(r) = date(d.release_date.as_deref()) {
                cards.push(info_card("Released", r));
            }
        }
        MediaKind::Tv => {
            if let Some(r) = date(d.release_date.as_deref()) {
                cards.push(info_card("First aired", r));
            }
            if let Some(r) = date(d.last_air_date.as_deref()) {
                cards.push(info_card("Last aired", r));
            }
            if let Some(n) = d.number_of_episodes {
                cards.push(info_card("Episodes", n.to_string()));
            }
        }
    }
    let mins = d.runtime.or(d.episode_run_time.first().copied()).unwrap_or(0);
    if mins > 0 {
        let label = if kind == MediaKind::Tv { "Episode length" } else { "Runtime" };
        cards.push(info_card(label, if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins} min") }));
    }
    if let Some(s) = d.status.as_ref().filter(|s| !s.is_empty()) {
        cards.push(info_card("Status", s.clone()));
    }
    if d.vote_average > 0.0 {
        cards.push(info_card("TMDb", format!("★ {:.1} · {} votes", d.vote_average, d.vote_count)));
    }
    if !d.genres.is_empty() {
        cards.push(info_card("Genres", d.genres.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let crew = |jobs: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for c in d.credits.crew.iter().filter(|c| c.job.as_deref().is_some_and(|j| jobs.contains(&j))) {
            if !v.contains(&c.name) {
                v.push(c.name.clone());
            }
        }
        v
    };
    let names = |v: &[rj_seerr::models::Named]| v.iter().map(|n| n.name.clone()).collect::<Vec<_>>();
    let lists = [
        ("Created by", names(&d.created_by)),
        ("Director", crew(&["Director"])),
        ("Writers", crew(&["Screenplay", "Writer", "Story", "Novel"])),
        ("Music", crew(&["Original Music Composer"])),
        ("Network", names(&d.networks)),
        ("Studio", names(&d.production_companies)),
        ("Country", names(&d.production_countries)),
    ];
    for (label, list) in lists {
        if !list.is_empty() {
            cards.push(info_card(label, list.join(", ")));
        }
    }
    if let Some(t) = d.original_title.as_ref().filter(|t| **t != d.title && !t.is_empty()) {
        cards.push(info_card("Original title", t.clone()));
    }
    if let Some(l) = d.original_language.as_deref().and_then(rj_core::lang::Lang::parse) {
        cards.push(info_card("Original language", l.name()));
    }
    if d.budget > 0 {
        cards.push(info_card("Budget", money(d.budget)));
    }
    if d.revenue > 0 {
        cards.push(info_card("Box office", money(d.revenue)));
    }
    if let Some(c) = d.collection.as_ref().filter(|c| !c.name.is_empty()) {
        cards.push(info_card("Collection", c.name.clone()));
    }
    cards
}

/// Streaming / rent / buy in `region`, one card each.
fn where_to_watch(d: &Details, region: Option<&str>) -> Vec<CardContent> {
    let Some(r) = region.and_then(|r| d.watch_providers.iter().find(|p| p.iso_3166_1.eq_ignore_ascii_case(r))) else {
        return Vec::new();
    };
    let list = |v: &[WatchProvider]| v.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ");
    [("Streaming", &r.flatrate), ("Rent", &r.rent), ("Buy", &r.buy)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(label, v)| crate::screens::info_card(label, list(v)))
        .collect()
}

fn cast_cards(d: &Details) -> Vec<CardContent> {
    d.credits
        .cast
        .iter()
        .take(30)
        .map(|c| CardContent {
            title: c.name.clone(),
            subtitle: c.character.clone().unwrap_or_default(),
            image: tmdb_image(c.profile_path.as_deref(), "w185"),
            progress: -1.0,
            played: false,
            mark: 0,
            target: Target::None,
        })
        .collect()
}

/// A TMDb title page: what Jellyseerr knows, with Request / Open / Trailer.
fn title_content(kind: MediaKind, d: &Details, region: Option<&str>, arr: &crate::requests::ArrInfo) -> Content {
    let info = d.media_info.clone().unwrap_or_default();
    let mut buttons = Vec::new();
    if info.in_library()
        && let Some(jf) = &info.jellyfin_media_id
    {
        let src = match kind {
            MediaKind::Movie => Source::Detail { id: jf.clone() },
            MediaKind::Tv => Source::Series { id: jf.clone() },
        };
        buttons.push(button("Open in library", Target::Open(src)));
    }
    if let Some(url) = d.trailer_url() {
        buttons.push(button(
            "Trailer",
            Target::Trailer { item_id: String::new(), local: false, remote: Some(url.to_owned()), title: d.title.clone() },
        ));
    }
    match kind {
        MediaKind::Movie if info.status == status::AVAILABLE => {}
        MediaKind::Movie if info.requested() => buttons.push(button("Requested", Target::None)),
        MediaKind::Movie => {
            buttons.push(button("Request", Target::Discover(DiscoverAction::Request { kind, id: d.id, title: d.title.clone() })))
        }
        MediaKind::Tv => {
            let open = d.seasons.iter().any(|s| {
                s.season_number > 0
                    && info.seasons.iter().find(|x| x.season_number == s.season_number).is_none_or(|x| x.status < status::PENDING)
            });
            if open {
                let label = if info.in_library() || info.requested() { "Request more seasons…" } else { "Request…" };
                buttons.push(button(label, Target::Open(Source::SeasonPicker { id: d.id })));
            } else if info.status != status::AVAILABLE {
                buttons.push(button("Requested", Target::None));
            }
        }
    }

    if arr.client.is_some() && arr.arr_id.is_some() {
        buttons.push(button("Manual search…", Target::Discover(DiscoverAction::ManualSearch)));
        let again = if arr.queue.is_empty() { "Search again" } else { "Remove & search again" };
        buttons.push(button(again, Target::Discover(DiscoverAction::SearchAgain)));
    }

    let mut meta: Vec<String> = Vec::new();
    if let Some(y) = d.year() {
        meta.push(y.to_string());
    }
    let mins = d.runtime.or(d.episode_run_time.first().copied()).unwrap_or(0);
    if mins > 0 {
        meta.push(if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins} min") });
    }
    if kind == MediaKind::Tv {
        let n = d.seasons.iter().filter(|s| s.season_number > 0).count();
        meta.push(format!("{n} season{}", if n == 1 { "" } else { "s" }));
    }
    if !d.genres.is_empty() {
        meta.push(d.genres.iter().take(3).map(|g| g.name.as_str()).collect::<Vec<_>>().join(", "));
    }
    if d.vote_average > 0.0 {
        meta.push(format!("TMDb {:.1}", d.vote_average));
    }
    let status_line = match (crate::requests::request_status(&info, crate::requests::today()), status_label(Some(&info))) {
        (Some(s), _) => s,
        (None, "") => "Not in your library".to_owned(),
        (None, s) => s.to_owned(),
    };
    let info_line = match d.tagline.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => format!("{t}  ·  {status_line}"),
        None => status_line,
    };

    let mut rows = Vec::new();
    if !buttons.is_empty() {
        rows.push(row("", STYLE_BUTTON, buttons));
    }
    rows.extend(crate::requests::download_row(arr));
    let about = title_about(kind, d);
    if !about.is_empty() {
        rows.push(row("About", STYLE_SETTING, about));
    }
    let watch = where_to_watch(d, region);
    if !watch.is_empty() {
        rows.push(row(format!("Where to watch ({})", region.unwrap_or_default()), STYLE_SETTING, watch));
    }
    let cast = cast_cards(d);
    if !cast.is_empty() {
        rows.push(row("Cast", crate::layout::STYLE_POSTER, cast));
    }
    Content {
        source: Source::SeerrTitle { kind, id: d.id },
        header: Some(HeaderContent {
            title: d.title.clone(),
            meta: meta.join(" · "),
            info: info_line,
            overview: d.overview.clone().unwrap_or_default(),
            backdrop: tmdb_image(d.backdrop_path.as_deref(), "w1280"),
            ratings_for: Some((kind, d.id)),
        }),
        rows,
        focus: (0, 0),
        paging: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(id: u32, name: &str) -> Genre {
        Genre { id, name: name.into() }
    }

    fn genres() -> (Vec<Genre>, Vec<Genre>) {
        (
            vec![g(28, "Action"), g(12, "Adventure"), g(878, "Science Fiction"), g(27, "Horror"), g(36, "History")],
            vec![g(10759, "Action & Adventure"), g(10765, "Sci-Fi & Fantasy"), g(10764, "Reality")],
        )
    }

    #[test]
    fn genre_ids_cross_kinds() {
        let (m, t) = genres();
        assert_eq!(genre_ids("Action", MediaKind::Movie, &m, &t), [28]);
        assert_eq!(genre_ids("Action", MediaKind::Tv, &m, &t), [10759]);
        assert_eq!(genre_ids("Action & Adventure", MediaKind::Movie, &m, &t), [28, 12]);
        assert!(genre_ids("Reality", MediaKind::Movie, &m, &t).is_empty());
        assert!(genre_ids("History", MediaKind::Tv, &m, &t).is_empty());
    }

    #[test]
    fn both_lists_movie_genres_plus_tv_only() {
        let (m, t) = genres();
        let names = genre_names(DiscoverKind::Both, &m, &t);
        assert!(names.contains(&"Reality".to_owned()));
        assert!(!names.contains(&"Action & Adventure".to_owned()));
        assert_eq!(genre_names(DiscoverKind::Shows, &m, &t).len(), 3);
    }

    #[test]
    fn cursor_skips_kinds_that_cannot_match() {
        let (m, t) = genres();
        let f = DiscoverFilters { kind: DiscoverKind::Both, include_genres: vec!["History".into()], ..Default::default() };
        let c = Cursor::new(&f, None, &m, &t);
        assert_eq!(c.kinds.len(), 1);
        assert_eq!(c.kinds[0].kind, MediaKind::Movie);

        let f = DiscoverFilters {
            kind: DiscoverKind::Both,
            include_genres: vec!["History".into(), "Reality".into()],
            exclude_genres: vec!["Horror".into()],
            ..Default::default()
        };
        let c = Cursor::new(&f, None, &m, &t);
        assert_eq!(c.kinds.len(), 2);
        assert_eq!(c.kinds[0].exclude, [27]);

        let f = DiscoverFilters { all_genres: true, ..f };
        assert!(Cursor::new(&f, None, &m, &t).kinds.is_empty());
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(19_723), "2024-01-01");
        assert_eq!(civil_date(19_782), "2024-02-29");
        assert_eq!(civil_date(20_748), "2026-10-22");
    }

    #[test]
    fn decades_to_dates() {
        assert_eq!(date_span(&[]), (None, None));
        assert_eq!(date_span(&[1990, 2010]), (Some("1990-01-01".into()), Some("2019-12-31".into())));
        assert_eq!(date_span(&[0, 1980]), (None, Some("1989-12-31".into())));
        assert_eq!(date_span(&[0]), (None, Some("1969-12-31".into())));
    }

    #[test]
    fn client_side_filters() {
        let f = DiscoverFilters {
            decades: vec![1990],
            language: Some("en".into()),
            library: LibraryFilter::Hide,
            hide_requested: true,
            ..Default::default()
        };
        let c = Cursor::new(&f, None, &[], &[]);
        let t = |date: &str, lang: &str, status: u8| Title {
            release_date: Some(date.into()),
            original_language: Some(lang.into()),
            media_info: Some(rj_seerr::models::MediaInfo { status, ..Default::default() }),
            genre_ids: vec![27],
            ..Default::default()
        };
        assert!(c.keep(&t("1994-05-01", "en", 0), &[]));
        assert!(!c.keep(&t("2004-05-01", "en", 0), &[]));
        assert!(!c.keep(&t("1994-05-01", "fr", 0), &[]));
        assert!(!c.keep(&t("1994-05-01", "en", status::AVAILABLE), &[]));
        assert!(!c.keep(&t("1994-05-01", "en", status::PENDING), &[]));
        assert!(!c.keep(&t("1994-05-01", "en", 0), &[27]));
    }

    #[test]
    fn rating_post_filter() {
        let f = DiscoverFilters { imdb_min: 7.0, rt_critics_min: 80, ..Default::default() };
        let c = Cursor::new(&f, None, &[], &[]);
        let r = |imdb, rt| Ratings { imdb, rt_critics: rt, rt_audience: None };
        assert!(c.passes_ratings(MediaKind::Movie, &r(Some(7.5), Some(90))));
        assert!(!c.passes_ratings(MediaKind::Movie, &r(Some(6.9), Some(90))));
        assert!(!c.passes_ratings(MediaKind::Movie, &r(None, Some(90))));
        // Shows have no IMDb rating; only RT counts.
        assert!(c.passes_ratings(MediaKind::Tv, &r(None, Some(85))));
    }

    #[test]
    fn shuffle_pages_cover_first_twenty() {
        let mut k = KindCursor { kind: MediaKind::Movie, query: DiscoverQuery::default(), exclude: vec![], next: 1, order: None, done: false };
        let mut seed = 12345;
        assert_eq!(k.page(), Some(1));
        k.advance(300, &mut seed);
        let mut seen = vec![1];
        while let Some(p) = k.page() {
            seen.push(p);
            k.advance(300, &mut seed);
        }
        seen.sort_unstable();
        assert_eq!(seen, (1..=20).collect::<Vec<_>>());

        let mut k = KindCursor { order: None, next: 1, done: false, ..k };
        let mut no_shuffle = 0;
        k.advance(2, &mut no_shuffle);
        assert_eq!(k.page(), Some(2));
        k.advance(2, &mut no_shuffle);
        assert_eq!(k.page(), None);
    }

    #[test]
    fn genre_cycle_and_summary() {
        let mut f = DiscoverFilters::default();
        apply(&mut f, Choice::Genre("Horror".into()));
        assert_eq!(f.include_genres, ["Horror"]);
        apply(&mut f, Choice::Genre("Horror".into()));
        assert!(f.include_genres.is_empty());
        assert_eq!(f.exclude_genres, ["Horror"]);
        apply(&mut f, Choice::Genre("Horror".into()));
        assert!(f.exclude_genres.is_empty());
        apply(&mut f, Choice::ImdbMin);
        apply(&mut f, Choice::Decade(1990));
        assert_eq!(summary(&f, &[]), "Movies · 1990s · IMDb 6.0+ · popular");
    }

    #[test]
    fn title_page_has_trailer_about_and_watch() {
        let d: Details = serde_json::from_value(serde_json::json!({
            "id": 27205, "title": "Inception", "releaseDate": "2010-07-15", "runtime": 148,
            "budget": 160000000, "revenue": 825532764u64, "tagline": "Your mind is the scene of the crime.",
            "relatedVideos": [{ "url": "https://www.youtube.com/watch?v=YoHD9XEInc0", "type": "Trailer", "site": "YouTube" }],
            "credits": { "cast": [{ "name": "Leonardo DiCaprio", "character": "Cobb" }],
                         "crew": [{ "name": "Christopher Nolan", "job": "Director" }, { "name": "Christopher Nolan", "job": "Screenplay" }] },
            "watchProviders": [{ "iso_3166_1": "BE", "flatrate": [{ "id": 8, "name": "Netflix" }] }]
        }))
        .unwrap();
        let c = title_content(MediaKind::Movie, &d, Some("BE"), &Default::default());
        let titled = |t: &str| c.rows.iter().find(|r| r.title.starts_with(t));
        assert!(c.rows[0].cards.iter().any(|b| b.title == "Trailer"));
        assert!(c.rows[0].cards.iter().any(|b| b.title == "Request"));
        let about: Vec<(&str, &str)> = titled("About").unwrap().cards.iter().map(|c| (c.title.as_str(), c.subtitle.as_str())).collect();
        assert!(about.contains(&("Director", "Christopher Nolan")));
        assert!(about.contains(&("Writers", "Christopher Nolan")));
        assert!(about.contains(&("Budget", "$160M")));
        assert!(about.contains(&("Box office", "$826M")));
        assert_eq!(titled("Where to watch").unwrap().cards[0].subtitle, "Netflix");
        assert_eq!(titled("Cast").unwrap().cards[0].subtitle, "Cobb");
        assert!(c.header.unwrap().info.starts_with("Your mind"));
        // No region: no Where to watch row.
        assert!(title_content(MediaKind::Movie, &d, None, &Default::default()).rows.iter().all(|r| !r.title.starts_with("Where")));
    }

    #[test]
    fn ratings_formatting() {
        let r = Ratings { imdb: Some(7.84), rt_critics: Some(92), rt_audience: None };
        assert_eq!(ratings_line(&r), "IMDb 7.8 · RT 92%");
        assert_eq!(ratings_line(&Ratings::default()), "");
    }
}
