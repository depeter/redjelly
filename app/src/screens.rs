//! Content for every browse-type screen, loaded off the UI thread. Results
//! are plain `Send` data; `nav.rs` turns them into Slint models.

use rj_jellyfin::Client;
use rj_jellyfin::models::{ImageRef, Item, StreamType, TICKS_PER_SECOND, User};

use crate::images::ImageSrc;
use crate::layout::{STYLE_BUTTON, STYLE_POSTER, STYLE_SETTING, STYLE_WIDE};
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
    /// Library items featuring a cast or crew member.
    Person { id: String, name: String },
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
    /// Server trailer file when `local`, else the web `remote` URL.
    Trailer { item_id: String, local: bool, remote: Option<String>, title: String },
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

/// "15 Jul 2010" from Jellyfin's ISO dates.
fn format_date(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, m, d) = (iso.get(..4)?, iso.get(5..7)?.parse::<usize>().ok()?, iso.get(8..10)?.parse::<u32>().ok()?);
    Some(format!("{d} {} {y}", MONTHS.get(m.checked_sub(1)?)?))
}

fn format_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", bytes as f64 / 1e6) }
}

fn stream_lang(s: &rj_jellyfin::models::MediaStream) -> String {
    s.language.as_deref().and_then(rj_core::lang::Lang::parse).map_or("Unknown language".into(), |l| l.name())
}

/// Read-only label + value card.
fn info_card(label: impl Into<String>, value: impl Into<String>) -> CardContent {
    CardContent {
        title: label.into(),
        subtitle: value.into(),
        image: None,
        progress: -1.0,
        played: false,
        mark: 0,
        target: Target::None,
    }
}

/// One card per video, audio and subtitle track, plus the file; and a
/// note when the top preferred subtitle language is missing.
fn media_cards(item: &Item, preferred: &[rj_core::lang::Lang]) -> (Vec<CardContent>, Option<String>) {
    let Some(ms) = item.media_sources.first() else { return (Vec::new(), None) };
    let mut cards = Vec::new();
    let mut sub_langs = Vec::new();
    for s in &ms.media_streams {
        let detail = s.display_title.clone().or_else(|| s.codec.clone()).unwrap_or_default();
        let flags = |s: &rj_jellyfin::models::MediaStream| {
            let mut f = Vec::new();
            if s.is_default {
                f.push("default");
            }
            if s.is_forced {
                f.push("forced");
            }
            if s.is_hearing_impaired {
                f.push("SDH");
            }
            if s.is_external {
                f.push("external");
            }
            if f.is_empty() { String::new() } else { format!(" · {}", f.join(", ")) }
        };
        match s.kind {
            StreamType::Video => cards.push(info_card("Video", detail)),
            StreamType::Audio => cards.push(info_card(format!("Audio · {}", stream_lang(s)), format!("{detail}{}", flags(s)))),
            StreamType::Subtitle => {
                if let Some(l) = s.language.as_deref().and_then(rj_core::lang::Lang::parse) {
                    sub_langs.push(l);
                }
                cards.push(info_card(format!("Subtitles · {}", stream_lang(s)), format!("{detail}{}", flags(s))));
            }
            _ => {}
        }
    }
    let mut file = Vec::new();
    if let Some(c) = &ms.container {
        file.push(c.to_uppercase());
    }
    if let Some(size) = ms.size {
        file.push(format_size(size));
    }
    if let Some(name) = ms.path.as_deref().and_then(|p| p.rsplit(['/', '\\']).next()) {
        file.push(name.to_owned());
    }
    if !file.is_empty() {
        cards.push(info_card("File", file.join(" · ")));
    }
    let missing = preferred
        .first()
        .filter(|top| !sub_langs.contains(top))
        .map(|top| format!("no {} subtitles", top.name()));
    (cards, missing)
}

/// Facts for the About row, skipping unknowns.
fn about_cards(item: &Item) -> Vec<CardContent> {
    let mut cards = Vec::new();
    if let Some(t) = item.original_title.as_ref().filter(|t| **t != item.name) {
        cards.push(info_card("Original title", t.clone()));
    }
    if let Some(d) = item.premiere_date.as_deref().and_then(format_date) {
        cards.push(info_card(if item.kind == "Episode" { "Aired" } else { "Released" }, d));
    }
    if let Some(r) = runtime(item) {
        cards.push(info_card("Runtime", r));
    }
    if let Some(r) = &item.official_rating {
        cards.push(info_card("Age rating", r.clone()));
    }
    if let Some(r) = item.community_rating {
        cards.push(info_card("Community rating", format!("★ {r:.1}")));
    }
    if let Some(r) = item.critic_rating {
        cards.push(info_card("Critics", format!("{r:.0}%")));
    }
    if !item.genres.is_empty() {
        cards.push(info_card("Genres", item.genres.join(", ")));
    }
    let names = |kind: &str| -> Vec<String> {
        let mut v: Vec<String> = item.people.iter().filter(|p| p.kind == kind).map(|p| p.name.clone()).collect();
        v.dedup();
        v
    };
    for (kind, one, many) in [("Director", "Director", "Directors"), ("Writer", "Writer", "Writers"), ("Producer", "Producer", "Producers")] {
        let n = names(kind);
        if !n.is_empty() {
            cards.push(info_card(if n.len() == 1 { one } else { many }, n.join(", ")));
        }
    }
    if !item.studios.is_empty() {
        cards.push(info_card("Studio", item.studios.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if !item.production_locations.is_empty() {
        cards.push(info_card("Country", item.production_locations.join(", ")));
    }
    cards
}

/// Cast first, then crew; each opens what else they're in.
fn people_cards(c: &Client, item: &Item) -> Vec<CardContent> {
    let rank = |kind: &str| match kind {
        "Actor" => 0,
        "GuestStar" => 1,
        "Director" => 2,
        "Writer" => 3,
        _ => 4,
    };
    let mut people: Vec<&rj_jellyfin::models::Person> = item.people.iter().collect();
    people.sort_by_key(|p| rank(&p.kind));
    people
        .into_iter()
        .take(40)
        .map(|p| CardContent {
            title: p.name.clone(),
            subtitle: match p.role.as_deref().filter(|r| !r.is_empty()) {
                Some(r) if p.kind == "Actor" || p.kind == "GuestStar" => r.to_owned(),
                Some(r) => format!("{} · {r}", p.kind),
                None => p.kind.clone(),
            },
            image: image(
                c,
                p.primary_image_tag.as_ref().map(|t| ImageRef { item_id: p.id.clone(), kind: "Primary", tag: t.clone() }),
                320,
            ),
            progress: -1.0,
            played: false,
            mark: 0,
            target: Target::Open(Source::Person { id: p.id.clone(), name: p.name.clone() }),
        })
        .collect()
}

/// The trailer to offer: a server file first, else a web (YouTube) one.
fn trailer_target(item: &Item) -> Option<Target> {
    let remote = item
        .remote_trailers
        .iter()
        .find(|t| t.url.contains("youtu"))
        .or(item.remote_trailers.first())
        .map(|t| t.url.clone());
    let local = item.local_trailer_count.unwrap_or(0) > 0;
    (local || remote.is_some()).then(|| Target::Trailer { item_id: item.id.clone(), local, remote, title: item.name.clone() })
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
    if let Some(t) = trailer_target(&item) {
        buttons.push(button("Trailer", t));
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

    let about = about_cards(&item);
    if !about.is_empty() {
        rows.push(row("About", STYLE_SETTING, about));
    }
    let people = people_cards(c, &item);
    if !people.is_empty() {
        rows.push(row("Cast & crew", STYLE_POSTER, people));
    }
    let (media, missing) = media_cards(&item, preferred);
    if !media.is_empty() {
        let title = match missing {
            Some(m) => format!("Video & audio · {m}"),
            None => "Video & audio".into(),
        };
        rows.push(row(title, STYLE_SETTING, media));
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
            info: item.taglines.first().cloned().unwrap_or_default(),
            overview: item.overview.clone().unwrap_or_default(),
            backdrop: image(c, item.backdrop(), 1280),
            ratings_for: tmdb_id(&item).filter(|_| item.kind == "Movie").map(|id| (rj_seerr::MediaKind::Movie, id)),
        }),
        rows,
        focus: (0, 0),
        paging: None,
    })
}

/// Everything in the library with this person, newest first.
pub async fn person(c: &Client, id: &str, name: &str, columns: usize) -> anyhow::Result<Content> {
    let query: Vec<(String, String)> = [
        ("personIds", id),
        ("recursive", "true"),
        ("includeItemTypes", "Movie,Series"),
        ("sortBy", "ProductionYear,SortName"),
        ("sortOrder", "Descending"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let mut paging = Paging { source: PageSource::Library(query), loaded: 0, total: 0, columns };
    let (person, page) = tokio::join!(c.item(id), library_page(c, &paging));
    let (cards, total) = page?;
    paging.loaded = cards.len() as u32;
    paging.total = total;
    Ok(Content {
        source: Source::Person { id: id.into(), name: name.into() },
        header: Some(HeaderContent {
            title: name.into(),
            meta: format!("{total} in your library"),
            overview: person.ok().and_then(|p| p.overview).unwrap_or_default(),
            ..Default::default()
        }),
        rows: grid_rows(cards, columns),
        focus: (0, 0),
        paging: (paging.loaded < total).then_some(paging),
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

    fn movie(json: serde_json::Value) -> Item {
        let mut base = serde_json::json!({ "Id": "m1", "Name": "Inception", "Type": "Movie" });
        base.as_object_mut().unwrap().extend(json.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn dates_and_sizes() {
        assert_eq!(format_date("2010-07-15T00:00:00.0000000Z").as_deref(), Some("15 Jul 2010"));
        assert_eq!(format_date("bad"), None);
        assert_eq!(format_size(8_400_000_000), "8.4 GB");
        assert_eq!(format_size(700_000_000), "700 MB");
    }

    #[test]
    fn about_lists_crew_and_facts() {
        let item = movie(serde_json::json!({
            "OriginalTitle": "Inception",
            "PremiereDate": "2010-07-15T00:00:00Z",
            "CriticRating": 87.0,
            "Studios": [{ "Name": "Legendary", "Id": "s" }],
            "People": [
                { "Id": "p1", "Name": "Christopher Nolan", "Type": "Director" },
                { "Id": "p1", "Name": "Christopher Nolan", "Type": "Writer" },
                { "Id": "p2", "Name": "Leonardo DiCaprio", "Type": "Actor", "Role": "Cobb", "PrimaryImageTag": "t" }
            ]
        }));
        let about: Vec<(String, String)> = about_cards(&item).into_iter().map(|c| (c.title, c.subtitle)).collect();
        assert!(!about.iter().any(|(t, _)| t == "Original title"), "same as name");
        assert!(about.contains(&("Released".into(), "15 Jul 2010".into())));
        assert!(about.contains(&("Critics".into(), "87%".into())));
        assert!(about.contains(&("Director".into(), "Christopher Nolan".into())));
        assert!(about.contains(&("Studio".into(), "Legendary".into())));
    }

    #[test]
    fn trailer_prefers_server_file_then_youtube() {
        let none = movie(serde_json::json!({}));
        assert!(trailer_target(&none).is_none());
        let web = movie(serde_json::json!({ "RemoteTrailers": [
            { "Url": "https://vimeo.com/1" }, { "Url": "https://www.youtube.com/watch?v=abc" }
        ] }));
        let Some(Target::Trailer { local, remote, .. }) = trailer_target(&web) else { panic!() };
        assert!(!local);
        assert_eq!(remote.as_deref(), Some("https://www.youtube.com/watch?v=abc"));
        let local = movie(serde_json::json!({ "LocalTrailerCount": 1 }));
        assert!(matches!(trailer_target(&local), Some(Target::Trailer { local: true, remote: None, .. })));
    }

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
