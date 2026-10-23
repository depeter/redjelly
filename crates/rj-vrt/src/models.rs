//! VRT's GraphQL page model, flattened into what redjelly shows: a page is a
//! list of titled sections of tiles.

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

use crate::channels::Channel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileKind {
    Program,
    Episode,
    Livestream,
    Banner,
    /// A category / genre link (search facets).
    Button,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub kind: TileKind,
    pub title: String,
    /// Episode title or programme blurb.
    pub description: String,
    /// "S2 · Afl.61", "Vr 23/10".
    pub meta: String,
    /// Site path (`/vrtmax/a-z/thuis/`).
    pub link: Option<String>,
    /// Resized image URL.
    pub image: Option<String>,
    /// False for episodes that aren't (or no longer) online.
    pub available: bool,
    /// Broadcast start, Unix seconds (guide tiles only).
    pub start: Option<i64>,
    pub duration: Option<u32>,
    /// Watched fraction, 0..1, from the account's resume points.
    pub progress: Option<f32>,
    pub completed: bool,
}

/// What selecting a tile does.
#[derive(Debug, Clone, PartialEq)]
pub enum TileAction {
    /// Open another VRT page (programme, collection).
    Open(String),
    /// Play this page in VRT's web player.
    Play(String),
    Live(&'static Channel),
    None,
}

impl Tile {
    pub fn action(&self) -> TileAction {
        let Some(link) = self.link.as_deref() else { return TileAction::None };
        if let Some(ch) = Channel::from_link(link) {
            return TileAction::Live(ch);
        }
        // Event streams (a Sporza race) play like an episode.
        if link.starts_with("/vrtmax/livestream/video/") {
            return TileAction::Play(link.into());
        }
        if self.kind == TileKind::Episode && is_episode_link(link) {
            return if self.available { TileAction::Play(link.into()) } else { TileAction::None };
        }
        if link.starts_with("/vrtmax/") { TileAction::Open(link.into()) } else { TileAction::None }
    }
}

/// `/vrtmax/a-z/{programme}/{season}/{episode}/`.
pub fn is_episode_link(link: &str) -> bool {
    let parts: Vec<_> = link.trim_matches('/').split('/').collect();
    parts.len() >= 5 && parts[0] == "vrtmax" && parts[1] == "a-z"
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    pub list_id: Option<String>,
    pub tiles: Vec<Tile>,
    /// Set while there are more tiles than shown (`list(listId, after)`).
    pub cursor: Option<String>,
    /// A season VRT only loads on demand; `tiles` is empty.
    pub lazy: bool,
    /// From a page's tab ("Kijk (34)") rather than its body.
    pub tab: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub kind: String,
    pub title: String,
    pub description: String,
    pub image: Option<String>,
    pub sections: Vec<Section>,
    /// `Mijn lijst` toggle: (action id, on).
    pub favorite: Option<(String, bool)>,
    /// The header's "Kijk nu" episode.
    pub watch: Option<String>,
}

/// `https://images.vrt.be/orig/…` → a 640 px wide copy.
pub fn resize(url: &str) -> String {
    url.replacen("/orig/", "/w640hx/", 1)
}

/// Broadcast start encoded in a guide tile's `componentId` (`$` + base64 of
/// `…d#13<epoch ms>…`).
pub fn component_start(id: &str) -> Option<i64> {
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let raw = engine.decode(id.strip_prefix('$')?).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let at = text.find("d#13")? + 4;
    let ms = text.get(at..at + 13)?;
    ms.bytes().all(|b| b.is_ascii_digit()).then(|| ms.parse::<i64>().ok()).flatten().map(|ms| ms / 1000)
}

/// "8 min" → 480.
fn minutes(meta: &[Meta]) -> Option<u32> {
    meta.iter().find_map(|m| m.value.as_deref()?.strip_suffix(" min")?.trim().parse::<u32>().ok().map(|m| m * 60))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTile {
    #[serde(rename = "__typename")]
    typename: String,
    component_id: Option<String>,
    title: Option<String>,
    action: Option<RawAction>,
    image: Option<RawImage>,
    description: Option<String>,
    available: Option<bool>,
    primary_meta: Option<Vec<Meta>>,
    status_meta: Option<Vec<Meta>>,
    progress: Option<RawProgress>,
}

#[derive(Debug, Deserialize)]
struct RawAction {
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImage {
    template_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    value: Option<String>,
    short_value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawProgress {
    pub duration_in_seconds: Option<f64>,
    pub progress_in_seconds: Option<f64>,
    #[serde(default)]
    pub completed: bool,
}

impl RawTile {
    fn into_tile(self) -> Option<Tile> {
        let kind = match self.typename.as_str() {
            "ProgramTile" => TileKind::Program,
            "EpisodeTile" => TileKind::Episode,
            "LivestreamTile" => TileKind::Livestream,
            "BannerTile" => TileKind::Banner,
            "ButtonTile" => TileKind::Button,
            // Podcasts, radio: audio, not for a TV client.
            _ => return None,
        };
        let meta = self
            .primary_meta
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.short_value.as_deref().or(m.value.as_deref()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let total = self.progress.as_ref().and_then(|p| p.duration_in_seconds).filter(|d| *d > 0.0);
        let progress = match (&self.progress, total) {
            (Some(p), Some(total)) => p.progress_in_seconds.map(|s| (s / total).clamp(0.0, 1.0) as f32).filter(|f| *f > 0.0),
            _ => None,
        };
        Some(Tile {
            kind,
            title: self.title.unwrap_or_default(),
            description: self.description.unwrap_or_default(),
            meta,
            link: self.action.and_then(|a| a.link).filter(|l| !l.is_empty()),
            image: self.image.and_then(|i| i.template_url).map(|u| resize(&u)),
            available: self.available.unwrap_or(true),
            start: self.component_id.as_deref().and_then(component_start),
            duration: total.map(|d| d as u32).or_else(|| minutes(self.status_meta.as_deref().unwrap_or_default())),
            progress,
            completed: self.progress.is_some_and(|p| p.completed),
        })
    }
}

pub(crate) fn tile(v: &Value) -> Option<Tile> {
    RawTile::deserialize(v).ok()?.into_tile()
}

/// Tiles and the next-page cursor of a `PaginatedTileList` or `StaticTileList`.
pub(crate) fn tiles(list: &Value) -> (Vec<Tile>, Option<String>) {
    if let Some(items) = list.get("items").and_then(Value::as_array) {
        return (items.iter().filter_map(tile).collect(), None);
    }
    let paged = &list["paginatedItems"];
    let tiles = paged["edges"].as_array().into_iter().flatten().filter_map(|e| tile(&e["node"])).collect();
    let info = &paged["pageInfo"];
    let cursor = info["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
        .then(|| info["endCursor"].as_str().map(str::to_owned))
        .flatten();
    (tiles, cursor)
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Depth-first over components, descending into tabbed `ContainerNavigation`s.
/// An untitled list takes its tab's title ("Seizoen 32 (40 afleveringen)").
fn sections(components: &Value, tab: Option<&str>, out: &mut Vec<Section>) {
    for c in components.as_array().into_iter().flatten() {
        let list_id = str_of(&c["listId"]);
        let title = str_of(&c["title"]).or_else(|| tab.map(str::to_owned)).unwrap_or_default();
        match c["__typename"].as_str().unwrap_or_default() {
            "ContainerNavigation" => {
                for item in c["items"].as_array().into_iter().flatten() {
                    sections(&item["components"], item["title"].as_str(), out);
                }
            }
            "PaginatedTileList" | "StaticTileList" => {
                let (tiles, cursor) = tiles(c);
                if !tiles.is_empty() {
                    out.push(Section { title, list_id, tiles, cursor, lazy: false, tab: tab.map(str::to_owned) });
                }
            }
            "LazyTileList" if list_id.is_some() => {
                out.push(Section { title, list_id, tiles: vec![], cursor: None, lazy: true, tab: tab.map(str::to_owned) })
            }
            _ => {}
        }
    }
}

impl Page {
    /// Search result pages repeat a few hits of every tab under "Alles";
    /// keeps the full tabs only.
    pub fn without_summary_tab(mut self) -> Self {
        let tabbed = self.sections.iter().any(|s| s.tab.as_deref().is_some_and(|t| t != "Alles"));
        if tabbed {
            self.sections.retain(|s| s.tab.as_deref() != Some("Alles"));
        }
        self
    }

    /// `data.page` of the page query; `None` when VRT has no such page.
    pub(crate) fn parse(page: &Value) -> Option<Page> {
        if page.is_null() {
            return None;
        }
        let mut out = Vec::new();
        sections(&page["components"], None, &mut out);
        let header = &page["header"];
        let mut p = Page {
            kind: page["__typename"].as_str().unwrap_or_default().into(),
            title: page["title"].as_str().unwrap_or_default().into(),
            description: header["description"].as_str().unwrap_or_default().into(),
            image: header["image"]["templateUrl"].as_str().map(resize),
            sections: out,
            ..Default::default()
        };
        for item in header["actionItems"].as_array().into_iter().flatten() {
            let a = &item["action"];
            match a["__typename"].as_str() {
                Some("FavoriteAction") => {
                    p.favorite = str_of(&a["id"]).map(|id| (id, a["favorite"].as_bool().unwrap_or(false)))
                }
                Some("LinkAction") => {
                    if let Some(l) = str_of(&a["link"]).filter(|l| is_episode_link(l)) {
                        p.watch.get_or_insert(l);
                    }
                }
                _ => {}
            }
        }
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn home_page() {
        let p = Page::parse(&fixture("home")["data"]["page"]).unwrap();
        assert_eq!(p.kind, "HomePage");
        assert!(p.sections.len() > 10);
        let first = &p.sections[0];
        assert_eq!(first.title, "Moet je nu zien");
        assert!(first.cursor.is_some(), "only 8 of the list were asked for");
        let t = &first.tiles[0];
        assert_eq!(t.kind, TileKind::Program);
        assert!(t.image.as_deref().unwrap().contains("/w640hx/"));
        assert!(matches!(t.action(), TileAction::Open(l) if l.starts_with("/vrtmax/a-z/")));
        // Podcast tiles are dropped, so their list is too.
        assert!(!p.sections.iter().any(|s| s.title == "Populaire podcasts"));
    }

    #[test]
    fn programme_page() {
        let p = Page::parse(&fixture("program_thuis")["data"]["page"]).unwrap();
        assert_eq!(p.title, "Thuis");
        let (id, on) = p.favorite.clone().unwrap();
        assert!(id.starts_with("o%FavoriteAction|"));
        assert!(!on);
        assert!(is_episode_link(p.watch.as_deref().unwrap()));
        let season = p.sections.iter().find(|s| s.title.starts_with("Seizoen 32")).unwrap();
        assert!(!season.lazy);
        let ep = &season.tiles[0];
        assert_eq!(ep.kind, TileKind::Episode);
        assert!(matches!(ep.action(), TileAction::Play(_)));
        let lazy: Vec<_> = p.sections.iter().filter(|s| s.lazy).collect();
        assert!(lazy.len() > 20);
        assert!(lazy.iter().all(|s| s.list_id.is_some() && s.tiles.is_empty()));
    }

    #[test]
    fn search_page_tabs() {
        let p = Page::parse(&fixture("search_thuis")["data"]["page"]).unwrap();
        assert_eq!(p.kind, "SearchResultPage");
        assert!(p.sections.iter().any(|s| s.tab.as_deref() == Some("Alles")));
        assert!(p.sections.iter().any(|s| s.tab.as_deref().is_some_and(|t| t.starts_with("Kijk ("))));
    }

    #[test]
    fn search_landing() {
        let p = Page::parse(&fixture("search_landing")["data"]["page"]).unwrap();
        let cats = p.sections.iter().find(|s| s.title == "Zoek in aanbod").unwrap();
        assert!(cats.tiles.iter().all(|t| t.kind == TileKind::Button));
        assert!(matches!(cats.tiles[0].action(), TileAction::Open(l) if l.starts_with("/vrtmax/zoeken/?facets=")));
        let live = p.sections.iter().find(|s| s.title == "Kijk live").unwrap();
        assert!(live.tiles.iter().all(|t| t.kind == TileKind::Livestream));
        // Radio streams are audio-only tiles and get dropped.
        assert!(!p.sections.iter().any(|s| s.title == "Luister live"));
    }

    #[test]
    fn guide_tile_start_and_length() {
        let id = "$byUyNnxlcGlzb2RlfG8jMzAjMG4jMzQjMG4jMzAjMG8jMTM0OSMxME84IzEwZCMxMzE3OTI3Mjg0NjU3MjAjMTAjMTAjMTMjM3xlcGd8fHx8fGVwaXNvZGV8fHx8fCU=";
        assert_eq!(component_start(id), Some(1_792_728_465));
        assert_eq!(component_start("$bm9wZQ"), None);
        assert_eq!(component_start("plain"), None);
        let meta = [Meta { value: Some("22 min".into()), short_value: None }];
        assert_eq!(minutes(&meta), Some(1320));
    }

    #[test]
    fn links() {
        assert!(is_episode_link("/vrtmax/a-z/thuis/32/thuis-s32a6149/"));
        assert!(!is_episode_link("/vrtmax/a-z/thuis/"));
        assert!(!is_episode_link("/vrtmax/event/abc/"));
        let mut t = Tile {
            kind: TileKind::Episode,
            title: String::new(),
            description: String::new(),
            meta: String::new(),
            link: Some("/vrtmax/a-z/thuis/32/thuis-s32a6149/".into()),
            image: None,
            available: false,
            start: None,
            duration: None,
            progress: None,
            completed: false,
        };
        assert_eq!(t.action(), TileAction::None);
        t.available = true;
        assert!(matches!(t.action(), TileAction::Play(_)));
        t.link = Some("/vrtmax/livestream/video/ketnet/".into());
        assert!(matches!(t.action(), TileAction::Live(c) if c.slug == "ketnet"));
        t.link = Some("/vrtmax/livestream/video/wk-rotterdam/".into());
        assert!(matches!(t.action(), TileAction::Play(_)));
    }
}
