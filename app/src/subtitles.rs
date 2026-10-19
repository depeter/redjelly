//! Subtitles in the player (SPEC §6): provider search, the S panel,
//! download → auto-sync → load → upload, delay memory and auto-sync of the
//! current track.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rj_core::lang::Lang;
use rj_jellyfin::Client;
use rj_jellyfin::models::{Item, MediaSource, StreamType};
use rj_subs::providers::jellyfin::JellyfinServer;
use rj_subs::providers::opensubtitles::OpenSubtitles;
use rj_subs::providers::subdl::Subdl;
use rj_subs::timing::{self, Format};
use rj_subs::{Candidate, Provider, ProviderId, Query, SubtitleFile};
use slint::{ModelRc, SharedString, VecModel};

use crate::{App, PanelItem};

/// Results of a provider search for the item being played.
pub struct SubSearch {
    pub item_id: String,
    pub running: bool,
    pub query: Option<Query>,
    pub results: Vec<Candidate>,
    pub searched: Vec<&'static str>,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelAction {
    None,
    Download(usize),
    AutoSync,
    Search,
    /// Whisper on the current audio track.
    AiTranscribe,
    /// Translate a text subtitle track (Jellyfin index, language).
    AiTranslate(u32, usize),
    /// Keymap editor: rebind this action.
    KeyBinding(rj_input::Context, rj_input::Action),
    /// Switch to this mpv audio track.
    Audio(i64),
    /// Switch to this mpv subtitle track (None = off).
    Subtitle(Option<i64>),
    UpdateInstall,
    UpdateSkip,
    UpdateCheck,
}

/// What the side panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PanelKind {
    #[default]
    Subtitles,
    Audio,
    Keymap,
    Update,
}

#[derive(Default)]
pub struct Panel {
    pub kind: PanelKind,
    pub entries: Vec<(PanelItem, PanelAction)>,
    pub focus: usize,
    /// Keymap editor waiting for a key for this binding.
    pub capturing: Option<(rj_input::Context, rj_input::Action)>,
}

enum Downloaded {
    /// File on disk, ready for mpv; `synced` = median shift applied (ms).
    /// `audio_sync`: no subtitle track to align to, sync to the audio next.
    Local { path: PathBuf, file: SubtitleFile, synced: Option<i64>, audio_sync: bool },
    /// The Jellyfin server fetched and stored it itself.
    OnServer,
}

pub fn header(title: String) -> (PanelItem, PanelAction) {
    (PanelItem { title: title.into(), header: true, ..Default::default() }, PanelAction::None)
}

pub fn entry(title: impl Into<SharedString>, detail: impl Into<SharedString>, action: PanelAction) -> (PanelItem, PanelAction) {
    (PanelItem { title: title.into(), detail: detail.into(), ..Default::default() }, action)
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Builds the provider query from Jellyfin metadata (SPEC §6.1).
async fn build_query(
    client: &Client,
    http: &reqwest::Client,
    item: &Item,
    ms: Option<&MediaSource>,
    stream_url: &str,
    languages: Vec<Lang>,
    want_hash: bool,
) -> Query {
    let episode = item.kind == "Episode";
    let mut q = Query {
        languages,
        title: if episode { item.series_name.clone().unwrap_or_default() } else { item.name.clone() },
        year: item.production_year,
        imdb_id: item.provider_ids.get("Imdb").cloned(),
        tmdb_id: item.provider_ids.get("Tmdb").cloned(),
        season: item.parent_index_number.filter(|_| episode),
        episode: item.index_number.filter(|_| episode),
        file_name: ms.and_then(|m| m.path.as_deref()).map(|p| base_name(p).to_owned()),
        jellyfin_item_id: item.id.clone(),
        ..Default::default()
    };
    if let (true, Some(series_id)) = (episode, &item.series_id)
        && let Ok(series) = client.item(series_id).await {
            q.parent_imdb_id = series.provider_ids.get("Imdb").cloned();
            q.parent_tmdb_id = series.provider_ids.get("Tmdb").cloned();
        }
    if let (true, Some(size)) = (want_hash, ms.and_then(|m| m.size)) {
        match rj_subs::hash::remote_movie_hash(http, stream_url, &client.auth_header(), size).await {
            Ok(h) => q.movie_hash = h,
            Err(e) => tracing::debug!("movie hash: {e}"),
        }
    }
    q
}

/// Fetches a text subtitle track from the server as SRT.
async fn fetch_srt(client: &Client, http: &reqwest::Client, item_id: &str, ms_id: &str, index: u32) -> anyhow::Result<String> {
    let url = client.subtitle_url(item_id, ms_id, index, "srt")?;
    let bytes = http.get(url).header("Authorization", client.auth_header()).send().await?.error_for_status()?.bytes().await?;
    Ok(rj_subs::encoding::to_utf8(&bytes))
}

/// Cue times of a text subtitle track to align against, if the file has one.
async fn reference_cues(
    client: &Client,
    http: &reqwest::Client,
    item_id: &str,
    ms: &MediaSource,
    exclude_index: Option<u32>,
) -> Option<Vec<(i64, i64)>> {
    let mut candidates: Vec<_> = ms
        .media_streams
        .iter()
        .filter(|s| s.kind == StreamType::Subtitle && s.is_text_subtitle_stream && Some(s.index) != exclude_index)
        .collect();
    // Embedded tracks are authored for this exact release: best reference.
    candidates.sort_by_key(|s| s.is_external);
    for s in candidates {
        let Ok(text) = fetch_srt(client, http, item_id, &ms.id, s.index).await else { continue };
        let cues: Vec<_> = timing::parse(&text, Format::Srt).iter().map(|c| (c.start_ms, c.end_ms)).collect();
        if cues.len() >= 20 {
            return Some(cues);
        }
    }
    None
}

/// What audio-based sync needs off the UI thread.
#[derive(Clone)]
struct AudioSync {
    client: Client,
    http: reqwest::Client,
    item_id: String,
    ms: MediaSource,
    models: crate::models::Models,
    work_dir: PathBuf,
}

/// Shows progress as a toast, at most once a second.
pub(crate) fn toast_progress() -> impl Fn(String) + Send + Sync + Clone + 'static {
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now() - Duration::from_secs(5)));
    move |msg: String| {
        let mut l = last.lock().unwrap();
        if l.elapsed() >= Duration::from_secs(1) {
            *l = std::time::Instant::now();
            let _ = slint::invoke_from_event_loop(move || crate::with_app(|app| app.toast(msg)));
        }
    }
}

/// Speech spans (ms) of the audio track matching `lang`: the reference for
/// syncing to the audio. Cached per item and track, so re-syncing is instant.
async fn audio_reference(
    ctx: &AudioSync,
    lang: Option<&Lang>,
    progress: impl Fn(String) + Send + Sync + Clone + 'static,
) -> anyhow::Result<Vec<(i64, i64)>> {
    let index = crate::audio::track_for_sync(&ctx.ms, lang);
    let cache = ctx.work_dir.join(format!("speech-{}-{}.json", ctx.item_id, index.map_or(-1, i64::from)));
    if let Ok(text) = tokio::fs::read_to_string(&cache).await
        && let Ok(spans) = serde_json::from_str::<Vec<(i64, i64)>>(&text) {
            return Ok(spans);
        }
    tokio::fs::create_dir_all(&ctx.work_dir).await?;
    let cancel = Arc::new(AtomicBool::new(false));
    let p = progress.clone();
    ctx.models.ensure_vad(&ctx.http, &move |s, _| p(s), &cancel).await?;
    let wav = ctx.work_dir.join(format!("sync-{}.wav", ctx.item_id));
    let p = progress.clone();
    crate::audio::extract_wav(
        &ctx.client,
        &ctx.item_id,
        &ctx.ms,
        index,
        &wav,
        move |what, f| p(format!("Syncing to the audio · {} {:.0}%", what.to_lowercase(), f * 100.0)),
        cancel.clone(),
    )
    .await?;
    progress("Syncing to the audio · finding speech".into());
    let tool = ctx.models.vad_tool().ok_or_else(|| anyhow::anyhow!("voice detection tool missing after download"))?;
    let model = ctx.models.vad_model();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let wav2 = wav.clone();
    let spans = tokio::task::spawn_blocking(move || rj_ai::vad::speech_spans(&tool, &model, &wav2, threads, &cancel)).await?;
    let _ = tokio::fs::remove_file(&wav).await;
    let spans = spans?;
    anyhow::ensure!(spans.len() >= 20, "too little speech detected in the audio to sync against");
    let _ = tokio::fs::write(&cache, serde_json::to_vec(&spans)?).await;
    Ok(spans)
}

fn median(v: &[i64]) -> i64 {
    let mut v = v.to_vec();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// Re-times subtitle text against reference spans; returns the new text and
/// the median shift (ms).
async fn align_text(text: &str, format: &str, reference: Vec<(i64, i64)>) -> anyhow::Result<(String, i64)> {
    let format = Format::from_ext(format).unwrap_or(Format::Srt);
    let cues = timing::parse(text, format);
    let target: Vec<_> = cues.iter().map(|c| (c.start_ms, c.end_ms)).collect();
    let deltas = tokio::task::spawn_blocking(move || rj_subs::sync::align(&reference, &target)).await?;
    Ok((timing::shift(text, format, &cues, &deltas), median(&deltas)))
}

fn seconds(ms: i64) -> String {
    format!("{:+.2} s", ms as f64 / 1000.0)
}

impl App {
    fn audio_sync_ctx(&self) -> Option<AudioSync> {
        let session = self.session.borrow();
        let s = session.as_ref()?;
        let item_id = s.req.item.as_ref()?.id.clone();
        let ms = s.req.media_source.clone()?;
        let cfg = self.config.borrow().ai.clone();
        Some(AudioSync {
            client: self.client.borrow().clone()?,
            http: self.http.clone(),
            item_id,
            ms,
            models: crate::models::Models::new(self.models_dir.clone(), &cfg),
            work_dir: self.models_dir.join("work"),
        })
    }

    fn providers(&self) -> (Vec<Arc<dyn Provider>>, Vec<String>) {
        let cfg = self.config.borrow().providers.clone();
        let mut list: Vec<Arc<dyn Provider>> = Vec::new();
        let mut problems = Vec::new();
        if cfg.jellyfin_remote
            && let Some(c) = self.client.borrow().clone() {
                list.push(Arc::new(JellyfinServer::new(c)));
            }
        if cfg.subdl {
            match cfg.subdl_api_key.filter(|k| !k.trim().is_empty()) {
                Some(key) => list.push(Arc::new(Subdl::new(self.http.clone(), key))),
                None => problems.push("Subdl needs subdl_api_key in config.toml (free at subdl.com)".into()),
            }
        }
        if cfg.opensubtitles {
            match self.opensubtitles() {
                Some(os) => list.push(os),
                None => problems.push("OpenSubtitles is enabled but opensubtitles_api_key is missing".into()),
            }
        }
        (list, problems)
    }

    /// Kept alive so its login token and quota survive between searches.
    fn opensubtitles(&self) -> Option<Arc<OpenSubtitles>> {
        if let Some(os) = self.opensubs.borrow().clone() {
            return Some(os);
        }
        let cfg = self.config.borrow().providers.clone();
        let key = cfg.opensubtitles_api_key.filter(|k| !k.trim().is_empty())?;
        let creds = cfg.opensubtitles_username.and_then(|u| {
            let pw = self.tokens.get_secret(&crate::secrets::provider_account("opensubtitles", &u))?;
            Some((u, pw))
        });
        let os = Arc::new(OpenSubtitles::new(self.http.clone(), key, env!("CARGO_PKG_VERSION"), creds));
        *self.opensubs.borrow_mut() = Some(os.clone());
        Some(os)
    }

    /// Searches every configured provider for the playing item. `auto`:
    /// triggered by a missing preferred language; report with a toast only.
    pub fn start_sub_search(&self, auto: bool) {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        let Some(item) = s.req.item.clone() else { return };
        let Some(client) = self.client.borrow().clone() else { return };
        let ms = s.req.media_source.clone();
        let stream_url = s.req.url.clone();
        drop(session);

        let (providers, problems) = self.providers();
        let prefs = self.config.borrow().track_preferences();
        let want_hash = providers.iter().any(|p| p.id() == ProviderId::OpenSubtitles);
        let searched: Vec<&'static str> = providers.iter().map(|p| p.id().label()).collect();
        *self.sub_search.borrow_mut() = Some(SubSearch {
            item_id: item.id.clone(),
            running: true,
            query: None,
            results: Vec::new(),
            searched: searched.clone(),
            problems: problems.clone(),
        });
        self.refresh_panel();

        let http = self.http.clone();
        let languages = prefs.languages.clone();
        self.spawn_ui(
            async move {
                let q = build_query(&client, &http, &item, ms.as_ref(), &stream_url, languages, want_hash).await;
                let tasks: Vec<_> = providers
                    .into_iter()
                    .map(|p| {
                        let q = q.clone();
                        tokio::spawn(async move { (p.id(), p.search(&q).await) })
                    })
                    .collect();
                let mut results = Vec::new();
                let mut errors = Vec::new();
                for t in tasks {
                    match t.await {
                        Ok((_, Ok(found))) => results.extend(found),
                        Ok((id, Err(e))) => errors.push(format!("{}: {e}", id.label())),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                (item.id, q, results, errors)
            },
            move |app, (item_id, q, mut results, errors)| {
                rj_subs::rank::rank(&mut results, &q, &prefs);
                let mut search = app.sub_search.borrow_mut();
                let Some(s) = search.as_mut().filter(|s| s.item_id == item_id) else { return };
                s.running = false;
                s.query = Some(q);
                s.results = results;
                s.problems.extend(errors);
                let top = prefs.languages.first().cloned();
                let found_top = top.as_ref().map_or(0, |t| s.results.iter().filter(|c| &c.lang == t).count());
                drop(search);
                if auto
                    && let Some(top) = top {
                        app.toast(if found_top > 0 {
                            format!("{found_top} {} subtitles found · press S to choose", top.name())
                        } else {
                            format!("No {} subtitles found · press S for options", top.name())
                        });
                    }
                app.refresh_panel();
            },
        );
    }

    pub fn open_panel(&self) {
        let item_id = self.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
        let searched = self.sub_search.borrow().as_ref().map(|s| s.item_id.clone());
        *self.panel.borrow_mut() = Some(Panel::default());
        let ui = self.ui();
        ui.set_panel_title("Subtitles".into());
        ui.set_panel_visible(true);
        if item_id.is_some() && searched != item_id {
            self.start_sub_search(false);
        } else {
            self.refresh_panel();
        }
        self.panel_move(0);
        let current = self.player.mpv().get_property::<i64>("sid").ok();
        self.focus_panel_entry(|a| *a == PanelAction::Subtitle(current));
    }

    pub fn close_panel(&self) {
        *self.panel.borrow_mut() = None;
        self.ui().set_panel_visible(false);
    }

    pub fn panel_open(&self) -> bool {
        self.panel.borrow().is_some()
    }

    /// Rebuilds the panel lines from the current search state.
    pub fn refresh_panel(&self) {
        // Bound first: a borrow in the match head lives through the arms,
        // and they borrow the panel mutably.
        let kind = self.panel.borrow().as_ref().map(|p| p.kind);
        match kind {
            None => return,
            Some(PanelKind::Keymap) => return self.refresh_keymap_panel(),
            Some(PanelKind::Update) => return self.refresh_update_panel(),
            Some(PanelKind::Audio) => {
                let entries = self.audio_panel_entries();
                return self.show_panel_entries(entries);
            }
            Some(PanelKind::Subtitles) => {}
        }
        let prefs = self.config.borrow().track_preferences();
        let top = prefs.languages.first().cloned();
        let mut entries = vec![header("ON THIS VIDEO".into())];
        entries.extend(self.subtitle_track_entries());
        let has_sub = self.player.mpv().get_property::<i64>("sid").is_ok();
        entries.push(header("FIX OR GET SUBTITLES".into()));
        let mut sync = entry(
            "Auto-sync current subtitle",
            "If it's early or late · Z / X shift it by hand",
            PanelAction::AutoSync,
        );
        sync.0.dimmed = !has_sub;
        entries.push(sync);
        if let Some(top) = &top {
            entries.extend(self.ai_panel_entries(top));
        }

        let search = self.sub_search.borrow();
        let status = match search.as_ref() {
            None => "Not searched yet".to_owned(),
            Some(s) if s.running => format!("Searching {}…", s.searched.join(", ")),
            Some(s) => {
                let mut st = if s.searched.is_empty() {
                    "No subtitle providers configured".to_owned()
                } else {
                    format!("Searched {} · {} results", s.searched.join(", "), s.results.len())
                };
                for p in &s.problems {
                    st.push_str(&format!("\n{p}"));
                }
                st
            }
        };
        if let Some(s) = search.as_ref().filter(|s| !s.running) {
            for lang in &prefs.languages {
                let found: Vec<(usize, &Candidate)> = s.results.iter().enumerate().filter(|(_, c)| &c.lang == lang).collect();
                entries.push(header(format!("{} — {} FOUND", lang.name().to_uppercase(), found.len())));
                for (i, c) in found {
                    let mut detail = vec![c.provider.label().to_owned()];
                    if c.downloads > 0 {
                        detail.push(format!("{} downloads", c.downloads));
                    }
                    if c.provider.costs_quota() {
                        detail.push("uses daily quota".into());
                    }
                    if c.provider == ProviderId::JellyfinServer {
                        detail.push("saved by the server".into());
                    }
                    let badge = if c.hash_match {
                        "EXACT MATCH"
                    } else if c.hearing_impaired {
                        "SDH"
                    } else if c.forced {
                        "FORCED"
                    } else {
                        ""
                    };
                    let title = if c.release.is_empty() { "(no release name)".to_owned() } else { c.release.clone() };
                    let mut e = entry(title, detail.join(" · "), PanelAction::Download(i));
                    e.0.badge = badge.into();
                    entries.push(e);
                }
            }
        }
        drop(search);
        entries.push(header(String::new()));
        entries.push(entry("Search again", "", PanelAction::Search));

        self.ui().set_panel_status(status.into());
        self.show_panel_entries(entries);
    }

    pub(crate) fn show_panel_entries(&self, entries: Vec<(PanelItem, PanelAction)>) {
        let items: Vec<PanelItem> = entries.iter().map(|(i, _)| i.clone()).collect();
        self.ui().set_panel_items(ModelRc::new(VecModel::from(items)));
        let mut panel = self.panel.borrow_mut();
        let Some(p) = panel.as_mut() else { return };
        p.entries = entries;
        drop(panel);
        self.panel_move(0);
    }

    /// Puts the panel cursor on the first entry whose action matches.
    pub fn focus_panel_entry(&self, matches: impl Fn(&PanelAction) -> bool) {
        let mut panel = self.panel.borrow_mut();
        let Some(p) = panel.as_mut() else { return };
        let Some(i) = p.entries.iter().position(|(_, a)| matches(a)) else { return };
        p.focus = i;
        drop(panel);
        self.panel_move(0);
    }

    /// Moves the panel cursor by `delta`, skipping headers.
    pub fn panel_move(&self, delta: i32) {
        let mut panel = self.panel.borrow_mut();
        let Some(p) = panel.as_mut() else { return };
        if p.entries.is_empty() {
            return;
        }
        let selectable = |i: usize| p.entries.get(i).is_some_and(|(item, _)| !item.header);
        let mut i = p.focus.min(p.entries.len() - 1) as i32;
        let step = if delta < 0 { -1 } else { 1 };
        if delta != 0 {
            i += step;
        }
        while i >= 0 && (i as usize) < p.entries.len() && !selectable(i as usize) {
            i += step;
        }
        if i < 0 || i as usize >= p.entries.len() {
            // Nothing selectable that way: stay put (or find the first one).
            i = p.focus as i32;
            if !selectable(i as usize) {
                i = (0..p.entries.len()).find(|&j| selectable(j)).unwrap_or(0) as i32;
            }
        }
        p.focus = i as usize;
        self.ui().set_panel_focus(i);
    }

    pub fn panel_click(&self, index: usize) {
        if let Some(p) = self.panel.borrow_mut().as_mut() {
            p.focus = index;
        }
        self.panel_activate();
    }

    pub fn panel_activate(&self) {
        let action = self.panel.borrow().as_ref().and_then(|p| p.entries.get(p.focus).map(|e| e.1));
        match action {
            Some(PanelAction::Download(i)) => self.download_subtitle(i),
            Some(PanelAction::AutoSync) => self.auto_sync_current(),
            Some(PanelAction::Search) => self.start_sub_search(false),
            Some(PanelAction::AiTranscribe) => self.queue_ai_from_player(None),
            Some(PanelAction::AiTranslate(index, lang)) => self.queue_ai_from_player(Some((index, lang))),
            Some(PanelAction::KeyBinding(ctx, action)) => self.start_capture(ctx, action),
            Some(PanelAction::Audio(id)) => self.choose_audio(id),
            Some(PanelAction::Subtitle(id)) => self.choose_subtitle(id),
            Some(PanelAction::UpdateInstall) => self.install_update(),
            Some(PanelAction::UpdateSkip) => self.skip_update(),
            Some(PanelAction::UpdateCheck) => self.check_for_update(true),
            Some(PanelAction::None) | None => {}
        }
    }

    /// AI section of the panel: job status for this item, or ways to start one.
    fn ai_panel_entries(&self, top: &Lang) -> Vec<(PanelItem, PanelAction)> {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return Vec::new() };
        let Some(item) = s.req.item.as_ref() else { return Vec::new() };
        if let Some(status) = self.job_for(&item.id) {
            return vec![entry(format!("AI {} subtitles", top.name()), status, PanelAction::None)];
        }
        let mut out = vec![entry(
            format!("Generate {} subtitles from audio (AI)", top.name()),
            "Whisper on your GPU, from the current audio track · a few minutes",
            PanelAction::AiTranscribe,
        )];
        let pairs = crate::models::Models::opus_pairs();
        if let Some(ms) = &s.req.media_source {
            for st in ms.media_streams.iter().filter(|st| st.kind == StreamType::Subtitle && st.is_text_subtitle_stream) {
                let Some(from) = st.language.as_deref().and_then(Lang::parse) else { continue };
                if pairs.iter().any(|(f, t)| *f == from && t == top) {
                    let lang_index = pairs.iter().position(|(f, t)| *f == from && t == top).unwrap_or(0);
                    out.push(entry(
                        format!("Translate {} subtitle to {} (AI)", from.name(), top.name()),
                        "Keeps the original timing · about a minute",
                        PanelAction::AiTranslate(st.index, lang_index),
                    ));
                    break;
                }
            }
        }
        out
    }

    fn queue_ai_from_player(&self, translate: Option<(u32, usize)>) {
        let session = self.session.borrow();
        let Some(item) = session.as_ref().and_then(|s| s.req.item.clone()) else { return };
        drop(session);
        let kind = match translate {
            Some((sub_index, pair)) => {
                let Some((from, _)) = crate::models::Models::opus_pairs().into_iter().nth(pair) else { return };
                crate::ai::JobKind::TranslateSub { sub_index, from }
            }
            None => {
                let mpv = self.player.mpv();
                let audio_index =
                    mpv.get_property::<i64>("current-tracks/audio/ff-index").ok().filter(|i| *i >= 0).map(|i| i as u32);
                let spoken = mpv.get_property::<String>("current-tracks/audio/lang").ok().and_then(|l| Lang::parse(&l));
                crate::ai::JobKind::Transcribe { audio_index, spoken }
            }
        };
        self.toast("Queued AI subtitles · it loads here automatically when done");
        self.queue_ai(item.id.clone(), crate::playback::display_title(&item), kind);
    }

    fn download_subtitle(&self, index: usize) {
        let (candidate, query) = {
            let search = self.sub_search.borrow();
            let Some(s) = search.as_ref() else { return };
            let (Some(c), Some(q)) = (s.results.get(index), s.query.clone()) else { return };
            (c.clone(), q)
        };
        let (providers, _) = self.providers();
        let Some(provider) = providers.into_iter().find(|p| p.id() == candidate.provider) else { return };
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        let Some(item_id) = s.req.item.as_ref().map(|i| i.id.clone()) else { return };
        let ms = s.req.media_source.clone();
        drop(session);
        let Some(client) = self.client.borrow().clone() else { return };
        let http = self.http.clone();
        let auto_sync = self.config.borrow().subtitles.auto_sync_non_hash;
        let cache = self.sub_cache.clone();
        let existing_external = ms.as_ref().map_or(0, |m| {
            m.media_streams.iter().filter(|s| s.kind == StreamType::Subtitle && s.is_external).count()
        });

        self.close_panel();
        self.toast(format!("Downloading {} subtitle from {}…", candidate.lang.name(), candidate.provider.label()));
        let c = candidate.clone();
        self.spawn_ui(
            async move {
                let Some(mut file) = provider.download(&query, &c).await? else { return anyhow::Ok(Downloaded::OnServer) };
                let (mut synced, mut audio_sync) = (None, false);
                if auto_sync && !c.hash_match {
                    let reference = match &ms {
                        Some(m) => reference_cues(&client, &http, &item_id, m, None).await,
                        None => None,
                    };
                    match reference {
                        Some(reference) => {
                            let (text, shift) = align_text(&file.text, &file.format, reference).await?;
                            file.text = text;
                            synced = Some(shift);
                        }
                        // No subtitle track to align to: load now, sync to the audio after.
                        None => audio_sync = ms.is_some(),
                    }
                }
                tokio::fs::create_dir_all(&cache).await?;
                let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let path = cache.join(format!("{item_id}.{}.{n}.{}", file.lang, file.format));
                tokio::fs::write(&path, &file.text).await?;
                Ok(Downloaded::Local { path, file, synced, audio_sync })
            },
            move |app, result| match result {
                Ok(Downloaded::Local { path, file, synced, audio_sync }) => {
                    let track = app.load_downloaded(&path, &file, &candidate, synced, audio_sync);
                    if audio_sync {
                        app.sync_download_to_audio(file, candidate, track);
                    } else {
                        app.upload_subtitle_file(file);
                    }
                }
                Ok(Downloaded::OnServer) => app.wait_for_server_subtitle(candidate.lang.clone(), existing_external),
                Err(e) => app.toast(format!("Download failed: {e:#}")),
            },
        );
    }

    /// Loads a downloaded file into mpv and selects it; returns its track id.
    fn load_downloaded(&self, path: &std::path::Path, file: &SubtitleFile, c: &Candidate, synced: Option<i64>, audio_sync: bool) -> Option<i64> {
        let title = format!("{} · {}", c.provider.label(), c.release);
        let id = match self.player.add_subtitle(&path.to_string_lossy(), &title, file.lang.as_str()) {
            Ok(id) => {
                let _ = self.player.set_subtitle(Some(id));
                let _ = self.player.mpv().set_property("sub-delay", 0.0);
                id
            }
            Err(e) => {
                self.toast(format!("mpv couldn't load the subtitle: {e}"));
                return None;
            }
        };
        let mut msg = format!("{} subtitle loaded", file.lang.name());
        match synced {
            Some(ms) if ms.abs() >= 50 => msg.push_str(&format!(" · auto-synced {}", seconds(ms))),
            Some(_) => msg.push_str(" · timing already matched"),
            None if audio_sync => msg.push_str(" · syncing to the audio in the background…"),
            None => {}
        }
        self.toast(msg);
        Some(id)
    }

    /// Re-times a just-loaded download against the audio, swaps the synced
    /// version in, and uploads that one (the original if syncing fails).
    fn sync_download_to_audio(&self, mut file: SubtitleFile, c: Candidate, loaded_track: Option<i64>) {
        let Some(ctx) = self.audio_sync_ctx() else { return self.upload_subtitle_file(file) };
        let item_id = ctx.item_id.clone();
        let cache = self.sub_cache.clone();
        let progress = toast_progress();
        self.spawn_ui(
            async move {
                let result: anyhow::Result<(PathBuf, i64)> = async {
                    let reference = audio_reference(&ctx, Some(&file.lang), progress).await?;
                    let (text, shift) = align_text(&file.text, &file.format, reference).await?;
                    let path = cache.join(format!("{}.{}.synced.{}", ctx.item_id, file.lang, file.format));
                    tokio::fs::write(&path, &text).await?;
                    file.text = text;
                    Ok((path, shift))
                }
                .await;
                (file, result)
            },
            move |app, (file, result)| {
                match result {
                    Ok((path, shift)) => {
                        let playing = app.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
                        if playing.as_deref() == Some(item_id.as_str()) {
                            let title = format!("{} · {} · synced", c.provider.label(), c.release);
                            if let Ok(id) = app.player.add_subtitle(&path.to_string_lossy(), &title, file.lang.as_str()) {
                                let _ = app.player.set_subtitle(Some(id));
                                if let Some(old) = loaded_track {
                                    let _ = app.player.mpv().command("sub-remove", &[&old.to_string()]);
                                }
                            }
                        }
                        app.toast(if shift.abs() >= 50 {
                            format!("Subtitle synced to the audio · shifted {}", seconds(shift))
                        } else {
                            "Subtitle checked against the audio · timing already matched".to_owned()
                        });
                    }
                    Err(e) => app.toast(format!("Couldn't sync to the audio ({e:#}); keeping the downloaded timing")),
                }
                app.upload_subtitle_file(file);
            },
        );
    }

    fn upload_subtitle_file(&self, file: SubtitleFile) {
        if !self.config.borrow().subtitles.upload_to_server {
            return;
        }
        let item_id = self.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
        let (Some(item_id), Some(client)) = (item_id, self.client.borrow().clone()) else { return };
        let (lang, format, text, forced, hi) =
            (file.lang.iso639_2b().to_owned(), file.format.clone(), file.text.clone(), file.forced, file.hearing_impaired);
        self.spawn_ui(
            async move { client.upload_subtitle(&item_id, &lang, &format, text.as_bytes(), forced, hi).await },
            |app, r| match r {
                Ok(()) => app.toast("Subtitle saved to Jellyfin for every device"),
                Err(e) => app.toast(format!("Kept locally; uploading to Jellyfin failed: {e}")),
            },
        );
    }

    /// The server provider stores the file itself; poll the item until the
    /// new external track shows up, then load it from the server.
    fn wait_for_server_subtitle(&self, lang: Lang, existing_external: usize) {
        let session = self.session.borrow();
        let Some(item_id) = session.as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone())) else { return };
        drop(session);
        let Some(client) = self.client.borrow().clone() else { return };
        self.toast(format!("Jellyfin is fetching the {} subtitle…", lang.name()));
        let wanted = lang.clone();
        self.spawn_ui(
            async move {
                for _ in 0..12 {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let Ok(item) = client.item(&item_id).await else { continue };
                    let Some(ms) = item.media_sources.first() else { continue };
                    let external: Vec<_> =
                        ms.media_streams.iter().filter(|s| s.kind == StreamType::Subtitle && s.is_external).collect();
                    if external.len() > existing_external {
                        let s = external.iter().rev().find(|s| s.language.as_deref().and_then(Lang::parse).as_ref() == Some(&wanted)).or(external.last())?;
                        let url = match &s.delivery_url {
                            Some(u) => client.url(u).ok()?,
                            None => client.subtitle_url(&item_id, &ms.id, s.index, "srt").ok()?,
                        };
                        return Some(url.to_string());
                    }
                }
                None
            },
            move |app, url| match url {
                Some(url) => match app.player.add_subtitle(&url, "Jellyfin", lang.as_str()) {
                    Ok(id) => {
                        let _ = app.player.set_subtitle(Some(id));
                        app.toast(format!("{} subtitle loaded from Jellyfin", lang.name()));
                    }
                    Err(e) => app.toast(format!("mpv couldn't load the subtitle: {e}")),
                },
                None => app.toast("Jellyfin didn't add the subtitle (check the server log)"),
            },
        );
    }

    /// Ctrl+Z / panel: aligns the current subtitle to another text track (or
    /// to the audio when there is none) and applies the result as a
    /// (remembered) subtitle delay.
    pub fn auto_sync_current(&self) {
        let mpv = self.player.mpv();
        let prop = |k: &str| mpv.get_property::<String>(k).ok().filter(|v| !v.is_empty());
        if prop("current-tracks/sub/id").is_none() {
            return self.toast("No subtitle selected to sync");
        }
        let external = prop("current-tracks/sub/external-filename");
        let ff_index = mpv.get_property::<i64>("current-tracks/sub/ff-index").ok().filter(|i| *i >= 0).map(|i| i as u32);
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        let (Some(item_id), Some(ms)) = (s.req.item.as_ref().map(|i| i.id.clone()), s.req.media_source.clone()) else {
            return self.toast("Auto-sync needs a video from Jellyfin");
        };
        drop(session);
        let Some(client) = self.client.borrow().clone() else { return };
        let http = self.http.clone();
        let sub_lang = prop("current-tracks/sub/lang").and_then(|l| Lang::parse(&l));
        let Some(audio_ctx) = self.audio_sync_ctx() else { return };
        let progress = toast_progress();
        self.close_panel();
        self.toast("Syncing subtitle…");
        self.spawn_ui(
            async move {
                // Text of the current track: external file/URL, or the
                // embedded track via the server (ff-index = Jellyfin index).
                let (text, exclude) = match &external {
                    Some(src) if src.starts_with("http") => {
                        let b = http.get(src).header("Authorization", client.auth_header()).send().await?.bytes().await?;
                        (rj_subs::encoding::to_utf8(&b), None)
                    }
                    Some(path) => (rj_subs::encoding::to_utf8(&tokio::fs::read(path).await?), None),
                    None => {
                        let idx = ff_index.ok_or_else(|| anyhow::anyhow!("can't identify the embedded track"))?;
                        (fetch_srt(&client, &http, &item_id, &ms.id, idx).await?, Some(idx))
                    }
                };
                let format = rj_subs::encoding::detect_format(&text).and_then(Format::from_ext).unwrap_or(Format::Srt);
                let target: Vec<_> = timing::parse(&text, format).iter().map(|c| (c.start_ms, c.end_ms)).collect();
                let (reference, against) = match reference_cues(&client, &http, &item_id, &ms, exclude).await {
                    Some(r) => (r, "another subtitle track"),
                    None => (audio_reference(&audio_ctx, sub_lang.as_ref(), progress).await?, "the audio"),
                };
                let deltas = tokio::task::spawn_blocking(move || rj_subs::sync::align(&reference, &target)).await?;
                anyhow::Ok((median(&deltas), against))
            },
            |app, result| match result {
                Ok((delta, against)) => {
                    let _ = app.player.mpv().set_property("sub-delay", delta as f64 / 1000.0);
                    app.remember_sub_delay();
                    app.toast(format!("Subtitle synced to {against} · delay {}", seconds(delta)));
                }
                Err(e) => app.toast(format!("Auto-sync failed: {e:#}")),
            },
        );
    }

    /// Stable key for the current subtitle track (delay memory).
    fn current_sub_key(&self) -> Option<String> {
        let mpv = self.player.mpv();
        mpv.get_property::<String>("current-tracks/sub/id").ok()?;
        if let Ok(src) = mpv.get_property::<String>("current-tracks/sub/external-filename")
            && !src.is_empty() {
                // Server URLs carry the track index in the path; drop the query.
                return Some(format!("ext:{}", src.split('?').next().unwrap_or(&src)));
            }
        let idx = mpv.get_property::<i64>("current-tracks/sub/ff-index").ok()?;
        Some(format!("emb:{idx}"))
    }

    pub fn remember_sub_delay(&self) {
        let item = self.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
        let (Some(item), Some(key)) = (item, self.current_sub_key()) else { return };
        let ms = (self.player.mpv().get_property::<f64>("sub-delay").unwrap_or(0.0) * 1000.0).round() as i64;
        let mut state = self.state.borrow_mut();
        state.set_sub_delay(&item, &key, ms);
        if let Err(e) = state.save(&self.state_path) {
            tracing::warn!("saving state: {e}");
        }
    }

    /// Re-applies a remembered delay whenever the subtitle track changes.
    pub fn check_sub_track_change(&self) {
        let sid = self.player.mpv().get_property::<String>("sid").unwrap_or_default();
        if *self.last_sid.borrow() == sid {
            return;
        }
        *self.last_sid.borrow_mut() = sid;
        let item = self.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
        let (Some(item), Some(key)) = (item, self.current_sub_key()) else { return };
        let ms = self.state.borrow().sub_delay(&item, &key);
        let _ = self.player.mpv().set_property("sub-delay", ms as f64 / 1000.0);
        if ms != 0 {
            self.toast(format!("Remembered subtitle delay {}", seconds(ms)));
        }
    }
}
