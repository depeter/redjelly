//! Requests: what's happening to requested titles (approval, Radarr/Sonarr
//! download queue), the Home "Requests" row, and Radarr/Sonarr tools on
//! title pages: manual release search + grab, and remove & search again.
//! Radarr/Sonarr addresses and keys come from Jellyseerr's settings, which
//! needs a Jellyseerr admin account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rj_seerr::arr::{QueueItem, Release};
use rj_seerr::models::{Details, DownloadItem, MediaInfo, request_status, status};
use rj_seerr::{ArrClient, Client as SeerrClient, Error as SeerrError, MediaKind, SearchScope};

use crate::App;
use crate::layout::{STYLE_POSTER, STYLE_SETTING};
use crate::nav::Placement;
use crate::screens::{CardContent, RowContent, Source, Target, info_card, row};
use crate::subtitles::{Panel, PanelAction, PanelKind, entry, header};

/// Home and title pages re-check statuses this often while on screen.
pub const REFRESH: Duration = Duration::from_secs(30);
/// A second press within this confirms "Remove & search again".
const CONFIRM: Duration = Duration::from_secs(4);
const REQUESTS_ROW: u32 = 30;

pub type ArrCache = Arc<Mutex<HashMap<(MediaKind, u32), ArrClient>>>;

#[derive(Default)]
pub struct State {
    pub arr: ArrCache,
    releases: RefCell<Option<ReleasePanel>>,
    title: RefCell<Option<TitleArr>>,
    reattempt_armed: Cell<Option<(u32, Instant)>>,
    remove_armed: Cell<Option<(MediaKind, u32, Instant)>>,
    next_refresh: Cell<Option<Instant>>,
}

// ---- Status text ---------------------------------------------------------------------

/// Days since 1970-01-01 for a `YYYY-MM-DD…` date.
fn days_from_civil(date: &str) -> Option<i64> {
    let y: i64 = date.get(..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn today() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64 / 86_400)
}

fn ago(created: Option<&str>, today: i64) -> Option<String> {
    let days = today - days_from_civil(created?)?;
    Some(match days {
        ..=0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    })
}

/// "12 min", "2 h 5 min", "1 d 3 h" from `HH:MM:SS` / `D.HH:MM:SS`.
fn time_left(t: &str) -> Option<String> {
    let (days, rest) = match t.split_once('.') {
        Some((d, r)) if !d.contains(':') => (d.parse::<u64>().ok()?, r),
        _ => (0, t),
    };
    let mut parts = rest.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m) = (parts.next()??, parts.next()??);
    Some(match (days, h, m) {
        (0, 0, 0) => "less than a minute".into(),
        (0, 0, m) => format!("{m} min"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, h, _) => format!("{d} d {h} h"),
    })
}

/// Radarr/Sonarr queue statuses, worst first.
const SEVERITY: [&str; 8] = ["failed", "warning", "downloadClientUnavailable", "paused", "delay", "downloading", "queued", "completed"];

fn severity(status: &str) -> usize {
    SEVERITY.iter().position(|s| s.eq_ignore_ascii_case(status)).unwrap_or(SEVERITY.len())
}

/// One line for Jellyseerr's view of the download queue.
fn download_line(items: &[DownloadItem]) -> Option<String> {
    let worst = items.iter().min_by_key(|i| severity(&i.status))?;
    let (size, left): (f64, f64) = items.iter().fold((0.0, 0.0), |(s, l), i| (s + i.size, l + i.size_left));
    let files = if items.len() > 1 { format!(" · {} files", items.len()) } else { String::new() };
    Some(match worst.status.to_ascii_lowercase().as_str() {
        "downloading" => {
            let pct = if size > 0.0 { format!(" {:.0}%", (1.0 - left / size) * 100.0) } else { String::new() };
            let eta = worst.time_left.as_deref().and_then(time_left).map(|t| format!(" · {t} left")).unwrap_or_default();
            format!("Downloading{pct}{eta}{files}")
        }
        "queued" => format!("Queued in the download client{files}"),
        "paused" => format!("Paused in the download client{files}"),
        "delay" => "Waiting for a better release (delay profile)".into(),
        "completed" => "Downloaded · importing".into(),
        "warning" => "Stuck: the download has a warning".into(),
        "failed" => "Download failed".into(),
        "downloadclientunavailable" => "Download client unreachable".into(),
        other => format!("Download {other}"),
    })
}

/// What's happening with a requested title; None when there's nothing to
/// say (never requested, or fully in the library).
pub fn request_status(info: &MediaInfo, today: i64) -> Option<String> {
    if info.status == status::AVAILABLE {
        return None;
    }
    if let Some(line) = download_line(&info.download_status) {
        return Some(line);
    }
    let req = info.requests.iter().max_by(|a, b| a.created_at.cmp(&b.created_at));
    let when = req.and_then(|r| ago(r.created_at.as_deref(), today));
    let requested = when.as_ref().map(|w| format!(" · requested {w}")).unwrap_or_default();
    match req.map(|r| r.status) {
        Some(request_status::PENDING_APPROVAL) => Some(format!("Waiting for approval{requested}")),
        Some(request_status::DECLINED) => Some("Request declined".into()),
        Some(request_status::FAILED) => Some("Request failed: Radarr/Sonarr couldn't add it".into()),
        _ if info.status == status::PARTIALLY_AVAILABLE => Some("Partly in library · rest not downloaded yet".into()),
        _ if matches!(info.status, status::PENDING | status::PROCESSING) || req.is_some() => {
            let old = req.and_then(|r| r.created_at.as_deref()).and_then(days_from_civil).is_some_and(|d| today - d >= 2);
            Some(if old { format!("No release found yet{requested}") } else { format!("Searching for a release{requested}") })
        }
        _ => None,
    }
}

/// One Radarr/Sonarr queue entry: (headline, details).
fn queue_lines(q: &QueueItem) -> (String, String) {
    let pct = if q.size > 0.0 { format!(" {:.0}%", (1.0 - q.sizeleft / q.size) * 100.0) } else { String::new() };
    let state = q.tracked_download_state.as_deref().unwrap_or("");
    let problem = matches!(q.tracked_download_status.as_deref(), Some("warning" | "error"));
    let headline = match (state, q.status.to_ascii_lowercase().as_str()) {
        ("importBlocked", _) => "Import blocked".into(),
        ("importPending", _) => "Downloaded · waiting to import".into(),
        ("importing", _) => "Importing".into(),
        ("failedPending", _) | (_, "failed") => "Download failed".into(),
        (_, "downloading") if problem => format!("Stuck at{pct}"),
        (_, "downloading") => {
            let eta = q.timeleft.as_deref().and_then(time_left).map(|t| format!(" · {t} left")).unwrap_or_default();
            format!("Downloading{pct}{eta}")
        }
        (_, "queued") => "Queued in the download client".into(),
        (_, "paused") => "Paused in the download client".into(),
        (_, "delay") => "Waiting for a better release".into(),
        (_, "completed") => "Downloaded".into(),
        (_, other) => other.to_owned(),
    };
    let mut details = q.messages();
    if details.is_empty() {
        details.push(q.title.clone());
    }
    if let Some(c) = &q.download_client {
        details.push(c.clone());
    }
    (headline, details.join(" · "))
}

// ---- Radarr / Sonarr ---------------------------------------------------------------

/// Radarr/Sonarr facts for a title page.
#[derive(Debug, Clone, Default)]
pub struct ArrInfo {
    pub client: Option<ArrClient>,
    /// Movie / series id inside Radarr/Sonarr.
    pub arr_id: Option<u32>,
    pub queue: Vec<QueueItem>,
    pub error: Option<String>,
}

async fn arr_client(c: &SeerrClient, cache: &ArrCache, kind: MediaKind, service_id: u32) -> Result<ArrClient, String> {
    if let Some(a) = cache.lock().ok().and_then(|m| m.get(&(kind, service_id)).cloned()) {
        return Ok(a);
    }
    let what = if kind == MediaKind::Movie { "Radarr" } else { "Sonarr" };
    let servers = c.servarr_servers(kind).await.map_err(|e| match e {
        SeerrError::Unauthorized => format!("{what} tools need a Jellyseerr admin account"),
        e => format!("Couldn't read {what} settings from Jellyseerr: {e}"),
    })?;
    let server = servers.iter().find(|s| s.id == service_id).ok_or_else(|| format!("{what} server not found in Jellyseerr"))?;
    let a = ArrClient::connect(server, kind).await.map_err(|e| e.to_string())?;
    if let Ok(mut m) = cache.lock() {
        m.insert((kind, service_id), a.clone());
    }
    Ok(a)
}

/// Connects to the title's Radarr/Sonarr and reads its queue. Never fails:
/// problems end up in `error` so the page still shows.
pub async fn arr_info(c: &SeerrClient, cache: &ArrCache, kind: MediaKind, info: Option<&MediaInfo>) -> ArrInfo {
    let Some((service, arr_id)) = info.and_then(|i| i.service_id.zip(i.external_service_id)) else {
        return ArrInfo::default();
    };
    let client = match arr_client(c, cache, kind, service).await {
        Ok(a) => a,
        Err(e) => return ArrInfo { error: Some(e), arr_id: Some(arr_id), ..Default::default() },
    };
    match client.queue(arr_id).await {
        Ok(queue) => ArrInfo { client: Some(client), arr_id: Some(arr_id), queue, error: None },
        Err(e) => {
            // A stale cached address: reconnect next time.
            if let Ok(mut m) = cache.lock() {
                m.remove(&(kind, service));
            }
            ArrInfo { client: Some(client), arr_id: Some(arr_id), queue: Vec::new(), error: Some(e.to_string()) }
        }
    }
}

/// The title page's Download row: queue entries with their real messages.
pub fn download_row(arr: &ArrInfo) -> Option<RowContent> {
    let mut cards: Vec<CardContent> = arr
        .queue
        .iter()
        .map(|q| {
            let (headline, details) = queue_lines(q);
            info_card(headline, details)
        })
        .collect();
    if let Some(e) = &arr.error {
        cards.push(info_card("Radarr / Sonarr", e.clone()));
    }
    (!cards.is_empty()).then(|| row("Download", STYLE_SETTING, cards))
}

#[derive(Clone)]
struct TitleArr {
    kind: MediaKind,
    id: u32,
    title: String,
    seasons: Vec<u32>,
    arr: ArrInfo,
    /// Jellyseerr media id and request ids, for "Remove request".
    media_id: Option<u64>,
    requests: Vec<u64>,
    partial: bool,
}

// ---- Home "Requests" row --------------------------------------------------------------

/// Open and failed requests (everyone's when allowed), with titles and
/// posters from TMDb.
pub async fn requests_row(c: SeerrClient) -> Result<Vec<CardContent>, SeerrError> {
    let reqs = c.open_requests(REQUESTS_ROW).await?;
    let today = today();
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let handles: Vec<_> = reqs
        .into_iter()
        .filter_map(|r| {
            let info = r.media_with_request();
            let kind = r.kind.or(info.media_type)?;
            let id = info.tmdb_id?;
            let (c, sem) = (c.clone(), sem.clone());
            Some(tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let d = c.details(kind, id).await.ok()?;
                let state = request_status(&info, today).unwrap_or_else(|| "Requested".into());
                let who = info
                    .requests
                    .first()
                    .and_then(|r| r.requested_by.as_ref())
                    .and_then(|u| u.name().map(str::to_owned));
                Some(CardContent {
                    title: d.title.clone(),
                    subtitle: match who {
                        Some(w) => format!("{state} · {w}"),
                        None => state,
                    },
                    image: crate::discover::tmdb_image(d.poster_path.as_deref(), "w342"),
                    progress: info.download_status.first().and_then(DownloadItem::progress).map_or(-1.0, |p| p as f32),
                    played: false,
                    mark: 0,
                    target: Target::Open(Source::SeerrTitle { kind, id }),
                })
            }))
        })
        .collect();
    let mut cards = Vec::new();
    for h in handles {
        if let Ok(Some(card)) = h.await {
            cards.push(card);
        }
    }
    Ok(cards)
}

// ---- Release panel -------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ReleaseAction {
    Grab(Box<Release>),
    Season(u32),
    Episodes(u32),
    Episode { id: u32, label: String },
    Seasons,
}

struct ReleasePanel {
    arr: ArrClient,
    arr_id: u32,
    title: String,
    seasons: Vec<u32>,
    actions: Vec<ReleaseAction>,
}

fn size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", bytes as f64 / 1e6) }
}

/// "WEBDL-1080p · 4.2 GB · 3 days · 120 seeders · Indexer · English · +50"
fn release_detail(r: &Release) -> String {
    let mut parts = vec![r.quality_name().to_owned(), size(r.size)];
    parts.push(match r.age {
        0 => "today".into(),
        1 => "1 day".into(),
        n => format!("{n} days"),
    });
    match (r.protocol.as_str(), r.seeders) {
        ("torrent", Some(s)) => parts.push(format!("{s} seeders")),
        ("usenet", _) => parts.push("Usenet".into()),
        _ => {}
    }
    if !r.indexer.is_empty() {
        parts.push(r.indexer.clone());
    }
    let langs: Vec<&str> = r.languages.iter().map(|l| l.name.as_str()).filter(|n| !n.is_empty()).collect();
    if !langs.is_empty() {
        parts.push(langs.join("/"));
    }
    if r.custom_format_score != 0 {
        parts.push(format!("score {:+}", r.custom_format_score));
    }
    let mut s = parts.join(" · ");
    let reasons = r.rejection_reasons();
    if !reasons.is_empty() {
        s.push_str(&format!("\nRejected: {}", reasons.join("; ")));
    }
    s
}

impl App {
    fn show_release_panel(&self, title: &str, status: &str, entries: Vec<(crate::PanelItem, PanelAction)>) {
        let ui = self.ui();
        ui.set_panel_title(title.into());
        ui.set_panel_status(status.into());
        *self.panel.borrow_mut() = Some(Panel { kind: PanelKind::Releases, ..Default::default() });
        ui.set_panel_visible(true);
        self.show_panel_entries(entries);
    }

    /// Remembers the title page's Radarr/Sonarr facts for its buttons.
    pub fn set_title_arr(&self, kind: MediaKind, d: &Details, arr: ArrInfo) {
        let mut seasons: Vec<u32> = d.seasons.iter().map(|s| s.season_number).collect();
        // Specials last.
        seasons.sort_by_key(|&n| (n == 0, n));
        let info = d.media_info.clone().unwrap_or_default();
        *self.requests.title.borrow_mut() = Some(TitleArr {
            kind,
            id: d.id,
            title: d.title.clone(),
            seasons,
            arr,
            media_id: info.id,
            requests: info.requests.iter().map(|r| r.id).collect(),
            partial: info.status == status::PARTIALLY_AVAILABLE,
        });
    }

    /// The title page on top's Radarr/Sonarr facts.
    fn title_arr(&self) -> Option<(ArrClient, u32, TitleArr)> {
        let t = self.top_title()?;
        Some((t.arr.client.clone()?, t.arr.arr_id?, t))
    }

    /// What we know about the title page on top.
    fn top_title(&self) -> Option<TitleArr> {
        let Some(Source::SeerrTitle { kind, id }) = self.top_source() else { return None };
        self.requests.title.borrow().clone().filter(|t| t.kind == kind && t.id == id)
    }

    /// "Remove request" on a title page (second press confirms): cancels
    /// queued downloads, removes the title from Radarr/Sonarr (files kept)
    /// and clears it in Jellyseerr. Partly downloaded shows only lose
    /// their requests, so what's there stays in Sonarr.
    pub fn remove_request(&self) {
        let Some(t) = self.top_title() else { return };
        let (kind, id) = (t.kind, t.id);
        let armed = self.requests.remove_armed.take().is_some_and(|(k, i, at)| (k, i) == (kind, id) && at.elapsed() < CONFIRM);
        if !armed {
            self.requests.remove_armed.set(Some((kind, id, Instant::now())));
            let downloads = if t.arr.queue.is_empty() || t.partial { "" } else { " and cancel its download" };
            return self.toast(format!("Press again to remove the request for {}{downloads}", t.title));
        }
        self.toast("Removing the request…");
        let what = if kind == MediaKind::Movie { "Radarr" } else { "Sonarr" };
        let arr = if t.partial { None } else { t.arr.client.clone().zip(t.arr.arr_id) };
        let left_in_arr = !t.partial && arr.is_none() && t.arr.arr_id.is_some();
        let queue: Vec<u64> = t.arr.queue.iter().map(|q| q.id).collect();
        let media_id = if t.partial { None } else { t.media_id };
        let requests = t.requests;
        self.seerr_call(
            move |c| {
                let (arr, queue, requests) = (arr.clone(), queue.clone(), requests.clone());
                async move {
                    let mut problem = None;
                    if let Some((a, arr_id)) = &arr {
                        for q in &queue {
                            if let Err(e) = a.remove_download(*q, false).await {
                                tracing::warn!("removing download {q}: {e}");
                            }
                        }
                        match a.delete(*arr_id).await {
                            // Already gone (a retry after re-signing in).
                            Ok(()) | Err(rj_seerr::ArrError::Status { status: reqwest::StatusCode::NOT_FOUND, .. }) => {}
                            Err(e) => problem = Some(format!("couldn't remove it from {what}: {e}")),
                        }
                    }
                    let cleared = match media_id {
                        Some(m) => match c.delete_media(m).await {
                            Ok(()) => true,
                            // Not an admin: delete the requests one by one.
                            Err(SeerrError::Unauthorized) => false,
                            Err(e) => return Err(e),
                        },
                        None => false,
                    };
                    if !cleared {
                        for r in &requests {
                            c.delete_request(*r).await?;
                        }
                    }
                    Ok(problem)
                }
            },
            move |app, r| match r {
                Ok(problem) => {
                    app.toast(match problem {
                        Some(p) => format!("Request removed, but {p}"),
                        None if left_in_arr => format!("Request removed · it's still in {what} (needs a Jellyseerr admin to remove)"),
                        None => "Request removed".to_owned(),
                    });
                    app.open(Source::SeerrTitle { kind, id }, Placement::ReplaceTop);
                }
                Err(e) => app.toast(format!("Couldn't remove the request: {e:#}")),
            },
        );
    }

    /// "Manual search…" on a title page.
    pub fn manual_search(&self) {
        if let Some((arr, arr_id, t)) = self.title_arr() {
            self.open_manual_search(arr, arr_id, t.title, t.seasons);
        }
    }

    /// "Search again" / "Remove & search again" on a title page.
    pub fn search_again(&self) {
        if let Some((arr, arr_id, t)) = self.title_arr() {
            self.reattempt(arr, arr_id, t.arr.queue.iter().map(|q| q.id).collect());
        }
    }

    fn open_manual_search(&self, arr: ArrClient, arr_id: u32, title: String, seasons: Vec<u32>) {
        let movie = arr.kind == MediaKind::Movie;
        *self.requests.releases.borrow_mut() = Some(ReleasePanel { arr, arr_id, title, seasons, actions: Vec::new() });
        if movie {
            self.search_releases(SearchScope::Movie(arr_id), String::new());
        } else {
            self.show_seasons();
        }
    }

    fn show_seasons(&self) {
        let mut entries = vec![header("SEARCH A SEASON".into())];
        let mut actions = Vec::new();
        {
            let rp = self.requests.releases.borrow();
            let Some(p) = rp.as_ref() else { return };
            for &n in &p.seasons {
                entries.push(entry(
                    if n == 0 { "Specials".into() } else { format!("Season {n}") },
                    "Season packs and single episodes",
                    PanelAction::Release(actions.len()),
                ));
                actions.push(ReleaseAction::Season(n));
            }
        }
        let title = self.set_release_actions(actions);
        self.show_release_panel(&title, "Pick a season to search", entries);
    }

    /// Stores the panel's actions; returns the panel title.
    fn set_release_actions(&self, actions: Vec<ReleaseAction>) -> String {
        let mut rp = self.requests.releases.borrow_mut();
        let Some(p) = rp.as_mut() else { return String::new() };
        p.actions = actions;
        format!("Releases · {}", p.title)
    }

    fn search_releases(&self, scope: SearchScope, label: String) {
        let Some((arr, title)) = self.requests.releases.borrow().as_ref().map(|p| (p.arr.clone(), p.title.clone())) else { return };
        let what = if label.is_empty() { title.clone() } else { format!("{title} · {label}") };
        self.show_release_panel(
            &format!("Releases · {title}"),
            &format!("Searching indexers for {what}… this can take a minute"),
            Vec::new(),
        );
        self.spawn_ui(async move { arr.releases(scope).await }, move |app, r| {
            if !app.panel.borrow().as_ref().is_some_and(|p| p.kind == PanelKind::Releases) {
                return; // closed meanwhile
            }
            let releases = match r {
                Ok(r) => r,
                Err(e) => return app.show_release_panel(&format!("Releases · {title}"), &format!("Search failed: {e}"), Vec::new()),
            };
            let mut entries = Vec::new();
            let mut actions = Vec::new();
            if let SearchScope::Season(_, n) = scope {
                entries.push(entry("‹ Other seasons", "", PanelAction::Release(actions.len())));
                actions.push(ReleaseAction::Seasons);
                entries.push(entry(format!("Pick a single episode of season {n}…"), "", PanelAction::Release(actions.len())));
                actions.push(ReleaseAction::Episodes(n));
            }
            let ok = releases.iter().filter(|r| !r.rejected).count();
            entries.push(header(format!("{} RELEASES · {ok} ACCEPTABLE", releases.len())));
            for r in releases {
                let mut item = entry(r.title.clone(), release_detail(&r), PanelAction::Release(actions.len()));
                if r.rejected {
                    item.0.badge = "REJECTED".into();
                    item.0.dimmed = true;
                } else if r.full_season {
                    item.0.badge = "SEASON".into();
                }
                entries.push(item);
                actions.push(ReleaseAction::Grab(Box::new(r)));
            }
            let panel_title = app.set_release_actions(actions);
            app.show_release_panel(
                &panel_title,
                &format!("{what} · best first, as {} ranks them · Enter grabs", if scope_is_movie(scope) { "Radarr" } else { "Sonarr" }),
                entries,
            );
        });
    }

    fn show_episodes(&self, season: u32) {
        let Some((arr, series)) = self.requests.releases.borrow().as_ref().map(|p| (p.arr.clone(), p.arr_id)) else { return };
        self.spawn_ui(async move { arr.episodes(series).await }, move |app, r| {
            let eps = match r {
                Ok(e) => e,
                Err(e) => return app.toast(format!("Couldn't load episodes: {e}")),
            };
            let mut entries = vec![entry("‹ Other seasons", "", PanelAction::Release(0))];
            let mut actions = vec![ReleaseAction::Seasons];
            entries.push(header(format!("SEASON {season}")));
            for e in eps.iter().filter(|e| e.season_number == season) {
                let label = format!("S{:02}E{:02}", e.season_number, e.episode_number);
                entries.push(entry(
                    format!("{label} · {}", e.title),
                    if e.has_file { "In library" } else { "Missing" },
                    PanelAction::Release(actions.len()),
                ));
                actions.push(ReleaseAction::Episode { id: e.id, label });
            }
            let title = app.set_release_actions(actions);
            app.show_release_panel(&title, "Pick an episode to search", entries);
        });
    }

    pub fn release_action(&self, i: usize) {
        let (action, arr, arr_id) = {
            let rp = self.requests.releases.borrow();
            let Some(p) = rp.as_ref() else { return };
            let Some(a) = p.actions.get(i).cloned() else { return };
            (a, p.arr.clone(), p.arr_id)
        };
        match action {
            ReleaseAction::Seasons => self.show_seasons(),
            ReleaseAction::Season(n) => self.search_releases(SearchScope::Season(arr_id, n), format!("Season {n}")),
            ReleaseAction::Episodes(n) => self.show_episodes(n),
            ReleaseAction::Episode { id, label } => self.search_releases(SearchScope::Episode(id), label),
            ReleaseAction::Grab(r) => {
                self.toast(format!("Grabbing {}…", r.title));
                self.spawn_ui(async move { arr.grab(&r).await.map(|_| r.title.clone()) }, |app, res| match res {
                    Ok(t) => {
                        app.close_panel();
                        app.toast(format!("Sent to the download client: {t}"));
                        app.refresh_requests_soon();
                    }
                    Err(e) => app.toast(format!("Couldn't grab: {e}")),
                });
            }
        }
    }

    /// "Remove & search again": blocklists every queued download of the
    /// title and starts an automatic search. Needs a second press.
    fn reattempt(&self, arr: ArrClient, arr_id: u32, queue_ids: Vec<u64>) {
        let armed = self.requests.reattempt_armed.take().is_some_and(|(id, t)| id == arr_id && t.elapsed() < CONFIRM);
        if !armed && !queue_ids.is_empty() {
            self.requests.reattempt_armed.set(Some((arr_id, Instant::now())));
            return self.toast("Press again to remove the download, blocklist it and search for another");
        }
        self.toast(if queue_ids.is_empty() { "Searching again…" } else { "Removing and searching again…" });
        self.spawn_ui(
            async move {
                for id in queue_ids {
                    arr.remove_and_blocklist(id).await?;
                }
                arr.search(arr_id).await
            },
            |app, r| match r {
                Ok(()) => {
                    app.toast("Searching for another release");
                    app.refresh_requests_soon();
                }
                Err(e) => app.toast(format!("Couldn't remove: {e}")),
            },
        );
    }

    /// Refreshes Home / a title page in a few seconds (after the *arr and
    /// Jellyseerr have caught up).
    fn refresh_requests_soon(&self) {
        self.requests.next_refresh.set(Some(Instant::now() + Duration::from_secs(5)));
    }

    /// From the UI tick: keeps statuses on Home and title pages fresh.
    pub fn requests_tick(&self, now: Instant) {
        match self.requests.next_refresh.get() {
            Some(t) if now >= t => {}
            Some(_) => return,
            // Screens are fresh when opened.
            None => return self.requests.next_refresh.set(Some(now + REFRESH)),
        }
        self.requests.next_refresh.set(Some(now + REFRESH));
        if self.config.borrow().discover.seerr_url.is_none()
            || self.ui().get_screen() != crate::SCREEN_BROWSE
            || self.panel_open()
            || self.busy.get()
        {
            return;
        }
        if let Some(src @ (Source::Home | Source::SeerrTitle { .. })) = self.top_source() {
            self.open(src, Placement::Refresh);
        }
    }
}

fn scope_is_movie(scope: SearchScope) -> bool {
    matches!(scope, SearchScope::Movie(_))
}

/// Requests row first, Popular second (both optional) for Home.
pub fn home_rows(requests: Vec<CardContent>, popular: Vec<CardContent>) -> Vec<RowContent> {
    vec![row("Requests", STYLE_POSTER, requests), row("Popular & well rated", STYLE_POSTER, popular)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rj_seerr::models::RequestInfo;

    fn info(status: u8, req: Option<(u8, &str)>, downloads: Vec<DownloadItem>) -> MediaInfo {
        MediaInfo {
            status,
            requests: req
                .map(|(s, d)| RequestInfo { id: 1, status: s, created_at: Some(d.into()), requested_by: None })
                .into_iter()
                .collect(),
            download_status: downloads,
            ..Default::default()
        }
    }

    fn dl(status: &str, size: f64, left: f64, time: &str) -> DownloadItem {
        DownloadItem { status: status.into(), size, size_left: left, time_left: Some(time.into()), ..Default::default() }
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(days_from_civil("2024-02-29"), Some(19_782));
        assert_eq!(days_from_civil("2026-10-22T10:00:00.000Z"), Some(20_748));
    }

    #[test]
    fn times_left() {
        assert_eq!(time_left("00:12:34").as_deref(), Some("12 min"));
        assert_eq!(time_left("02:05:00").as_deref(), Some("2 h 5 min"));
        assert_eq!(time_left("1.03:00:00").as_deref(), Some("1 d 3 h"));
        assert_eq!(time_left("00:00:20").as_deref(), Some("less than a minute"));
    }

    #[test]
    fn statuses() {
        let today = 20_748; // 2026-10-22
        let s = |i: MediaInfo| request_status(&i, today);
        assert_eq!(s(info(status::AVAILABLE, Some((5, "2026-10-01")), vec![])), None);
        assert_eq!(s(info(0, None, vec![])), None);
        assert_eq!(
            s(info(status::PENDING, Some((request_status::PENDING_APPROVAL, "2026-10-21")), vec![])).as_deref(),
            Some("Waiting for approval · requested yesterday")
        );
        assert_eq!(
            s(info(status::PROCESSING, Some((request_status::APPROVED, "2026-10-22")), vec![])).as_deref(),
            Some("Searching for a release · requested today")
        );
        assert_eq!(
            s(info(status::PROCESSING, Some((request_status::APPROVED, "2026-10-17")), vec![])).as_deref(),
            Some("No release found yet · requested 5 days ago")
        );
        assert_eq!(
            s(info(status::PROCESSING, Some((2, "2026-10-17")), vec![dl("downloading", 100.0, 58.0, "00:12:00")])).as_deref(),
            Some("Downloading 42% · 12 min left")
        );
        // The worst entry wins; progress covers all of them.
        let two = vec![dl("downloading", 100.0, 0.0, "00:01:00"), dl("warning", 100.0, 100.0, "")];
        assert_eq!(s(info(status::PROCESSING, None, two)).as_deref(), Some("Stuck: the download has a warning"));
        assert_eq!(
            s(info(status::PROCESSING, Some((request_status::FAILED, "2026-10-17")), vec![])).as_deref(),
            Some("Request failed: Radarr/Sonarr couldn't add it")
        );
    }

    #[test]
    fn queue_headlines() {
        let q = |state: Option<&str>, status: &str, tracked: Option<&str>| QueueItem {
            id: 1,
            title: "Movie.2024.1080p".into(),
            status: status.into(),
            tracked_download_status: tracked.map(Into::into),
            tracked_download_state: state.map(Into::into),
            status_messages: vec![rj_seerr::arr::StatusMessage { title: "x".into(), messages: vec!["Not a Custom Format upgrade".into()] }],
            error_message: None,
            size: 10.0,
            sizeleft: 4.0,
            timeleft: Some("00:30:00".into()),
            download_client: Some("qBittorrent".into()),
            indexer: None,
            episode_id: None,
            season_number: None,
        };
        let (h, d) = queue_lines(&q(Some("importBlocked"), "completed", Some("warning")));
        assert_eq!(h, "Import blocked");
        assert_eq!(d, "Not a Custom Format upgrade · qBittorrent");
        assert_eq!(queue_lines(&q(Some("downloading"), "downloading", Some("ok"))).0, "Downloading 60% · 30 min left");
        assert_eq!(queue_lines(&q(Some("downloading"), "downloading", Some("warning"))).0, "Stuck at 60%");
    }

    #[test]
    fn release_details() {
        let r: Release = serde_json::from_value(serde_json::json!({
            "guid": "g", "indexerId": 1, "title": "Movie.2024.1080p.WEB", "size": 4_200_000_000u64, "age": 3,
            "seeders": 120, "protocol": "torrent", "indexer": "Nyaa", "quality": { "quality": { "name": "WEBDL-1080p" } },
            "languages": [{ "name": "English" }], "customFormatScore": 50,
            "rejected": true, "rejections": ["Not an upgrade"]
        }))
        .unwrap();
        assert_eq!(
            release_detail(&r),
            "WEBDL-1080p · 4.2 GB · 3 days · 120 seeders · Nyaa · English · score +50\nRejected: Not an upgrade"
        );
    }
}
