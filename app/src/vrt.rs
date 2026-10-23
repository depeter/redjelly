//! VRT MAX: live channels, TV guide and catalogue screens (native, from
//! VRT's GraphQL via `rj-vrt`), played in VRT's own web player
//! (`web_player.rs`, Windows) or, for the two unencrypted channels, in mpv.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rj_input::Action;
use rj_vrt::{CHANNELS, Channel, Section, Tile, TileAction, TileKind, epg};
use slint::SharedString;

use crate::images::ImageSrc;
use crate::layout::{self, STYLE_BUTTON, STYLE_WIDE};
use crate::nav::Placement;
use crate::playback::{PlayRequest, format_time};
use crate::screens::{CardContent, Content, HeaderContent, PageSource, Paging, RowContent, Source, Target, button, row};
use crate::web_player::{self, PageState, WebPlayer};
use crate::{App, SCREEN_PLAYER, with_app};

/// Setting cards and image cards: dimmed, can't be played here.
pub const MARK_UNAVAILABLE: i32 = 2;
/// Image cards: red LIVE badge.
pub const MARK_LIVE: i32 = 3;

const ACCESS_COOKIE: &str = "vrtnu-site_profile_at";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(45);
/// Give up when the page shows no video this long after loading.
const NO_VIDEO_TIMEOUT: Duration = Duration::from_secs(40);
/// Within this of the live edge counts as live.
const LIVE_SLACK: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Auth {
    Unknown,
    /// The hidden web player is on VRT's login pages.
    SigningIn(Instant),
    Done,
}

/// Something to play once signing in has finished.
#[derive(Debug, Clone)]
struct Queued {
    url: String,
    title: String,
    live: Option<&'static Channel>,
}

/// A page playing in the web player.
pub struct WebSession {
    title: String,
    live: Option<&'static Channel>,
    /// The programme on air (live channels with a guide), for Restart.
    programme: Option<epg::Programme>,
    state: PageState,
    loaded: Instant,
    shown_msg: String,
}

pub struct State {
    client: RefCell<rj_vrt::Client>,
    pub web: WebPlayer,
    pub session: RefCell<Option<WebSession>>,
    auth: Cell<Auth>,
    queued: RefCell<Option<Queued>>,
    /// Player key for the clear streams (see `rj_vrt::stream`).
    key_cache: PathBuf,
}

impl State {
    pub fn new(data_dir: &std::path::Path, cache_dir: &std::path::Path) -> Self {
        Self {
            client: RefCell::new(rj_vrt::Client::new()),
            web: WebPlayer::new(data_dir.join("webview")),
            session: RefCell::new(None),
            auth: Cell::new(Auth::Unknown),
            queued: RefCell::new(None),
            key_cache: cache_dir.join("vrt-player-key.json"),
        }
    }
}

fn image(url: &str) -> ImageSrc {
    ImageSrc { url: url.to_owned(), key: url.to_owned() }
}

/// VRT refreshes its live screenshots every few minutes.
fn screenshot(url: &str) -> ImageSrc {
    let bucket = epg::now() / 300;
    ImageSrc { url: url.to_owned(), key: format!("{url}#{bucket}") }
}

fn unavailable(title: impl Into<String>, why: &str) -> CardContent {
    CardContent { mark: MARK_UNAVAILABLE, target: Target::VrtUnavailable(why.into()), ..button(title, Target::None) }
}

fn page_url(path: &str) -> String {
    format!("{}{path}", rj_vrt::SITE)
}

/// What's needed to build cards: whether DRM items can play here.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub web: bool,
    pub signed_in: bool,
}

fn live_target(ch: &'static Channel, caps: Caps) -> Option<Target> {
    (ch.clear || caps.web).then(|| Target::VrtLive { slug: ch.slug.into() })
}

fn tile_card(t: &Tile, caps: Caps) -> CardContent {
    let subtitle = [t.meta.as_str(), t.description.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
    let mut card = CardContent {
        title: t.title.clone(),
        subtitle,
        image: t.image.as_deref().map(image),
        progress: t.progress.unwrap_or(-1.0),
        played: t.completed,
        mark: 0,
        target: Target::None,
    };
    match t.action() {
        TileAction::Open(path) => card.target = Target::Open(Source::VrtPage { path }),
        TileAction::Play(path) if caps.web => {
            card.target = Target::VrtPlay { path, title: t.title.clone() };
            if t.kind == TileKind::Livestream {
                card.mark = MARK_LIVE;
            }
        }
        TileAction::Live(ch) => match live_target(ch, caps) {
            Some(target) => {
                card.target = target;
                card.mark = MARK_LIVE;
            }
            None => {
                card.mark = MARK_UNAVAILABLE;
                card.target = Target::VrtUnavailable("This channel only plays on Windows".into());
            }
        },
        TileAction::Play(_) => {
            card.mark = MARK_UNAVAILABLE;
            card.target = Target::VrtUnavailable("VRT MAX videos only play on Windows".into());
        }
        TileAction::None => {
            card.mark = MARK_UNAVAILABLE;
            card.target = Target::VrtUnavailable("Not online at VRT MAX".into());
        }
    }
    card
}

/// One section as a row; a "See all" card opens the rest as a grid.
fn section_row(s: &Section, caps: Caps) -> Option<RowContent> {
    if s.tiles.iter().all(|t| t.kind == TileKind::Button) {
        let cards = s.tiles.iter().filter_map(|t| match t.action() {
            TileAction::Open(path) => Some(button(&t.title, Target::Open(Source::VrtPage { path }))),
            _ => None,
        });
        return Some(row(&s.title, STYLE_BUTTON, cards.collect())).filter(|r| !r.cards.is_empty());
    }
    let mut cards: Vec<_> = s.tiles.iter().map(|t| tile_card(t, caps)).collect();
    if let (Some(_), Some(list_id)) = (&s.cursor, &s.list_id) {
        cards.push(button("See all", Target::Open(Source::VrtList { list_id: list_id.clone(), title: s.title.clone() })));
    }
    (!cards.is_empty()).then(|| row(&s.title, STYLE_WIDE, cards))
}

/// Rows for a page's sections; on-demand seasons become one row of buttons.
fn section_rows(sections: &[Section], caps: Caps) -> Vec<RowContent> {
    let mut rows: Vec<_> = sections.iter().filter(|s| !s.lazy).filter_map(|s| section_row(s, caps)).collect();
    let more: Vec<_> = sections
        .iter()
        .filter(|s| s.lazy)
        .filter_map(|s| {
            let list_id = s.list_id.clone()?;
            Some(button(&s.title, Target::Open(Source::VrtList { list_id, title: s.title.clone() })))
        })
        .collect();
    if !more.is_empty() {
        rows.push(row("More seasons", STYLE_BUTTON, more));
    }
    rows
}

/// "Live on VRT": every channel with what's on now.
pub async fn live_row(client: &rj_vrt::Client, caps: Caps) -> RowContent {
    let now = epg::now();
    let today = epg::local_date(0);
    let guides = futures_join(CHANNELS.iter().map(|ch| {
        let (client, today) = (client.clone(), today.clone());
        async move {
            let slug = ch.guide?;
            match tokio::time::timeout(Duration::from_secs(5), client.guide(slug, &today, now)).await {
                Ok(Ok(g)) => Some(g),
                Ok(Err(e)) => {
                    tracing::debug!("VRT guide {slug}: {e}");
                    None
                }
                Err(_) => None,
            }
        }
    }))
    .await;
    let cards = CHANNELS
        .iter()
        .zip(guides)
        .map(|(ch, guide)| {
            let current = guide.as_ref().and_then(|g| g.current().cloned());
            let mut card = CardContent {
                title: ch.name.into(),
                subtitle: current.as_ref().map_or_else(|| "Live".into(), |p| format!("{} · {}", p.title, p.times())),
                image: Some(ch.screenshot.map_or_else(|| image(ch.logo), screenshot)),
                progress: current.as_ref().map_or(-1.0, |p| p.progress(now)),
                played: false,
                mark: MARK_LIVE,
                target: Target::None,
            };
            match live_target(ch, caps) {
                Some(t) => card.target = t,
                None => {
                    card.mark = MARK_UNAVAILABLE;
                    card.subtitle = "Windows only".into();
                    card.target = Target::VrtUnavailable("This channel only plays on Windows".into());
                }
            }
            card
        })
        .collect();
    row("Live on VRT", STYLE_WIDE, cards)
}

/// `join_all` without the futures crate: spawns each and awaits in order.
async fn futures_join<T: Default + Send + 'static>(
    futs: impl Iterator<Item = impl Future<Output = T> + Send + 'static>,
) -> Vec<T> {
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap_or_default());
    }
    out
}

fn header(title: &str, meta: String, overview: String, backdrop: Option<&str>) -> HeaderContent {
    HeaderContent { title: title.into(), meta, overview, backdrop: backdrop.map(image), ..Default::default() }
}

fn account_meta(caps: Caps) -> String {
    match (caps.web, caps.signed_in) {
        (false, _) => "Live: VRT NWS and Ketnet Junior · everything else needs Windows (DRM)".into(),
        (true, true) => "Signed in · type to search".into(),
        (true, false) => "Not signed in: add your VRT account in Settings · type to search".into(),
    }
}

/// The VRT MAX screen.
pub async fn vrt_home(client: rj_vrt::Client, caps: Caps) -> anyhow::Result<Content> {
    let mut toolbar = vec![button("Search VRT MAX", Target::Open(Source::VrtSearch { term: String::new() }))];
    for ch in CHANNELS.iter().filter(|c| c.guide.is_some()) {
        toolbar.push(button(format!("Guide · {}", ch.name), Target::Open(Source::VrtGuide { slug: ch.slug.into(), day: 0 })));
    }
    let mut rows = vec![row("", STYLE_BUTTON, toolbar)];
    let live = live_row(&client, caps);
    if !caps.web {
        rows.push(live.await);
        return Ok(Content { source: Source::Vrt, header: Some(header("VRT MAX", account_meta(caps), String::new(), None)), rows, focus: (1, 0), paging: None });
    }
    let (live, home, landing, cont, mine) = tokio::join!(
        live,
        client.page("/vrtmax/"),
        client.page("/vrtmax/zoeken/"),
        async { if caps.signed_in { client.continue_watching().await.ok() } else { None } },
        async { if caps.signed_in { client.my_list().await.ok() } else { None } },
    );
    let mut live = live;
    let landing = landing.ok().flatten();
    // Event streams (sports, specials) next to the channels.
    if let Some(l) = &landing {
        let events = l.sections.iter().filter(|s| s.tiles.iter().any(|t| t.kind == TileKind::Livestream)).flat_map(|s| &s.tiles);
        live.cards.extend(
            events.filter(|t| matches!(t.action(), TileAction::Play(_))).map(|t| {
                let mut c = tile_card(t, caps);
                c.subtitle = t.title.clone();
                c.title = "Live event".into();
                c
            }),
        );
    }
    rows.push(live);
    for (title, tiles) in [("Continue watching", cont), ("My list", mine)] {
        if let Some(tiles) = tiles.filter(|t| !t.is_empty()) {
            rows.push(row(title, STYLE_WIDE, tiles.iter().map(|t| tile_card(t, caps)).collect()));
        }
    }
    if let Some(l) = &landing {
        rows.extend(l.sections.iter().filter(|s| s.tiles.iter().all(|t| t.kind == TileKind::Button)).filter_map(|s| section_row(s, caps)));
    }
    let home = home?.ok_or_else(|| anyhow::anyhow!("VRT MAX returned no home page"))?;
    rows.extend(section_rows(&home.sections, caps));
    rows.retain(|r| !r.cards.is_empty());
    Ok(Content { source: Source::Vrt, header: Some(header("VRT MAX", account_meta(caps), String::new(), None)), rows, focus: (1, 0), paging: None })
}

/// A programme, collection or category page.
pub async fn vrt_page(client: rj_vrt::Client, path: String, caps: Caps) -> anyhow::Result<Content> {
    let page = client.page(&path).await?.ok_or_else(|| anyhow::anyhow!("VRT MAX has no page {path}"))?.without_summary_tab();
    let mut rows = Vec::new();
    let mut buttons = Vec::new();
    if let Some(watch) = &page.watch {
        buttons.push(if caps.web {
            button("Play", Target::VrtPlay { path: watch.clone(), title: page.title.clone() })
        } else {
            unavailable("Play", "VRT MAX videos only play on Windows")
        });
    }
    if let (Some((id, on)), true) = (&page.favorite, caps.signed_in) {
        buttons.push(button(
            if *on { "Remove from My list" } else { "Add to My list" },
            Target::VrtFavorite { id: id.clone(), on: !*on },
        ));
    }
    let has_buttons = !buttons.is_empty();
    if has_buttons {
        rows.push(row("", STYLE_BUTTON, buttons));
    }
    rows.extend(section_rows(&page.sections, caps));
    // Start on Play, or else on the content below the My list button.
    let focus = (usize::from(has_buttons && page.watch.is_none() && rows.len() > 1), 0);
    Ok(Content {
        source: Source::VrtPage { path },
        header: Some(header(&page.title, String::new(), page.description.clone(), page.image.as_deref())),
        rows,
        focus,
        paging: None,
    })
}

fn grid(cards: Vec<CardContent>, columns: usize) -> Vec<RowContent> {
    let mut rows = Vec::new();
    let mut it = cards.into_iter().peekable();
    while it.peek().is_some() {
        rows.push(row("", STYLE_WIDE, it.by_ref().take(columns.max(1)).collect()));
    }
    rows
}

pub fn grid_columns(window_width: f32) -> usize {
    layout::visible_cards(window_width - layout::SIDE_PAD, STYLE_WIDE)
}

/// A whole list as a grid, loading more as you scroll.
pub async fn vrt_list(client: rj_vrt::Client, list_id: String, title: String, caps: Caps, columns: usize) -> anyhow::Result<Content> {
    let (tiles, cursor) = client.list(&list_id, None).await?;
    let cards: Vec<_> = tiles.iter().map(|t| tile_card(t, caps)).collect();
    let paging = cursor.map(|c| Paging {
        source: PageSource::Vrt { list_id: list_id.clone(), cursor: c },
        loaded: cards.len() as u32,
        total: 0,
        columns,
    });
    Ok(Content {
        source: Source::VrtList { list_id, title: title.clone() },
        header: Some(header(&title, String::new(), String::new(), None)),
        rows: grid(cards, columns),
        focus: (0, 0),
        paging,
    })
}

pub async fn vrt_search(client: rj_vrt::Client, term: String, caps: Caps) -> anyhow::Result<Content> {
    let rows = if term.trim().is_empty() {
        // Categories and what's popular while nothing is typed.
        let landing = client.page("/vrtmax/zoeken/").await?;
        let sections = landing.map(|p| p.sections).unwrap_or_default();
        section_rows(&sections.into_iter().filter(|s| !s.tiles.iter().any(|t| t.kind == TileKind::Livestream)).collect::<Vec<_>>(), caps)
    } else {
        section_rows(&client.search(&term).await?, caps)
    };
    Ok(Content {
        source: Source::VrtSearch { term: term.clone() },
        header: Some(HeaderContent {
            title: "Search VRT MAX".into(),
            meta: format!("{term}_"),
            info: if rows.is_empty() && !term.is_empty() { "Nothing found".into() } else { String::new() },
            ..Default::default()
        }),
        rows,
        focus: (0, 0),
        paging: None,
    })
}

/// One channel's guide for a day, as a grid starting at what's on now.
pub async fn vrt_guide(client: rj_vrt::Client, slug: String, day: i64, caps: Caps, columns: usize) -> anyhow::Result<Content> {
    let ch = Channel::by_slug(&slug).ok_or_else(|| anyhow::anyhow!("unknown VRT channel {slug}"))?;
    let now = epg::now();
    let guide = client.guide(ch.guide.unwrap_or(ch.slug), &epg::local_date(day), now).await?;
    let mut toolbar = Vec::new();
    if let Some(t) = live_target(ch, caps) {
        toolbar.push(button(format!("Watch {} live", ch.name), t));
    }
    if day > -7 {
        toolbar.push(button(format!("◀ {}", epg::day_label(day - 1)), Target::Open(Source::VrtGuide { slug: slug.clone(), day: day - 1 })));
    }
    if day < 1 {
        toolbar.push(button(format!("{} ▶", epg::day_label(day + 1)), Target::Open(Source::VrtGuide { slug: slug.clone(), day: day + 1 })));
    }
    for other in CHANNELS.iter().filter(|c| c.guide.is_some() && c.slug != slug) {
        toolbar.push(button(other.name, Target::Open(Source::VrtGuide { slug: other.slug.into(), day })));
    }
    let mut focus_at = None;
    let cards: Vec<_> = guide
        .programmes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut card = CardContent {
                title: format!("{} · {}", epg::clock(p.start), p.title),
                subtitle: p.description.clone(),
                image: p.image.as_deref().map(image),
                progress: -1.0,
                played: false,
                mark: 0,
                target: Target::None,
            };
            if p.live {
                focus_at = Some(i);
                card.progress = p.progress(now);
                card.mark = MARK_LIVE;
                card.target = live_target(ch, caps).unwrap_or(Target::VrtUnavailable("This channel only plays on Windows".into()));
            } else if p.start > now {
                card.target = Target::VrtUnavailable(format!("Starts at {}", epg::clock(p.start)));
            } else if let (Some(vod), true) = (&p.vod, caps.web) {
                card.target = Target::VrtPlay { path: vod.clone(), title: p.title.clone() };
            } else {
                card.mark = MARK_UNAVAILABLE;
                card.target =
                    Target::VrtUnavailable(if p.vod.is_some() { "VRT MAX videos only play on Windows" } else { "Not online at VRT MAX" }.into());
            }
            card
        })
        .collect();
    // Today: what's on now; another day: its first (or, in the past, last
    // still-online) programme.
    let at = focus_at.unwrap_or(if day < 0 { cards.len().saturating_sub(1) } else { 0 });
    let columns = columns.max(1);
    let mut rows = vec![row("", STYLE_BUTTON, toolbar)];
    let empty = cards.is_empty();
    rows.extend(grid(cards, columns));
    Ok(Content {
        source: Source::VrtGuide { slug, day },
        header: Some(HeaderContent {
            title: format!("{} · {}", ch.name, epg::day_label(day)),
            info: if empty { "No guide for this day".into() } else { "Enter on a past programme plays it from VRT MAX".into() },
            ..Default::default()
        }),
        rows,
        focus: if empty { (0, 0) } else { (1 + at / columns, at % columns) },
        paging: None,
    })
}

impl App {
    pub fn vrt_enabled(&self) -> bool {
        self.config.borrow().vrt.enabled
    }

    pub fn vrt_caps(&self) -> Caps {
        Caps {
            // The video window may not exist yet (Home loads first).
            web: web_player::available() && self.config.borrow().video_output() == rj_core::config::VideoOutput::Window,
            signed_in: self.vrt.client.borrow().signed_in(),
        }
    }

    pub fn vrt_client(&self) -> rj_vrt::Client {
        self.vrt.client.borrow().clone()
    }

    /// Home's "Live on VRT" row, loaded alongside the rest of home.
    pub fn home_vrt_rows(&self) -> Option<impl Future<Output = Vec<RowContent>> + Send + 'static> {
        if !self.vrt_enabled() {
            return None;
        }
        self.vrt_sign_in();
        let (client, caps) = (self.vrt_client(), self.vrt_caps());
        Some(async move {
            match tokio::time::timeout(Duration::from_secs(6), live_row(&client, caps)).await {
                Ok(r) => vec![r],
                Err(_) => {
                    tracing::warn!("home VRT row: VRT took too long");
                    Vec::new()
                }
            }
        })
    }

    pub fn open_vrt(&self, source: Source, placement: Placement) {
        self.vrt_sign_in();
        let (client, caps) = (self.vrt_client(), self.vrt_caps());
        let columns = grid_columns(self.window_width());
        let src = source.clone();
        self.load(source, placement, async move {
            match src {
                Source::Vrt => vrt_home(client, caps).await,
                Source::VrtPage { path } => vrt_page(client, path, caps).await,
                Source::VrtList { list_id, title } => vrt_list(client, list_id, title, caps, columns).await,
                Source::VrtSearch { term } => vrt_search(client, term, caps).await,
                Source::VrtGuide { slug, day } => vrt_guide(client, slug, day, caps, columns).await,
                _ => unreachable!("not a VRT screen"),
            }
        });
    }

    /// Next page of a VRT list grid.
    pub fn vrt_more(&self, generation: u64, list_id: String, cursor: String) {
        let (client, caps) = (self.vrt_client(), self.vrt_caps());
        self.spawn_ui(async move { client.list(&list_id, Some(&cursor)).await.map(|r| (list_id, r)) }, move |app, result| match result {
            Ok((list_id, (tiles, next))) => {
                let cards = tiles.iter().map(|t| tile_card(t, caps)).collect();
                if let Some(c) = next.clone() {
                    app.update_paging(generation, |p| {
                        if let Some(p) = p {
                            p.source = PageSource::Vrt { list_id, cursor: c };
                        }
                    });
                }
                app.append_grid(generation, cards, next.is_none());
            }
            Err(e) => {
                app.toast(format!("Couldn't load more: {e}"));
                app.update_paging(generation, |p| *p = None);
            }
        });
    }

    pub fn vrt_favorite(&self, id: String, on: bool) {
        let client = self.vrt_client();
        self.spawn_ui(async move { client.set_favorite(&id, on).await }, move |app, r| match r {
            Ok(()) => {
                app.toast(if on { "Added to My list" } else { "Removed from My list" });
                app.reload_top();
            }
            Err(e) => app.toast(format!("Couldn't update My list: {e}")),
        });
    }

    // ---- Signing in ------------------------------------------------------------------

    fn vrt_credentials(&self) -> Option<(String, String)> {
        let user = self.config.borrow().vrt.username.clone()?;
        let pass = self.tokens.get_secret(&crate::secrets::provider_account("vrtmax", &user))?;
        Some((user, pass))
    }

    /// Creates the web player (hidden) if needed.
    fn ensure_web(&self) -> anyhow::Result<()> {
        self.vrt.web.ensure(self.video_hwnd.get(), |msg| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.web_message(&msg)));
        })
    }

    /// Takes the account token from the web player's cookies; true when it's
    /// good for a while yet.
    fn vrt_take_token(&self) -> bool {
        let token = self.vrt.web.cookie("https://www.vrt.be/vrtmax/", ACCESS_COOKIE);
        let fresh = token.as_deref().and_then(rj_vrt::token_expiry).is_some_and(|exp| exp > epg::now() + 120);
        let token = token.filter(|_| fresh);
        let mut client = self.vrt.client.borrow_mut();
        *client = client.clone().with_token(token);
        fresh
    }

    /// Makes sure the VRT session is fresh: signs the web player in (hidden)
    /// with the stored account when the token is missing or expiring.
    pub fn vrt_sign_in(&self) {
        if !self.vrt_enabled() || !self.vrt_caps().web || self.vrt.session.borrow().is_some() {
            return;
        }
        if matches!(self.vrt.auth.get(), Auth::SigningIn(_)) {
            return;
        }
        if let Err(e) = self.ensure_web() {
            tracing::warn!("web player: {e:#}");
            return;
        }
        if self.vrt_take_token() {
            self.vrt.auth.set(Auth::Done);
            return;
        }
        if self.vrt_credentials().is_none() {
            self.vrt.auth.set(Auth::Done);
            return;
        }
        tracing::info!("VRT MAX: signing in");
        self.vrt.auth.set(Auth::SigningIn(Instant::now()));
        self.vrt.web.show(false);
        self.vrt.web.load(web_player::LOGIN_URL);
    }

    fn vrt_signed_in(&self, ok: bool, why: Option<String>) {
        self.vrt.auth.set(Auth::Done);
        self.vrt.web.load("about:blank");
        if ok {
            tracing::info!("VRT MAX: signed in");
            if self.top_source().is_some_and(|s| s.is_vrt()) {
                self.reload_top();
            }
        } else {
            tracing::warn!("VRT MAX sign-in failed: {why:?}");
            self.toast(format!("Couldn't sign in to VRT MAX{}", why.map(|w| format!(": {w}")).unwrap_or_default()));
        }
        if let Some(q) = self.vrt.queued.borrow_mut().take() {
            self.play_web(q.url, q.title, q.live);
        }
    }

    /// Settings → "Sign in now": forget the token and sign in again.
    pub fn vrt_sign_in_again(&self) {
        *self.vrt.client.borrow_mut() = self.vrt_client().with_token(None);
        self.vrt.auth.set(Auth::Unknown);
        if !self.vrt_enabled() {
            return self.toast("Turn VRT MAX on first");
        }
        if self.vrt_credentials().is_none() {
            return self.toast("Set your VRT MAX e-mail and password first");
        }
        if !self.vrt_caps().web {
            return self.toast("Signing in to VRT MAX needs Windows and Video output → Window");
        }
        // A stale cookie would make the login page bounce straight back.
        self.vrt_sign_in();
        if matches!(self.vrt.auth.get(), Auth::Done) {
            self.toast("Already signed in to VRT MAX");
        } else {
            self.toast("Signing in to VRT MAX…");
        }
    }

    /// A state report from the web player.
    pub fn web_message(&self, json: &str) {
        let Ok(state) = serde_json::from_str::<PageState>(json) else { return };
        if let Auth::SigningIn(_) = self.vrt.auth.get() {
            if state.frame != "top" {
                return;
            }
            // Trust WebView2 for where the page is, not the page itself.
            let url = self.vrt.web.url().unwrap_or_default();
            if url.starts_with("https://login.vrt.be/") {
                if let Some((user, pass)) = self.vrt_credentials() {
                    self.vrt.web.eval(&format!(
                        "window.__rj && __rj.login({}, {})",
                        web_player::js_str(&user),
                        web_player::js_str(&pass)
                    ));
                }
            } else if url.starts_with("https://www.vrt.be/") && !url.contains("/sso/") {
                let ok = self.vrt_take_token();
                self.vrt_signed_in(ok, (!ok).then(|| "no session after login".into()));
            }
            return;
        }
        let mut session = self.vrt.session.borrow_mut();
        let Some(s) = session.as_mut() else { return };
        // The frame with the video wins; the top page reports too.
        if state.video || !s.state.video || state.frame == s.state.frame {
            s.state = state;
        }
    }

    // ---- Playback --------------------------------------------------------------------

    pub fn vrt_live(&self, slug: &str) {
        let Some(ch) = Channel::by_slug(slug) else { return };
        if ch.clear {
            return self.play_clear(ch);
        }
        self.play_web(ch.page_url(), ch.name.into(), Some(ch));
    }

    pub fn vrt_play(&self, path: &str, title: String) {
        if !self.vrt.client.borrow().signed_in() && !path.starts_with("/vrtmax/livestream/") {
            self.toast("Not signed in to VRT MAX: the video may ask you to sign in (Settings → VRT MAX)");
        }
        self.play_web(page_url(path), title, None);
    }

    /// VRT NWS / Ketnet Junior: clear HLS through mpv.
    fn play_clear(&self, ch: &'static Channel) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.toast("Loading…");
        let (client, cache) = (self.vrt_client(), self.vrt.key_cache.clone());
        self.spawn_ui(async move { rj_vrt::stream::live_url(&client, ch, &cache).await }, move |app, r| {
            app.busy.set(false);
            app.clear_toast();
            match r {
                Ok(url) => app.start(PlayRequest { title: format!("{} · live", ch.name), ..PlayRequest::standalone(&url) }),
                Err(e) => app.toast(format!("Can't play {}: {e}", ch.name)),
            }
        });
    }

    fn play_web(&self, url: String, title: String, live: Option<&'static Channel>) {
        if let Err(e) = self.ensure_web() {
            return self.toast(format!("Can't play: {e:#}"));
        }
        if let Auth::SigningIn(_) = self.vrt.auth.get() {
            *self.vrt.queued.borrow_mut() = Some(Queued { url, title, live });
            return self.toast("Signing in to VRT MAX…");
        }
        // Stop mpv if it was playing something.
        if self.session.borrow().is_some() {
            self.handle_player(Action::Stop);
        }
        tracing::info!("web player: {url}");
        self.vrt.web.load(&url);
        self.vrt.web.show(true);
        *self.vrt.session.borrow_mut() = Some(WebSession {
            title: title.clone(),
            live,
            programme: None,
            state: PageState::default(),
            loaded: Instant::now(),
            shown_msg: String::new(),
        });
        let ui = self.ui();
        ui.set_player_title(title.as_str().into());
        ui.set_player_time(SharedString::new());
        ui.set_player_total(SharedString::new());
        ui.set_player_progress(0.0);
        ui.set_player_has_previous(live.is_some());
        ui.set_player_has_next(live.is_some());
        ui.set_screen(SCREEN_PLAYER);
        ui.invoke_focus_keys();
        self.show_osd();
        if let Some(m) = self.media.borrow_mut().as_mut() {
            let _ = m.set_now_playing(&rj_input::media_controls::NowPlaying {
                title: &title,
                subtitle: Some("VRT MAX"),
                cover_url: None,
                duration: None,
            });
        }
        // What's on now, for Restart and the times in the bar.
        if let Some(slug) = live.and_then(|c| c.guide) {
            let client = self.vrt_client();
            self.spawn_ui(
                async move { client.guide(slug, &epg::local_date(0), epg::now()).await },
                |app, r| {
                    if let (Ok(g), Some(s)) = (r, app.vrt.session.borrow_mut().as_mut()) {
                        s.programme = g.current().cloned();
                        if let Some(p) = &s.programme {
                            app.ui().set_player_title(format!("{} · {}", s.title, p.title).into());
                        }
                    }
                },
            );
        }
    }

    pub fn web_active(&self) -> bool {
        self.vrt.session.borrow().is_some()
    }

    pub fn web_stop(&self) {
        if self.vrt.session.borrow_mut().take().is_none() {
            return;
        }
        self.vrt.web.show(false);
        self.vrt.web.load("about:blank");
        self.close_panel();
        self.release_controls();
        if let Some(m) = self.media.borrow_mut().as_mut() {
            let _ = m.set_state(None, Duration::ZERO);
        }
        self.show_top();
        self.reload_top();
    }

    /// Every tick while the VRT session exists (signing in or playing).
    pub fn vrt_tick(&self, now: Instant) {
        if let Auth::SigningIn(since) = self.vrt.auth.get()
            && now.duration_since(since) > SIGN_IN_TIMEOUT
        {
            self.vrt_signed_in(false, Some("timed out".into()));
        }
        if self.vrt.web.created() {
            self.vrt.web.fit();
        }
    }

    /// The player tick while the web player plays.
    pub fn web_tick(&self, now: Instant) {
        let ui = self.ui();
        let (state, live, programme, loaded, new_msg) = {
            let mut session = self.vrt.session.borrow_mut();
            let Some(s) = session.as_mut() else { return };
            let msg = s.state.msgs.first().cloned().unwrap_or_default();
            let new_msg = (!msg.is_empty() && msg != s.shown_msg).then(|| {
                s.shown_msg = msg.clone();
                msg
            });
            (s.state.clone(), s.live, s.programme.clone(), s.loaded, new_msg)
        };
        if let Some(m) = new_msg {
            self.toast(format!("VRT MAX: {m}"));
        }
        if !state.video {
            if now.duration_since(loaded) > NO_VIDEO_TIMEOUT {
                self.toast("VRT's player didn't start; try again or check your VRT MAX account");
                return self.web_stop();
            }
            ui.set_player_time("Loading…".into());
        } else if let (Some((start, end)), Some(_)) = (state.seek.filter(|(s, e)| e > s), live) {
            let behind = (end - state.t).max(0.0);
            ui.set_player_time(if behind < LIVE_SLACK { "LIVE".into() } else { format!("-{}", format_time(behind)).into() });
            ui.set_player_total(programme.as_ref().map_or_else(SharedString::new, |p| p.times().into()));
            ui.set_player_progress(((state.t - start) / (end - start)).clamp(0.0, 1.0) as f32);
        } else {
            let dur = state.d.max(0.0);
            ui.set_player_time(format_time(state.t).into());
            ui.set_player_total(if dur > 0.0 { format_time(dur).into() } else { SharedString::new() });
            ui.set_player_progress(if dur > 0.0 { (state.t / dur).clamp(0.0, 1.0) as f32 } else { 0.0 });
        }
        ui.set_player_paused(state.video && state.paused);
        ui.set_player_volume(state.vol as f32);
        ui.set_player_muted(state.muted);
        if now > self.osd_until.get() {
            ui.set_player_osd(false);
            if !state.paused {
                self.release_controls();
            }
        }
        if now.duration_since(self.pointer_at.get()) >= crate::controls::POINTER_IDLE {
            ui.set_player_pointer_idle(true);
        }
        if now.duration_since(self.last_media_update.get()) >= Duration::from_secs(1) {
            self.last_media_update.set(now);
            if let Some(m) = self.media.borrow_mut().as_mut() {
                let _ = m.set_state(Some(!state.paused), Duration::from_secs_f64(state.t.max(0.0)));
            }
        }
    }

    fn web_cmd(&self, c: &str, arg: Option<f64>) {
        let arg = arg.map_or_else(|| "undefined".into(), |a| a.to_string());
        self.vrt.web.eval(&format!("window.__rj && __rj.cmd({}, {arg})", web_player::js_str(c)));
    }

    fn web_state(&self) -> PageState {
        self.vrt.session.borrow().as_ref().map(|s| s.state.clone()).unwrap_or_default()
    }

    /// Player keys while the web player plays.
    pub fn web_handle(&self, action: Action) {
        let ui = self.ui();
        if action == Action::Back && ui.get_player_osd() && !ui.get_player_paused() {
            return self.hide_controls();
        }
        self.show_osd();
        let state = self.web_state();
        let live = self.vrt.session.borrow().as_ref().and_then(|s| s.live);
        match action {
            Action::Back | Action::Stop => self.web_stop(),
            Action::Activate | Action::OsdMenu => self.controls_activate(),
            Action::PlayPause => self.web_cmd("toggle", None),
            Action::Seek(d) => self.web_cmd("seek", Some(d)),
            Action::SeekExact(t) => self.web_cmd("seekto", Some(t)),
            Action::SeekPercent(p) => self.web_seek_fraction((p / 100.0) as f32),
            Action::Volume(d) => {
                let v = (state.vol + d / 100.0).clamp(0.0, 1.0);
                self.web_cmd("vol", Some(v));
                self.toast(format!("Volume {}", (v * 100.0).round()));
            }
            Action::Mute => {
                self.web_cmd("mute", None);
                self.toast(if state.muted { "Sound on" } else { "Muted" });
            }
            Action::CycleSubtitle | Action::CycleSubtitleBack | Action::ToggleSubtitles | Action::SubtitlePanel => {
                self.web_cmd("subs", None);
                let cur = state.tracks.iter().position(|t| t.on);
                let next = match cur {
                    Some(i) => state.tracks.get(i + 1),
                    None => state.tracks.first(),
                };
                self.toast(match next {
                    _ if state.tracks.is_empty() => "No subtitles for this video".into(),
                    Some(t) => format!("Subtitles: {}", if t.label.is_empty() { &t.lang } else { &t.label }),
                    None => "Subtitles off".into(),
                });
            }
            Action::NextEpisode if live.is_some() => {
                self.web_cmd("live", None);
                self.toast("Live");
            }
            Action::PreviousEpisode if live.is_some() => self.web_restart(&state),
            Action::ShowStats => self.vrt.web.devtools(),
            Action::Speed(_) | Action::SpeedReset => {}
            _ => self.toast("Not available in VRT's player"),
        }
    }

    /// Back to the start of the programme on air, as far as the DVR window
    /// reaches.
    fn web_restart(&self, state: &PageState) {
        let programme = self.vrt.session.borrow().as_ref().and_then(|s| s.programme.clone());
        let (Some(p), Some((start, end))) = (programme, state.seek) else {
            return self.toast("Can't rewind this stream");
        };
        let elapsed = (epg::now() - p.start) as f64;
        let target = end - elapsed;
        if target < start {
            self.web_cmd("seekto", Some(start));
            self.toast(format!("Only the last {} min can be rewound", ((end - start) / 60.0).round()));
        } else {
            self.web_cmd("seekto", Some(target));
            self.toast(format!("From the start of {}", p.title));
        }
    }

    pub fn web_seek_fraction(&self, fraction: f32) {
        self.show_osd();
        let s = self.web_state();
        let f = f64::from(fraction).clamp(0.0, 1.0);
        let t = match s.seek {
            Some((start, end)) if s.d <= 0.0 => start + f * (end - start),
            _ => f * s.d.max(0.0),
        };
        self.web_cmd("seekto", Some(t));
    }

    pub fn web_set_volume(&self, fraction: f32) {
        self.show_osd();
        self.web_cmd("vol", Some(f64::from(fraction).clamp(0.0, 1.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(kind: TileKind, link: &str, available: bool) -> Tile {
        Tile {
            kind,
            title: "T".into(),
            description: "D".into(),
            meta: "S1".into(),
            link: Some(link.into()),
            image: None,
            available,
            start: None,
            duration: None,
            progress: Some(0.5),
            completed: false,
        }
    }

    #[test]
    fn cards_follow_platform() {
        let web = Caps { web: true, signed_in: true };
        let none = Caps { web: false, signed_in: false };
        let ep = tile(TileKind::Episode, "/vrtmax/a-z/thuis/32/thuis-s32a1/", true);
        assert!(matches!(tile_card(&ep, web).target, Target::VrtPlay { .. }));
        assert_eq!(tile_card(&ep, web).subtitle, "S1 · D");
        assert_eq!(tile_card(&ep, none).mark, MARK_UNAVAILABLE);
        let gone = tile(TileKind::Episode, "/vrtmax/a-z/thuis/32/thuis-s32a1/", false);
        assert_eq!(tile_card(&gone, web).mark, MARK_UNAVAILABLE);
        let prog = tile(TileKind::Program, "/vrtmax/a-z/thuis/", true);
        assert!(matches!(tile_card(&prog, none).target, Target::Open(Source::VrtPage { .. })));
        let nws = tile(TileKind::Livestream, "/vrtmax/livestream/video/vrt-nws/", true);
        assert!(matches!(tile_card(&nws, none).target, Target::VrtLive { .. }));
        let vrt1 = tile(TileKind::Livestream, "/vrtmax/livestream/video/vrt1/", true);
        assert_eq!(tile_card(&vrt1, none).mark, MARK_UNAVAILABLE);
        assert_eq!(tile_card(&vrt1, web).mark, MARK_LIVE);
    }

    #[test]
    fn sections_become_rows() {
        let caps = Caps { web: true, signed_in: false };
        let s = |title: &str, tiles: Vec<Tile>, cursor: Option<&str>, lazy: bool| Section {
            title: title.into(),
            list_id: Some(format!("id-{title}")),
            tiles,
            cursor: cursor.map(Into::into),
            lazy,
            tab: None,
        };
        let rows = section_rows(
            &[
                s("Seizoen 2", vec![tile(TileKind::Episode, "/vrtmax/a-z/x/2/x-s2a1/", true)], Some("c"), false),
                s("Seizoen 1", vec![], None, true),
                s("Genres", vec![tile(TileKind::Button, "/vrtmax/zoeken/?facets=x", true)], None, false),
            ],
            caps,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].cards.len(), 2, "episode + See all");
        assert!(matches!(&rows[0].cards[1].target, Target::Open(Source::VrtList { list_id, .. }) if list_id == "id-Seizoen 2"));
        assert_eq!(rows[1].style, STYLE_BUTTON);
        assert_eq!(rows[2].title, "More seasons");
    }
}
