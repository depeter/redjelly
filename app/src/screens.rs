//! Content for every browse-type screen, loaded off the UI thread. Results
//! are plain `Send` data; `nav.rs` turns them into Slint models.

use rj_jellyfin::Client;
use rj_jellyfin::models::{ImageRef, Item, StreamType, TICKS_PER_SECOND, User};

use crate::images::ImageSrc;
use crate::layout::{STYLE_BUTTON, STYLE_POSTER, STYLE_WIDE};
use crate::playback::format_time;

/// What a screen shows; also how to reload it.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Users,
    Home,
    Library { parent_id: String, title: String, collection_type: Option<String> },
    Series { id: String },
    Detail { id: String },
    Search { term: String },
    /// AI subtitle jobs (built locally, not loaded from the server).
    Jobs,
    /// Built locally too.
    Settings,
    /// Discover wizard step (built locally, see `discover.rs`).
    Discover { step: u8 },
    /// Discover results; the filters live in the config.
    DiscoverResults,
    /// A TMDb title via Jellyseerr, in the library or not.
    SeerrTitle { kind: rj_seerr::MediaKind, id: u32 },
    /// Choose seasons of a show to request (built locally).
    SeasonPicker { id: u32 },
}

/// What activating a card does.
#[derive(Debug, Clone)]
pub enum Target {
    None,
    Open(Source),
    Play { id: String, from_start: bool },
    SetPlayed { id: String, played: bool },
    User { id: String, name: String, has_password: bool },
    QuickConnect,
    ManualLogin,
    ChangeServer,
    /// Queue AI subtitles for one item / every episode of a series.
    AiItem { id: String },
    AiSeries { id: String },
    Job(u64),
    Setting(crate::settings::Setting),
    SwitchUser,
    Quit,
    /// Asks for a second press, then powers the computer off.
    Shutdown,
    Discover(crate::discover::DiscoverAction),
}

#[derive(Debug, Clone)]
pub struct CardContent {
    pub title: String,
    pub subtitle: String,
    pub image: Option<ImageSrc>,
    pub progress: f32,
    pub played: bool,
    /// Setting cards: 0 = plain, 1 = selected / included, 2 = excluded.
    pub mark: i32,
    pub target: Target,
}

#[derive(Debug, Clone)]
pub struct RowContent {
    pub title: String,
    pub style: i32,
    pub cards: Vec<CardContent>,
    /// Column focus lands on when entering this row.
    pub start: usize,
    /// How many trailing cards sit apart at the right edge, as compact buttons.
    pub pinned: usize,
}

#[derive(Debug, Clone, Default)]
pub struct HeaderContent {
    pub title: String,
    pub meta: String,
    pub info: String,
    pub overview: String,
    pub backdrop: Option<ImageSrc>,
    /// TMDb title whose IMDb/RT ratings get appended to `meta` once fetched.
    pub ratings_for: Option<(rj_seerr::MediaKind, u32)>,
}

/// Where more grid items come from.
#[derive(Debug, Clone)]
pub enum PageSource {
    /// Jellyfin `Items` query.
    Library(Vec<(String, String)>),
    Discover(Box<crate::discover::Cursor>),
}

/// More grid items to fetch as the user scrolls.
#[derive(Debug, Clone)]
pub struct Paging {
    pub source: PageSource,
    pub loaded: u32,
    pub total: u32,
    pub columns: usize,
}

#[derive(Debug, Clone)]
pub struct Content {
    pub source: Source,
    pub header: Option<HeaderContent>,
    pub rows: Vec<RowContent>,
    pub focus: (usize, usize),
    pub paging: Option<Paging>,
}

const PAGE: u32 = 120;

fn image(c: &Client, r: Option<ImageRef>, width: u32) -> Option<ImageSrc> {
    let r = r?;
    let url = c.image_url(&r.item_id, r.kind, &r.tag, width).ok()?;
    Some(ImageSrc { url: url.to_string(), key: format!("{}-{}-{}-{width}", r.item_id, r.kind, r.tag) })
}

fn episode_code(item: &Item) -> Option<String> {
    Some(format!("S{:02}E{:02}", item.parent_index_number?, item.index_number?))
}

/// Where activating an item leads.
pub fn item_target(item: &Item) -> Target {
    match item.kind.as_str() {
        "Series" => Target::Open(Source::Series { id: item.id.clone() }),
        "Season" => match &item.series_id {
            Some(id) => Target::Open(Source::Series { id: id.clone() }),
            None => Target::None,
        },
        "Movie" | "Episode" | "Video" | "MusicVideo" | "Trailer" => Target::Open(Source::Detail { id: item.id.clone() }),
        _ if item.is_folder => Target::Open(Source::Library {
            parent_id: item.id.clone(),
            title: item.name.clone(),
            collection_type: item.collection_type.clone(),
        }),
        _ => Target::Open(Source::Detail { id: item.id.clone() }),
    }
}

/// `in_series`: the screen already names the series, so don't repeat it.
pub fn item_card(c: &Client, item: &Item, style: i32, in_series: bool) -> CardContent {
    let subtitle = match (item.kind.as_str(), &item.series_name, episode_code(item)) {
        ("Episode", _, Some(code)) if in_series => {
            let mins = item.run_time_ticks.map(|t| t / TICKS_PER_SECOND / 60).unwrap_or(0);
            if mins > 0 { format!("{code} · {mins} min") } else { code }
        }
        ("Episode", Some(series), Some(code)) => format!("{series} · {code}"),
        ("Series", _, _) => item.production_year.map(|y| y.to_string()).unwrap_or_default(),
        _ => item.production_year.map(|y| y.to_string()).unwrap_or_default(),
    };
    let img = if style == STYLE_WIDE { image(c, item.landscape(), 576) } else { image(c, item.poster(), 320) };
    CardContent {
        title: item.name.clone(),
        subtitle,
        image: img,
        progress: item.progress().map_or(-1.0, |p| p as f32),
        played: item.user_data.as_ref().is_some_and(|u| u.played),
        mark: 0,
        target: item_target(item),
    }
}

pub fn button(title: impl Into<String>, target: Target) -> CardContent {
    CardContent { title: title.into(), subtitle: String::new(), image: None, progress: -1.0, played: false, mark: 0, target }
}

pub fn row(title: impl Into<String>, style: i32, cards: Vec<CardContent>) -> RowContent {
    RowContent { title: title.into(), style, cards, start: 0, pinned: 0 }
}

fn tmdb_id(item: &Item) -> Option<u32> {
    item.provider_ids.iter().find(|(k, _)| k.eq_ignore_ascii_case("tmdb")).and_then(|(_, v)| v.parse().ok())
}

fn runtime(item: &Item) -> Option<String> {
    let mins = item.run_time_ticks? / TICKS_PER_SECOND / 60;
    Some(if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins} min") })
}

fn meta_line(item: &Item) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(y) = item.production_year {
        parts.push(y.to_string());
    }
    if let Some(r) = runtime(item).filter(|_| item.kind != "Series") {
        parts.push(r);
    }
    if let Some(r) = &item.official_rating {
        parts.push(r.clone());
    }
    if let Some(r) = item.community_rating {
        parts.push(format!("★ {r:.1}"));
    }
    if !item.genres.is_empty() {
        parts.push(item.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    parts.join(" · ")
}

/// "Audio: eng 5.1, dut 2.0 · Subtitles: eng, dut" for the detail header.
fn media_line(item: &Item, preferred: &[rj_core::lang::Lang]) -> String {
    let Some(ms) = item.media_sources.first() else { return String::new() };
    let lang = |s: &rj_jellyfin::models::MediaStream| {
        s.language.as_deref().and_then(rj_core::lang::Lang::parse).map_or("?".into(), |l| l.to_string())
    };
    let mut audio: Vec<String> = Vec::new();
    let mut subs: Vec<String> = Vec::new();
    let mut video = String::new();
    for s in &ms.media_streams {
        match s.kind {
            StreamType::Video if video.is_empty() => {
                video = s.display_title.clone().unwrap_or_default();
            }
            StreamType::Audio => {
                let ch = match s.channels {
                    Some(6) => " 5.1",
                    Some(8) => " 7.1",
                    Some(2) => " 2.0",
                    _ => "",
                };
                audio.push(format!("{}{ch}", lang(s)));
            }
            StreamType::Subtitle => {
                let l = lang(s);
                if !subs.contains(&l) {
                    subs.push(l);
                }
            }
            _ => {}
        }
    }
    let mut parts = vec![];
    if !video.is_empty() {
        parts.push(video);
    }
    if !audio.is_empty() {
        parts.push(format!("Audio: {}", audio.join(", ")));
    }
    parts.push(if subs.is_empty() { "No subtitles".into() } else { format!("Subtitles: {}", subs.join(", ")) });
    if let Some(top) = preferred.first()
        && !subs.iter().any(|s| s == top.as_str()) {
            parts.push(format!("no '{top}' subtitles"));
        }
    parts.join(" · ")
}

pub async fn users(c: &Client, server_url: &str) -> anyhow::Result<Content> {
    let users = c.public_users().await?;
    let qc = c.quick_connect_enabled().await.unwrap_or(false);
    let user_card = |u: &User| CardContent {
        title: u.name.clone(),
        subtitle: String::new(),
        image: u.primary_image_tag.as_ref().and_then(|tag| {
            let url = c.url(&format!("UserImage?userId={}&tag={tag}&format=Jpg", u.id)).ok()?;
            Some(ImageSrc { url: url.to_string(), key: format!("user-{}-{tag}", u.id) })
        }),
        progress: -1.0,
        played: false,
        mark: 0,
        target: Target::User { id: u.id.clone(), name: u.name.clone(), has_password: u.has_password },
    };
    let mut rows = Vec::new();
    if !users.is_empty() {
        rows.push(row("", STYLE_POSTER, users.iter().map(user_card).collect()));
    }
    let mut buttons = Vec::new();
    if qc {
        buttons.push(button("Quick Connect", Target::QuickConnect));
    }
    buttons.push(button("Other user", Target::ManualLogin));
    buttons.push(button("Change server", Target::ChangeServer));
    rows.push(row("", STYLE_BUTTON, buttons));
    Ok(Content {
        source: Source::Users,
        header: Some(HeaderContent { title: "Who's watching?".into(), meta: server_url.into(), ..Default::default() }),
        rows,
        focus: (0, 0),
        paging: None,
    })
}

/// `discover`: Jellyseerr is set up, so offer the Discover wizard.
pub async fn home(c: &Client, discover: bool) -> anyhow::Result<Content> {
    let (resume, next_up, views) = tokio::try_join!(c.resume(24), c.next_up(24), c.user_views())?;
    let mut rows = vec![
        row("Continue watching", STYLE_WIDE, resume.items.iter().map(|i| item_card(c, i, STYLE_WIDE, false)).collect()),
        row("Next up", STYLE_WIDE, next_up.items.iter().map(|i| item_card(c, i, STYLE_WIDE, false)).collect()),
        row(
            "Libraries",
            STYLE_WIDE,
            views
                .items
                .iter()
                .map(|v| {
                    let mut card = item_card(c, v, STYLE_WIDE, false);
                    card.image = image(
                        c,
                        v.image_tags.get("Primary").map(|t| ImageRef { item_id: v.id.clone(), kind: "Primary", tag: t.clone() }),
                        576,
                    );
                    card.subtitle = String::new();
                    card
                })
                .collect(),
        ),
    ];
    // Fetch every library's "latest" row concurrently.
    let pending: Vec<_> = views
        .items
        .iter()
        .map(|v| {
            let (c, id) = (c.clone(), v.id.clone());
            tokio::spawn(async move { c.latest(&id, 20).await })
        })
        .collect();
    for (view, handle) in views.items.iter().zip(pending) {
        let items = handle.await??;
        rows.push(row(
            format!("Latest in {}", view.name),
            STYLE_POSTER,
            items.iter().map(|i| item_card(c, i, STYLE_POSTER, false)).collect(),
        ));
    }
    rows.retain(|r| !r.cards.is_empty());
    // The same buttons on top (press Up from the first row) and at the
    // bottom; focus still starts on the first content row.
    let toolbar = || {
        let mut b = vec![button("Search · or just type", Target::Open(Source::Search { term: String::new() }))];
        if discover {
            b.push(button("Discover", Target::Open(Source::Discover { step: 0 })));
        }
        b.extend([
            button("Settings", Target::Open(Source::Settings)),
            button("AI subtitle jobs", Target::Open(Source::Jobs)),
            button("Switch user", Target::SwitchUser),
        ]);
        b
    };
    rows.push(row("More", STYLE_BUTTON, toolbar()));
    let mut top = row("", STYLE_BUTTON, toolbar());
    top.cards.push(button("Quit", Target::Quit));
    top.cards.push(button("Shut down", Target::Shutdown));
    top.pinned = 2;
    rows.insert(0, top);
    let focus = if rows.len() > 1 { (1, 0) } else { (0, 0) };
    Ok(Content { source: Source::Home, header: None, rows, focus, paging: None })
}

fn library_query(parent_id: &str, collection_type: Option<&str>) -> Vec<(String, String)> {
    let mut q = vec![
        ("parentId".to_owned(), parent_id.to_owned()),
        ("sortBy".to_owned(), "SortName".to_owned()),
        ("sortOrder".to_owned(), "Ascending".to_owned()),
    ];
    let types = match collection_type {
        Some("movies") => Some("Movie"),
        Some("tvshows") => Some("Series"),
        Some("boxsets") => Some("BoxSet"),
        Some("musicvideos") => Some("MusicVideo"),
        Some("homevideos") => Some("Video"),
        _ => None,
    };
    if let Some(t) = types {
        q.push(("includeItemTypes".into(), t.into()));
        q.push(("recursive".into(), "true".into()));
    }
    q
}

pub async fn library_page(c: &Client, paging: &Paging) -> anyhow::Result<(Vec<CardContent>, u32)> {
    let start = paging.loaded.to_string();
    let limit = PAGE.to_string();
    let PageSource::Library(query) = &paging.source else { anyhow::bail!("not a library grid") };
    let mut q: Vec<(&str, &str)> = query.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    q.push(("startIndex", &start));
    q.push(("limit", &limit));
    let res = c.items(&q).await?;
    Ok((res.items.iter().map(|i| item_card(c, i, STYLE_POSTER, false)).collect(), res.total_record_count))
}

/// Splits cards into grid rows of `columns`.
pub fn grid_rows(cards: Vec<CardContent>, columns: usize) -> Vec<RowContent> {
    let mut rows = Vec::new();
    let mut it = cards.into_iter().peekable();
    while it.peek().is_some() {
        rows.push(row("", STYLE_POSTER, it.by_ref().take(columns).collect()));
    }
    rows
}

pub async fn library(
    c: &Client,
    parent_id: &str,
    title: &str,
    collection_type: Option<&str>,
    columns: usize,
) -> anyhow::Result<Content> {
    let mut paging =
        Paging { source: PageSource::Library(library_query(parent_id, collection_type)), loaded: 0, total: 0, columns };
    let (cards, total) = library_page(c, &paging).await?;
    paging.loaded = cards.len() as u32;
    paging.total = total;
    Ok(Content {
        source: Source::Library {
            parent_id: parent_id.into(),
            title: title.into(),
            collection_type: collection_type.map(Into::into),
        },
        header: Some(HeaderContent { title: title.into(), meta: format!("{total} items"), ..Default::default() }),
        rows: grid_rows(cards, columns),
        focus: (0, 0),
        paging: (paging.loaded < total).then_some(paging),
    })
}

pub async fn series(c: &Client, id: &str) -> anyhow::Result<Content> {
    let (series, episodes, next) = tokio::try_join!(c.item(id), c.episodes(id), c.next_up_for_series(id))?;

    let mut rows = Vec::new();
    let mut buttons = Vec::new();
    if let Some(ep) = next.items.first() {
        let code = episode_code(ep).unwrap_or_default();
        let label = match ep.progress() {
            Some(_) => format!("Resume {code}"),
            None => format!("Play {code}"),
        };
        buttons.push(button(label, Target::Play { id: ep.id.clone(), from_start: false }));
        buttons.push(button(format!("Open {code}"), Target::Open(Source::Detail { id: ep.id.clone() })));
    }
    let all_played = series.user_data.as_ref().is_some_and(|u| u.played);
    buttons.push(button(
        if all_played { "Mark unwatched" } else { "Mark watched" },
        Target::SetPlayed { id: id.into(), played: !all_played },
    ));
    buttons.push(button("AI subtitles: all episodes", Target::AiSeries { id: id.into() }));
    rows.push(row("", STYLE_BUTTON, buttons));

    // Group episodes by season, keeping server order.
    let mut seasons: Vec<(String, Vec<&Item>)> = Vec::new();
    for ep in &episodes.items {
        let name = ep
            .season_name
            .clone()
            .unwrap_or_else(|| ep.parent_index_number.map_or("Specials".into(), |n| format!("Season {n}")));
        match seasons.last_mut() {
            Some((n, eps)) if *n == name => eps.push(ep),
            _ => seasons.push((name, vec![ep])),
        }
    }
    let next_id = next.items.first().map(|e| e.id.as_str());
    for (name, eps) in &seasons {
        let mut r = row(name.clone(), STYLE_WIDE, eps.iter().map(|e| item_card(c, e, STYLE_WIDE, true)).collect());
        // Seasons open scrolled to the next episode to watch.
        r.start = eps.iter().position(|e| Some(e.id.as_str()) == next_id).unwrap_or(0);
        rows.push(r);
    }

    let mut meta = meta_line(&series);
    if !seasons.is_empty() {
        let n = seasons.len();
        meta = format!("{meta} · {n} season{}", if n == 1 { "" } else { "s" });
    }
    Ok(Content {
        source: Source::Series { id: id.into() },
        header: Some(HeaderContent {
            title: series.name.clone(),
            meta,
            info: String::new(),
            overview: series.overview.clone().unwrap_or_default(),
            backdrop: image(c, series.backdrop(), 1280),
            ratings_for: tmdb_id(&series).map(|id| (rj_seerr::MediaKind::Tv, id)),
        }),
        rows,
        focus: (0, 0),
        paging: None,
    })
}

pub async fn detail(c: &Client, id: &str, preferred: &[rj_core::lang::Lang]) -> anyhow::Result<Content> {
    let item = c.item(id).await?;
    let mut buttons = Vec::new();
    match item.user_data.as_ref().map(|u| u.playback_position_ticks).filter(|t| *t > 0) {
        Some(ticks) => {
            let at = format_time(ticks as f64 / TICKS_PER_SECOND as f64);
            buttons.push(button(format!("Resume {at}"), Target::Play { id: id.into(), from_start: false }));
            buttons.push(button("Play from start", Target::Play { id: id.into(), from_start: true }));
        }
        None => buttons.push(button("Play", Target::Play { id: id.into(), from_start: true })),
    }
    let played = item.user_data.as_ref().is_some_and(|u| u.played);
    buttons.push(button(
        if played { "Mark unwatched" } else { "Mark watched" },
        Target::SetPlayed { id: id.into(), played: !played },
    ));
    if let Some(top) = preferred.first() {
        let has_top = item.media_sources.first().is_some_and(|ms| {
            ms.media_streams.iter().any(|s| {
                s.kind == StreamType::Subtitle
                    && s.is_text_subtitle_stream
                    && s.language.as_deref().and_then(rj_core::lang::Lang::parse).as_ref() == Some(top)
            })
        });
        if !has_top {
            buttons.push(button(format!("AI {} subtitles", top.name()), Target::AiItem { id: id.into() }));
        }
    }
    if let (Some(series_id), Some(name)) = (&item.series_id, &item.series_name) {
        buttons.push(button(name.clone(), Target::Open(Source::Series { id: series_id.clone() })));
    }
    let mut rows = vec![row("", STYLE_BUTTON, buttons)];

    // Episodes: the rest of the season, scrolled to this one.
    if let (Some(series_id), Some(season)) = (&item.series_id, item.parent_index_number) {
        let eps = c.episodes(series_id).await?;
        let season_eps: Vec<&Item> = eps.items.iter().filter(|e| e.parent_index_number == Some(season)).collect();
        if !season_eps.is_empty() {
            let mut r = row(
                item.season_name.clone().unwrap_or_else(|| format!("Season {season}")),
                STYLE_WIDE,
                season_eps.iter().map(|e| item_card(c, e, STYLE_WIDE, true)).collect(),
            );
            r.start = season_eps.iter().position(|e| e.id == item.id).unwrap_or(0);
            rows.push(r);
        }
    }

    let title = match (&item.series_name, episode_code(&item)) {
        (Some(_), Some(code)) => format!("{code} · {}", item.name),
        _ => item.name.clone(),
    };
    let meta = match &item.series_name {
        Some(series) => format!("{series} · {}", meta_line(&item)),
        None => meta_line(&item),
    };
    Ok(Content {
        source: Source::Detail { id: id.into() },
        header: Some(HeaderContent {
            title,
            meta,
            info: media_line(&item, preferred),
            overview: item.overview.clone().unwrap_or_default(),
            backdrop: image(c, item.backdrop(), 1280),
            ratings_for: tmdb_id(&item).filter(|_| item.kind == "Movie").map(|id| (rj_seerr::MediaKind::Movie, id)),
        }),
        rows,
        focus: (0, 0),
        paging: None,
    })
}

pub async fn search(c: &Client, term: &str) -> anyhow::Result<Content> {
    let header = HeaderContent {
        title: "Search".into(),
        meta: format!("{term}_"),
        info: if term.is_empty() { "Type to search movies, shows and episodes".into() } else { String::new() },
        ..Default::default()
    };
    let mut rows = Vec::new();
    if !term.trim().is_empty() {
        let res = c.search(term, 60).await?;
        let of = |kind: &'static str| res.items.iter().filter(move |i| i.kind == kind);
        let movies: Vec<_> = of("Movie").map(|i| item_card(c, i, STYLE_POSTER, false)).collect();
        let series: Vec<_> = of("Series").map(|i| item_card(c, i, STYLE_POSTER, false)).collect();
        let episodes: Vec<_> = of("Episode").map(|i| item_card(c, i, STYLE_WIDE, false)).collect();
        for (title, style, cards) in [("Movies", STYLE_POSTER, movies), ("Shows", STYLE_POSTER, series), ("Episodes", STYLE_WIDE, episodes)] {
            if !cards.is_empty() {
                rows.push(row(title, style, cards));
            }
        }
    }
    let mut header = header;
    if !term.trim().is_empty() && rows.is_empty() {
        header.info = "Nothing found".into();
    }
    Ok(Content { source: Source::Search { term: term.into() }, header: Some(header), rows, focus: (0, 0), paging: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_splits_into_columns() {
        let cards = (0..7).map(|i| button(i.to_string(), Target::None)).collect();
        let rows = grid_rows(cards, 3);
        assert_eq!(rows.iter().map(|r| r.cards.len()).collect::<Vec<_>>(), [3, 3, 1]);
    }

    #[test]
    fn library_query_by_collection_type() {
        let q = library_query("p", Some("tvshows"));
        assert!(q.contains(&("includeItemTypes".into(), "Series".into())));
        assert!(!library_query("p", None).iter().any(|(k, _)| k == "recursive"));
    }
}
