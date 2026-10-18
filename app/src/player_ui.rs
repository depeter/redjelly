//! Playback screen: starting items, track rules + memory, OSD, progress
//! reports, OS media controls.

use std::time::{Duration, Instant};

use rj_core::lang::Lang;
use rj_core::tracks;
use rj_input::Action;
use rj_input::media_controls::NowPlaying;
use rj_jellyfin::models::PlaybackReport;
use rj_core::config::SegmentAction;
use rj_jellyfin::models::SegmentType;
use rj_player::libmpv2::events::Event;
use slint::{ComponentHandle, SharedString};

use crate::playback::{self, PlayRequest, Session, format_time};
use crate::{App, SCREEN_PLAYER};

const OSD_DURATION: Duration = Duration::from_secs(3);
/// Longer while someone is moving through the buttons with keys.
const OSD_FOCUSED_DURATION: Duration = Duration::from_secs(8);
/// Height of the bottom bar in ui/app.slint.
const BAR_HEIGHT: f32 = 150.0;

type ReportFn = fn(
    rj_jellyfin::Client,
    PlaybackReport,
) -> std::pin::Pin<Box<dyn Future<Output = Result<(), rj_jellyfin::Error>> + Send>>;

const REPORT_START: ReportFn = |c, r| Box::pin(async move { c.report_start(&r).await });
const REPORT_PROGRESS: ReportFn = |c, r| Box::pin(async move { c.report_progress(&r).await });
pub const REPORT_STOPPED: ReportFn = |c, r| Box::pin(async move { c.report_stopped(&r).await });

impl App {
    pub fn play_item(&self, id: String, from_start: bool) {
        let Some(client) = self.client.borrow().clone() else { return };
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.toast("Loading…");
        self.spawn_ui(async move { playback::prepare(&client, &id, from_start).await }, |app, result| {
            app.busy.set(false);
            app.clear_toast();
            match result {
                Ok(req) => app.start(req),
                Err(e) => app.toast(format!("Can't play: {e:#}")),
            }
        });
    }

    pub fn start(&self, req: PlayRequest) {
        if !self.video_ready.get() {
            *self.pending.borrow_mut() = Some(req);
            return;
        }
        let auth = self.client.borrow().as_ref().map(|c| c.auth_header()).unwrap_or_default();
        if let Err(e) = self.player.load(&req.url, &auth, req.start_secs) {
            self.toast(format!("mpv refused the file: {e}"));
            return;
        }
        let ui = self.ui();
        ui.set_player_title(req.title.as_str().into());
        ui.set_player_time(SharedString::new());
        ui.set_player_total(SharedString::new());
        ui.set_player_has_previous(false);
        ui.set_player_has_next(false);
        ui.set_player_progress(0.0);
        ui.set_screen(SCREEN_PLAYER);
        ui.invoke_focus_keys();
        if req.start_secs > 30.0 {
            self.toast(format!("Resumed at {} · press 0 to start over", format_time(req.start_secs)));
        }
        self.show_osd();

        let memory_key = req.item.as_ref().map(|i| i.series_id.clone().unwrap_or_else(|| i.id.clone()));
        let item_id = req.item.as_ref().map(|i| i.id.clone());
        if self.sub_search.borrow().as_ref().map(|s| Some(&s.item_id) != item_id.as_ref()).unwrap_or(false) {
            *self.sub_search.borrow_mut() = None;
        }
        self.last_sid.borrow_mut().clear();
        self.update_media_controls(&req);
        self.load_extras(&req);
        *self.session.borrow_mut() = Some(Session {
            req,
            memory_key,
            started: false,
            last_report: Instant::now(),
            segments: Vec::new(),
            skipped: Vec::new(),
            next: None,
            prev: None,
            countdown: None,
            countdown_cancelled: false,
            chain_to: None,
        });
    }

    /// Fetches skippable segments and neighbouring episodes in the background.
    fn load_extras(&self, req: &PlayRequest) {
        let (Some(item), Some(client)) = (req.item.clone(), self.client.borrow().clone()) else { return };
        self.spawn_ui(
            async move {
                let segments = client.media_segments(&item.id).await.map(|r| r.items).unwrap_or_default();
                let adjacent = match &item.series_id {
                    Some(series) => client.adjacent_episodes(series, &item.id).await.map(|r| r.items).unwrap_or_default(),
                    None => Vec::new(),
                };
                (item.id, segments, adjacent)
            },
            |app, (id, segments, adjacent)| {
                let mut session = app.session.borrow_mut();
                let Some(s) = session.as_mut().filter(|s| s.req.item.as_ref().is_some_and(|i| i.id == id)) else { return };
                let secs = |t: i64| t as f64 / rj_jellyfin::models::TICKS_PER_SECOND as f64;
                s.segments = segments
                    .into_iter()
                    .filter(|g| g.end_ticks > g.start_ticks)
                    .map(|g| playback::Segment { kind: g.kind, start: secs(g.start_ticks), end: secs(g.end_ticks) })
                    .collect();
                if let Some(pos) = adjacent.iter().position(|e| e.id == id) {
                    s.prev = pos.checked_sub(1).and_then(|p| adjacent.get(p).cloned());
                    s.next = adjacent.get(pos + 1).cloned();
                }
                let ui = app.ui();
                ui.set_player_has_previous(s.prev.is_some());
                ui.set_player_has_next(s.next.is_some());
                tracing::info!(segments = s.segments.len(), next = s.next.is_some(), "playback extras");
            },
        );
    }

    fn segment_action(&self, kind: SegmentType) -> SegmentAction {
        let cfg = self.config.borrow();
        match kind {
            SegmentType::Intro => cfg.playback.intro,
            SegmentType::Recap => cfg.playback.recap,
            SegmentType::Outro => cfg.playback.outro,
            SegmentType::Preview => cfg.playback.preview,
            SegmentType::Commercial => cfg.playback.commercial,
            SegmentType::Unknown => SegmentAction::Off,
        }
    }

    /// Skip buttons, auto-skips and the next-episode countdown (SPEC §5.3).
    fn update_segments(&self, pos: f64, dur: f64, now: Instant) {
        let countdown_secs = self.config.borrow().playback.next_episode_countdown;
        let mut skip_to = None;
        let mut skip_label = String::new();
        let mut toast = None;
        let mut play_next = None;
        let next_label;
        {
            let mut session = self.session.borrow_mut();
            let Some(s) = session.as_mut() else { return };
            let active = s.segments.iter().position(|g| pos >= g.start && pos < g.end - 0.5);
            let has_outro = s.segments.iter().any(|g| g.kind == SegmentType::Outro);
            let autoplay = countdown_secs > 0 && s.next.is_some() && !s.countdown_cancelled;
            let in_credits = match active {
                Some(i) => s.segments[i].kind == SegmentType::Outro,
                None => !has_outro && dur > 60.0 && dur - pos <= 30.0,
            };
            if autoplay && in_credits {
                s.countdown.get_or_insert(now + Duration::from_secs(countdown_secs.into()));
            } else if let Some(i) = active {
                let g = &s.segments[i];
                let what = match g.kind {
                    SegmentType::Intro => "intro",
                    SegmentType::Recap => "recap",
                    SegmentType::Outro => "credits",
                    SegmentType::Preview => "preview",
                    SegmentType::Commercial => "ad",
                    SegmentType::Unknown => "",
                };
                match self.segment_action(g.kind) {
                    SegmentAction::Auto if !s.skipped.contains(&i) => {
                        s.skipped.push(i);
                        skip_to = Some(g.end);
                        toast = Some(format!("Skipped {what}"));
                    }
                    SegmentAction::Ask => skip_label = format!("Skip {what}  ⏎"),
                    _ => {}
                }
            }
            next_label = match (s.countdown, &s.next) {
                (Some(deadline), Some(next)) => {
                    let left = deadline.saturating_duration_since(now).as_secs_f32().ceil() as u32;
                    if left == 0 {
                        play_next = Some(next.id.clone());
                    }
                    format!(
                        "Next: {} · in {left} s\nEnter: play now · Esc: keep watching",
                        playback::display_title(next)
                    )
                }
                _ => String::new(),
            };
        }
        let ui = self.ui();
        // The ⏎ glyph may be missing from fonts; keep the label plain.
        ui.set_player_skip(skip_label.replace("  ⏎", "").into());
        ui.set_player_next(next_label.into());
        if let Some(t) = skip_to {
            let _ = self.player.mpv().command("seek", &[&t.to_string(), "absolute"]);
        }
        if let Some(msg) = toast {
            self.toast(msg);
        }
        if let Some(id) = play_next {
            self.chain_to(id);
        }
    }

    /// Stops this item and plays `id` next (episode chaining).
    fn chain_to(&self, id: String) {
        if let Some(s) = self.session.borrow_mut().as_mut() {
            s.chain_to = Some(id);
            s.countdown = None;
        }
        let _ = self.player.mpv().command("stop", &[]);
    }

    /// Returns false when there was nothing to skip.
    pub fn skip_segment_or_play_next(&self) -> bool {
        let (countdown_next, segment_end) = {
            let session = self.session.borrow();
            let Some(s) = session.as_ref() else { return false };
            let pos = self.player.position();
            (
                s.countdown.and(s.next.as_ref().map(|n| n.id.clone())),
                s.segments.iter().find(|g| pos >= g.start && pos < g.end - 0.5).map(|g| g.end),
            )
        };
        if let Some(id) = countdown_next {
            self.chain_to(id);
        } else if let Some(end) = segment_end {
            // Only when the button is on screen (not for auto-skipped kinds).
            if self.ui().get_player_skip().is_empty() {
                return false;
            }
            let _ = self.player.mpv().command("seek", &[&end.to_string(), "absolute"]);
        } else {
            return false;
        }
        true
    }

    fn play_adjacent(&self, next: bool) {
        let target = self.session.borrow().as_ref().and_then(|s| if next { s.next.clone() } else { s.prev.clone() });
        match target {
            Some(item) => {
                self.toast(format!("Playing {}", playback::display_title(&item)));
                self.chain_to(item.id);
            }
            None => self.toast(if next { "No next episode" } else { "No previous episode" }),
        }
    }

    fn update_media_controls(&self, req: &PlayRequest) {
        let mut media = self.media.borrow_mut();
        let Some(m) = media.as_mut() else { return };
        let client = self.client.borrow();
        let cover = req.item.as_ref().zip(client.as_ref()).and_then(|(item, c)| {
            let r = item.poster()?;
            c.image_url(&r.item_id, r.kind, &r.tag, 400).ok().map(|u| u.to_string())
        });
        let series = req.item.as_ref().and_then(|i| i.series_name.clone());
        let duration = req.item.as_ref().and_then(|i| i.run_time_ticks).map(|t| Duration::from_nanos(t as u64 * 100));
        let np = NowPlaying {
            title: req.item.as_ref().map_or(req.title.as_str(), |i| i.name.as_str()),
            subtitle: series.as_deref(),
            cover_url: cover.as_deref(),
            duration,
        };
        if let Err(e) = m.set_now_playing(&np) {
            tracing::debug!("media controls: {e:?}");
        }
    }

    /// Adds server subtitles, applies the track rules (SPEC §5.1), reports start.
    fn on_file_loaded(&self) {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        for (url, title, lang) in &s.req.external_subs {
            if let Err(e) = self.player.add_subtitle(url, title, lang) {
                tracing::warn!("adding subtitle {url}: {e}");
            }
        }

        let all = self.player.tracks();
        let audio: Vec<_> = all.iter().filter(|t| t.kind == "audio").map(|t| t.to_audio()).collect();
        let subs: Vec<_> = all.iter().filter(|t| t.kind == "sub").map(|t| t.to_subtitle()).collect();
        let prefs = self.config.borrow().track_preferences();
        let remembered = s.memory_key.as_deref().map(|k| self.state.borrow().remembered(k)).unwrap_or_default();
        let sel = tracks::select(&audio, &subs, &prefs, &remembered);
        tracing::info!(?sel, ?remembered, tracks = all.len(), "track selection");

        if let Some(a) = sel.audio {
            let _ = self.player.set_audio(a.into());
        }
        let _ = self.player.set_subtitle(sel.subtitle.map(i64::from));

        let lang_of = |id: Option<u32>| {
            id.and_then(|id| all.iter().find(|t| t.id == i64::from(id)))
                .map(|t| t.lang.clone().unwrap_or_else(|| "unknown".into()))
        };
        let mut msg = format!(
            "Audio: {} · Subtitles: {}",
            lang_of(sel.audio).unwrap_or_else(|| "-".into()),
            lang_of(sel.subtitle).unwrap_or_else(|| "off".into())
        );
        if remembered.audio.is_some() || remembered.subtitle.is_some() {
            msg.push_str(" (remembered for this show)");
        }
        let auto_search = sel.search_for.is_some()
            && s.req.item.is_some()
            && self.config.borrow().subtitles.auto_search;
        if let Some(missing) = &sel.search_for {
            msg.push_str(&format!(" · no {} subtitles, searching…", missing.name()));
        }
        drop(session);
        self.toast(msg);
        self.report(REPORT_START);
        self.apply_remembered_shader();
        if auto_search {
            self.start_sub_search(true);
        }
    }

    pub fn report(&self, send: ReportFn) {
        let session = self.session.borrow();
        let (Some(s), Some(client)) = (session.as_ref(), self.client.borrow().clone()) else { return };
        let Some(report) = s.report(self.player.position(), self.player.paused()) else { return };
        let fut = send(client, report);
        self.rt.spawn(async move {
            if let Err(e) = fut.await {
                tracing::warn!("playback report failed: {e}");
            }
        });
    }

    fn end_session(&self) {
        self.report(REPORT_STOPPED);
        let chained = self.session.borrow_mut().take().and_then(|s| s.chain_to);
        let ui = self.ui();
        ui.set_player_skip(SharedString::new());
        ui.set_player_next(SharedString::new());
        if let Some(id) = chained {
            // Straight into the next episode, staying in the player.
            return self.play_item(id, false);
        }
        self.close_panel();
        self.release_controls();
        self.stats.set(false);
        self.raise_subtitles(false);
        if let Some(m) = self.media.borrow_mut().as_mut() {
            let _ = m.set_state(None, Duration::ZERO);
        }
        let ui = self.ui();
        ui.set_player_stats(SharedString::new());
        if self.standalone {
            let _ = slint::quit_event_loop();
            return;
        }
        // Back to where playback started, refreshed for the new progress.
        self.show_top();
        self.reload_top();
    }

    pub fn show_osd(&self) {
        let d = if self.controls_engaged() { OSD_FOCUSED_DURATION } else { OSD_DURATION };
        self.osd_until.set(Instant::now() + d);
        self.ui().set_player_osd(true);
    }

    /// Moves subtitles above the player bar while it's visible.
    fn raise_subtitles(&self, raise: bool) {
        if raise == self.subs_raised.get() {
            return;
        }
        self.subs_raised.set(raise);
        let pos = if raise {
            let ui = self.ui();
            let w = ui.window();
            let logical_height = w.size().height as f32 / w.scale_factor();
            (100.0 - BAR_HEIGHT / logical_height * 100.0).clamp(50.0_f32, 100.0) as i64
        } else {
            100
        };
        let _ = self.player.mpv().set_property("sub-pos", pos);
    }

    /// Runs every 100 ms: drains mpv events, updates the OSD, sends progress
    /// reports and media-control updates.
    pub fn player_tick(&self, now: Instant) {
        let mpv = self.player.mpv();
        while let Some(ev) = mpv.wait_event(0.0) {
            match ev {
                Ok(Event::StartFile) => {
                    if let Some(s) = self.session.borrow_mut().as_mut() {
                        s.started = true;
                    }
                }
                Ok(Event::FileLoaded) => self.on_file_loaded(),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("mpv: {e}");
                    self.toast(format!("Playback error: {e}"));
                }
            }
        }

        let started = self.session.borrow().as_ref().map(|s| s.started);
        match started {
            None => return,
            Some(true) if self.player.idle() => {
                // Reached the end with autoplay on: continue with the next episode.
                let autoplay = self.config.borrow().playback.next_episode_countdown > 0;
                if let Some(s) = self.session.borrow_mut().as_mut()
                    && s.chain_to.is_none() && autoplay && !s.countdown_cancelled {
                        s.chain_to = s.next.as_ref().map(|n| n.id.clone());
                    }
                return self.end_session();
            }
            _ => {}
        }

        self.check_sub_track_change();
        let ui = self.ui();
        let (pos, dur, paused) = (self.player.position(), self.player.duration(), self.player.paused());
        ui.set_player_time(format_time(pos).into());
        ui.set_player_total(format_time(dur).into());
        ui.set_player_progress(if dur > 0.0 { (pos / dur) as f32 } else { 0.0 });
        ui.set_player_paused(paused);
        if now > self.osd_until.get() {
            ui.set_player_osd(false);
            if !paused {
                self.release_controls();
            }
        }
        self.update_controls();
        self.raise_subtitles(ui.get_player_osd() || paused);
        if self.stats.get() {
            let quality = self.config.borrow().video.quality.label();
            ui.set_player_stats(
                format!("{}\ntonemap {}\nquality {quality}", self.player.stats(), self.tone_mapping_name()).into(),
            );
        }
        self.update_segments(pos, dur, now);

        if now.duration_since(self.last_media_update.get()) >= Duration::from_secs(1) {
            self.last_media_update.set(now);
            if let Some(m) = self.media.borrow_mut().as_mut() {
                let _ = m.set_state(Some(!paused), Duration::from_secs_f64(pos.max(0.0)));
            }
        }

        let interval = Duration::from_secs(self.config.borrow().playback.progress_report_secs.into());
        let due = self.session.borrow().as_ref().is_some_and(|s| now.duration_since(s.last_report) >= interval);
        if due {
            if let Some(s) = self.session.borrow_mut().as_mut() {
                s.last_report = now;
            }
            self.report(REPORT_PROGRESS);
        }
    }

    pub fn handle_player(&self, action: Action) {
        let ui = self.ui();
        self.show_osd();
        match action {
            Action::ShowStats => {
                self.stats.set(!self.stats.get());
                if !self.stats.get() {
                    ui.set_player_stats(SharedString::new());
                }
                return;
            }
            Action::SubtitlePanel => return self.open_panel(),
            Action::SkipSegment => {
                self.skip_segment_or_play_next();
                return;
            }
            Action::Activate | Action::OsdMenu => return self.controls_activate(),
            Action::NextEpisode => return self.play_adjacent(true),
            Action::PreviousEpisode => return self.play_adjacent(false),
            Action::CycleShader => return self.cycle_shader(),
            Action::CycleToneMapping => return self.cycle_tone_mapping(),
            Action::CyclePictureQuality => return self.cycle_picture_quality(),
            Action::Back if self.session.borrow().as_ref().is_some_and(|s| s.countdown.is_some()) => {
                if let Some(s) = self.session.borrow_mut().as_mut() {
                    s.countdown = None;
                    s.countdown_cancelled = true;
                }
                return self.toast("Autoplay cancelled for this episode");
            }
            Action::SubAutoSync => return self.auto_sync_current(),
            Action::Back => self.apply_to_player(Action::Stop),
            other => self.apply_to_player(other),
        }

        let mpv = self.player.mpv();
        let prop = |k: &str| mpv.get_property::<String>(k).unwrap_or_default();
        let feedback = match action {
            Action::Volume(_) | Action::Mute => Some(format!(
                "Volume {}{}",
                prop("volume"),
                if mpv.get_property::<bool>("mute").unwrap_or(false) { " (muted)" } else { "" }
            )),
            Action::Speed(_) | Action::SpeedReset => Some(format!("Speed {}", prop("speed"))),
            Action::SubDelay(_) => {
                self.remember_sub_delay();
                let ms = (mpv.get_property::<f64>("sub-delay").unwrap_or(0.0) * 1000.0).round();
                Some(format!("Subtitle delay {:+.1} s (remembered)", ms / 1000.0))
            }
            Action::CycleAudio => Some(self.remember_audio_choice()),
            Action::CycleSubtitle | Action::CycleSubtitleBack => Some(self.remember_subtitle_choice()),
            Action::ToggleSubtitles => Some(
                if mpv.get_property::<bool>("sub-visibility").unwrap_or(true) { "Subtitles shown" } else { "Subtitles hidden" }
                    .into(),
            ),
            _ => None,
        };
        if let Some(msg) = feedback {
            self.toast(msg.trim().to_owned());
        }
    }

    /// Remembers the audio track just picked; returns the message to show.
    pub fn remember_audio_choice(&self) -> String {
        let mpv = self.player.mpv();
        let prop = |k: &str| mpv.get_property::<String>(k).unwrap_or_default();
        let lang = prop("current-tracks/audio/lang");
        self.remember(|state, key| state.remember_audio(key, Lang::parse(&lang).as_ref()));
        let name = Lang::parse(&lang).map_or(lang, |l| l.name());
        format!("Audio: {name} {}", prop("current-tracks/audio/title")).trim().to_owned()
    }

    /// Remembers the subtitle track just picked; returns the message to show.
    pub fn remember_subtitle_choice(&self) -> String {
        let mpv = self.player.mpv();
        let prop = |k: &str| mpv.get_property::<String>(k).unwrap_or_default();
        let off = matches!(prop("sid").as_str(), "no" | "false" | "");
        let lang = prop("current-tracks/sub/lang");
        self.remember(|state, key| state.remember_subtitle(key, if off { None } else { Lang::parse(&lang) }.as_ref()));
        if off {
            return "Subtitles off".into();
        }
        let name = Lang::parse(&lang).map_or(lang, |l| l.name());
        format!("Subtitles: {name} {}", prop("current-tracks/sub/title")).trim().to_owned()
    }

    /// Stores a manual track choice for the current show (SPEC §5.2).
    fn remember(&self, f: impl FnOnce(&mut rj_core::state::AppState, &str)) {
        let key = self.session.borrow().as_ref().and_then(|s| s.memory_key.clone());
        let Some(key) = key else { return };
        let mut state = self.state.borrow_mut();
        f(&mut state, &key);
        if let Err(e) = state.save(&self.state_path) {
            tracing::warn!("saving state: {e}");
        }
    }

    fn apply_to_player(&self, action: Action) {
        match self.player.apply(action) {
            Ok(None) => {}
            Ok(Some(unhandled)) => self.toast(format!("{unhandled}: not implemented yet")),
            Err(e) => tracing::debug!("mpv rejected {action:?}: {e}"),
        }
    }

    /// Media keys / SMTC. Only meaningful while something is playing.
    pub fn media_key(&self, action: Action) {
        if self.session.borrow().is_some() {
            self.handle_player(action);
        }
    }
}
