//! AI subtitle jobs (SPEC §7): a sequential background queue. Each job
//! either transcribes an audio track with Whisper (translating if needed)
//! or translates an existing text subtitle, then uploads the result to
//! Jellyfin and loads it if that item is playing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rj_ai::pipeline::{self, NoTranslator, Translator};
use rj_ai::translate::{MarianPair, OpusMt};
use rj_ai::whisper_cli::WhisperCli;
use rj_core::lang::Lang;
use rj_jellyfin::Client;
use rj_jellyfin::models::{Item, MediaSource, StreamType};

use crate::layout::STYLE_WIDE;
use crate::models::Models;
use crate::nav::Placement;
use crate::screens::{CardContent, Content, HeaderContent, RowContent, Source, Target};
use crate::{App, SCREEN_BROWSE, with_app};

#[derive(Debug, Clone, PartialEq)]
pub enum JobKind {
    /// Whisper on an audio track (Jellyfin stream index), translating when
    /// the spoken language isn't the target.
    Transcribe { audio_index: Option<u32>, spoken: Option<Lang> },
    /// Translate an existing text subtitle track, keeping its timing.
    TranslateSub { sub_index: u32, from: Lang },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

pub struct Job {
    pub id: u64,
    pub item_id: String,
    pub title: String,
    pub target: Lang,
    pub kind: JobKind,
    pub state: JobState,
    pub status: String,
    pub progress: f32,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn active(&self) -> bool {
        matches!(self.state, JobState::Queued | JobState::Running)
    }

    pub fn summary(&self) -> String {
        match self.state {
            JobState::Queued => "Waiting…".into(),
            JobState::Running => format!("{} · {:.0}%", self.status, self.progress * 100.0),
            JobState::Done => "Done · saved to Jellyfin".into(),
            JobState::Failed => format!("Failed: {}", self.status),
            JobState::Cancelled => "Cancelled".into(),
        }
    }
}

/// Text subtitle languages already present on an item.
fn text_sub_langs(ms: &MediaSource) -> Vec<(u32, Lang)> {
    ms.media_streams
        .iter()
        .filter(|s| s.kind == StreamType::Subtitle && s.is_text_subtitle_stream)
        .filter_map(|s| Some((s.index, Lang::parse(s.language.as_deref()?)?)))
        .collect()
}

/// Cheapest way to get `target` subtitles for an item: translate an
/// existing subtitle when we have a model for it, else Whisper. `None` when
/// the item already has them.
pub fn plan(item: &Item, target: &Lang) -> Option<JobKind> {
    let ms = item.media_sources.first()?;
    let subs = text_sub_langs(ms);
    if subs.iter().any(|(_, l)| l == target) {
        return None;
    }
    let pairs = Models::opus_pairs();
    if let Some((index, from)) = subs.iter().find(|(_, from)| pairs.iter().any(|(f, t)| f == from && t == target)) {
        return Some(JobKind::TranslateSub { sub_index: *index, from: from.clone() });
    }
    let audio = ms.media_streams.iter().filter(|s| s.kind == StreamType::Audio);
    let chosen = audio.clone().find(|s| s.is_default).or_else(|| audio.clone().next());
    Some(JobKind::Transcribe {
        audio_index: chosen.map(|s| s.index),
        spoken: chosen.and_then(|s| s.language.as_deref()).and_then(Lang::parse),
    })
}

/// Sends throttled progress to the UI thread.
#[derive(Clone)]
struct Reporter {
    id: u64,
    last: Arc<Mutex<Instant>>,
}

impl Reporter {
    fn report(&self, status: String, progress: f32) {
        {
            let mut last = self.last.lock().unwrap();
            if last.elapsed() < Duration::from_millis(400) && progress < 1.0 {
                return;
            }
            *last = Instant::now();
        }
        let id = self.id;
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.job_progress(id, status, progress)));
    }
}

/// Everything a job needs off the UI thread.
struct JobInput {
    client: Client,
    http: reqwest::Client,
    models: Models,
    use_gpu: bool,
    vad: bool,
    threads: usize,
    work_dir: PathBuf,
    out_dir: PathBuf,
    item_id: String,
    kind: JobKind,
    target: Lang,
    cancel: Arc<AtomicBool>,
}

fn check_cancel(cancel: &AtomicBool) -> anyhow::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    Ok(())
}

async fn run(input: JobInput, rep: Reporter) -> anyhow::Result<PathBuf> {
    let JobInput { client, http, models, use_gpu, vad, threads, work_dir, out_dir, item_id, kind, target, cancel } = input;
    let status = {
        let rep = rep.clone();
        move |s: String, p: f32| rep.report(s, p)
    };
    let item = client.item(&item_id).await?;
    let ms = item.media_sources.first().cloned().ok_or_else(|| anyhow::anyhow!("item has no media"))?;
    tokio::fs::create_dir_all(&work_dir).await?;
    tokio::fs::create_dir_all(&out_dir).await?;
    let en = Lang::parse("en").expect("valid");

    let cues = match kind {
        JobKind::TranslateSub { sub_index, from } => {
            models.ensure(&http, false, Some((&from, &target)), &status, &cancel).await?;
            check_cancel(&cancel)?;
            status("Fetching subtitle".into(), 0.0);
            let url = client.subtitle_url(&item_id, &ms.id, sub_index, "srt")?;
            let bytes =
                http.get(url).header("Authorization", client.auth_header()).send().await?.error_for_status()?.bytes().await?;
            let source = rj_ai::srt::parse_srt(&rj_subs::encoding::to_utf8(&bytes));
            anyhow::ensure!(!source.is_empty(), "the source subtitle is empty");
            let dir = models.opus_dir(&from, &target);
            let (cancel2, target2, rep2) = (cancel.clone(), target.clone(), rep.clone());
            tokio::task::spawn_blocking(move || {
                let mut t = OpusMt::new(vec![MarianPair::load(&dir, from.clone(), target2.clone())?]);
                pipeline::translate_cues(source, &from, &target2, &mut t, &mut |p| rep2.report("Translating".into(), p), &cancel2)
            })
            .await??
        }
        JobKind::Transcribe { audio_index, spoken } => {
            let translate = spoken.as_ref() != Some(&target) && target != en;
            let pair_available = Models::opus_pairs().iter().any(|(f, t)| *f == en && *t == target);
            anyhow::ensure!(
                !translate || pair_available,
                "no translation model to {} yet (only English → Dutch)",
                target.name()
            );
            models.ensure(&http, true, translate.then_some((&en, &target)), &status, &cancel).await?;
            check_cancel(&cancel)?;

            // 1. Audio: server-side audio-only transcode, else decode the
            //    full stream locally.
            let wav = work_dir.join(format!("{item_id}.wav"));
            let auth = client.auth_header();
            let server_url = client.audio_only_url(&item_id, &ms.id, audio_index)?.to_string();
            let mpv_aid = audio_index.and_then(|idx| {
                let mut audio: Vec<u32> = ms
                    .media_streams
                    .iter()
                    .filter(|s| s.kind == StreamType::Audio && !s.is_external)
                    .map(|s| s.index)
                    .collect();
                audio.sort_unstable();
                audio.iter().position(|i| *i == idx).map(|p| p as i64 + 1)
            });
            let direct_url = client.direct_stream_url(&item_id, &ms.id)?.to_string();
            let (wav2, cancel2, rep2) = (wav.clone(), cancel.clone(), rep.clone());
            tokio::task::spawn_blocking(move || {
                let mut progress = |p: f32| rep2.report("Extracting audio".into(), p);
                match rj_player::extract::extract_audio(&server_url, Some(&auth), None, &wav2, &mut progress, &cancel2) {
                    Ok(()) => Ok(()),
                    Err(e) if e == "cancelled" => Err(e),
                    Err(e) => {
                        tracing::warn!("server audio transcode failed ({e}); decoding the full stream locally");
                        rep2.report("Extracting audio (full download)".into(), 0.0);
                        rj_player::extract::extract_audio(&direct_url, Some(&auth), mpv_aid, &wav2, &mut progress, &cancel2)
                    }
                }
            })
            .await?
            .map_err(|e| anyhow::anyhow!("audio extraction: {e}"))?;
            check_cancel(&cancel)?;

            // 2. Whisper (+ translation).
            let exe = models.whisper_cli().ok_or_else(|| anyhow::anyhow!("whisper-cli not found after download"))?;
            let mut whisper = WhisperCli {
                exe,
                model: models.whisper_model(),
                vad_model: vad.then(|| models.vad_model()),
                use_gpu,
                threads: if threads == 0 { std::thread::available_parallelism().map_or(4, |n| n.get()) } else { threads },
                work_dir: work_dir.clone(),
            };
            let opus_dir = models.opus_dir(&en, &target);
            let (cancel2, target2, rep2, wav2) = (cancel.clone(), target.clone(), rep.clone(), wav.clone());
            let result = tokio::task::spawn_blocking(move || {
                let mut translator: Box<dyn Translator> = if translate {
                    Box::new(OpusMt::new(vec![MarianPair::load(&opus_dir, en.clone(), target2.clone())?]))
                } else {
                    Box::new(NoTranslator)
                };
                let mut progress = |p: f32| {
                    let what = if p < 0.8 { "Transcribing" } else { "Translating" };
                    rep2.report(what.into(), p);
                };
                pipeline::generate(&wav2, spoken.as_ref(), &target2, &mut whisper, translator.as_mut(), &mut progress, &cancel2)
            })
            .await?;
            let _ = tokio::fs::remove_file(&wav).await;
            result?
        }
    };

    anyhow::ensure!(!cues.is_empty(), "no speech found");
    let srt = rj_ai::srt::to_srt(&cues);
    let path = out_dir.join(format!("{item_id}.{}.ai.srt", target));
    tokio::fs::write(&path, &srt).await?;
    status("Uploading to Jellyfin".into(), 1.0);
    client.upload_subtitle(&item_id, target.iso639_2b(), "srt", srt.as_bytes(), false, false).await?;
    Ok(path)
}

impl App {
    fn target_lang(&self) -> Option<Lang> {
        self.config.borrow().track_preferences().languages.first().cloned()
    }

    pub fn queue_ai(&self, item_id: String, title: String, kind: JobKind) {
        let Some(target) = self.target_lang() else { return };
        let duplicate = self.jobs.borrow().iter().any(|j| j.item_id == item_id && j.target == target && j.active());
        if duplicate {
            return self.toast("Already queued");
        }
        let id = self.jobs.borrow().len() as u64 + 1;
        self.jobs.borrow_mut().push(Job {
            id,
            item_id,
            title,
            target,
            kind,
            state: JobState::Queued,
            status: String::new(),
            progress: 0.0,
            cancel: Arc::default(),
        });
        self.kick_ai();
        self.jobs_changed();
    }

    /// Plans and queues a job for one item (detail screen button).
    pub fn queue_ai_for_item(&self, item_id: String) {
        let (Some(client), Some(target)) = (self.client.borrow().clone(), self.target_lang()) else { return };
        self.spawn_ui(async move { client.item(&item_id).await }, move |app, r| match r {
            Ok(item) => match plan(&item, &target) {
                Some(kind) => {
                    app.toast(format!("Queued AI {} subtitles · Ctrl+J shows progress", target.name()));
                    app.queue_ai(item.id.clone(), crate::playback::display_title(&item), kind);
                }
                None => app.toast(format!("This already has {} subtitles", target.name())),
            },
            Err(e) => app.toast(format!("Couldn't load item: {e}")),
        });
    }

    /// Queues every episode of a series that lacks target subtitles.
    pub fn queue_ai_for_series(&self, series_id: String) {
        let (Some(client), Some(target)) = (self.client.borrow().clone(), self.target_lang()) else { return };
        self.toast("Checking episodes…");
        self.spawn_ui(async move { client.episodes_with_media(&series_id).await }, move |app, r| match r {
            Ok(eps) => {
                let mut n = 0;
                for ep in &eps.items {
                    if let Some(kind) = plan(ep, &target) {
                        app.queue_ai(ep.id.clone(), crate::playback::display_title(ep), kind);
                        n += 1;
                    }
                }
                app.toast(if n == 0 {
                    format!("Every episode already has {} subtitles", target.name())
                } else {
                    format!("Queued {n} episodes for AI {} subtitles · Ctrl+J shows progress", target.name())
                });
            }
            Err(e) => app.toast(format!("Couldn't load episodes: {e}")),
        });
    }

    fn kick_ai(&self) {
        if self.ai_running.get() {
            return;
        }
        let next = self.jobs.borrow().iter().find(|j| j.state == JobState::Queued).map(|j| j.id);
        let Some(id) = next else { return };
        let Some(client) = self.client.borrow().clone() else { return };
        let input = {
            let mut jobs = self.jobs.borrow_mut();
            let Some(job) = jobs.iter_mut().find(|j| j.id == id) else { return };
            job.state = JobState::Running;
            job.status = "Starting".into();
            let cfg = self.config.borrow().ai.clone();
            JobInput {
                client,
                http: self.http.clone(),
                models: Models::new(self.models_dir.clone(), &cfg),
                use_gpu: cfg.use_gpu,
                vad: cfg.vad,
                threads: cfg.threads,
                work_dir: self.models_dir.join("work"),
                out_dir: self.sub_cache.clone(),
                item_id: job.item_id.clone(),
                kind: job.kind.clone(),
                target: job.target.clone(),
                cancel: job.cancel.clone(),
            }
        };
        self.ai_running.set(true);
        let rep = Reporter { id, last: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))) };
        self.spawn_ui(run(input, rep), move |app, result| app.job_finished(id, result));
    }

    fn job_progress(&self, id: u64, status: String, progress: f32) {
        if let Some(job) = self.jobs.borrow_mut().iter_mut().find(|j| j.id == id && j.state == JobState::Running) {
            job.status = status;
            job.progress = progress;
        }
        self.jobs_changed();
    }

    fn job_finished(&self, id: u64, result: anyhow::Result<PathBuf>) {
        self.ai_running.set(false);
        let (title, item_id, target) = {
            let mut jobs = self.jobs.borrow_mut();
            let Some(job) = jobs.iter_mut().find(|j| j.id == id) else { return };
            match &result {
                Ok(_) => job.state = JobState::Done,
                Err(_) if job.cancel.load(Ordering::Relaxed) => job.state = JobState::Cancelled,
                Err(e) => {
                    job.state = JobState::Failed;
                    job.status = format!("{e:#}");
                }
            }
            (job.title.clone(), job.item_id.clone(), job.target.clone())
        };
        match result {
            Ok(path) => {
                self.toast(format!("AI {} subtitles ready: {title}", target.name()));
                // Playing that item right now? Load it immediately.
                let playing = self.session.borrow().as_ref().and_then(|s| s.req.item.as_ref().map(|i| i.id.clone()));
                if playing.as_deref() == Some(item_id.as_str())
                    && let Ok(sid) = self.player.add_subtitle(&path.to_string_lossy(), "AI", target.as_str()) {
                        let _ = self.player.set_subtitle(Some(sid));
                    }
            }
            Err(e) if !e.to_string().contains("cancelled") => {
                tracing::warn!("AI job {id} failed: {e:#}");
                self.toast(format!("AI subtitles failed for {title}: {e:#}"));
            }
            Err(_) => {}
        }
        self.jobs_changed();
        self.kick_ai();
    }

    pub fn cancel_job(&self, id: u64) {
        let mut jobs = self.jobs.borrow_mut();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else { return };
        match job.state {
            JobState::Queued => job.state = JobState::Cancelled,
            JobState::Running => {
                job.cancel.store(true, Ordering::Relaxed);
                job.status = "Cancelling".into();
            }
            _ => return,
        }
        drop(jobs);
        self.toast("Cancelled");
        self.jobs_changed();
    }

    /// Refreshes whatever shows jobs: the Jobs screen and the subtitle panel.
    fn jobs_changed(&self) {
        if self.top_source() == Some(Source::Jobs) && self.ui().get_screen() == SCREEN_BROWSE {
            self.present(self.jobs_content(), Placement::ReplaceTop);
        }
        self.refresh_panel();
    }

    /// Job for the item being played, for the subtitle panel.
    pub fn job_for(&self, item_id: &str) -> Option<String> {
        self.jobs.borrow().iter().rev().find(|j| j.item_id == item_id && j.active()).map(|j| j.summary())
    }

    pub fn jobs_content(&self) -> Content {
        let jobs = self.jobs.borrow();
        let running = jobs.iter().filter(|j| j.state == JobState::Running).count();
        let queued = jobs.iter().filter(|j| j.state == JobState::Queued).count();
        let cards: Vec<CardContent> = jobs
            .iter()
            .rev()
            .map(|j| CardContent {
                title: j.title.clone(),
                subtitle: match &j.kind {
                    JobKind::TranslateSub { from, .. } => format!("{} → {} · {}", from.name(), j.target.name(), j.summary()),
                    JobKind::Transcribe { .. } => format!("Whisper → {} · {}", j.target.name(), j.summary()),
                },
                image: None,
                progress: if j.state == JobState::Running { j.progress } else { -1.0 },
                played: j.state == JobState::Done,
                target: Target::Job(j.id),
            })
            .collect();
        let empty = cards.is_empty();
        Content {
            source: Source::Jobs,
            header: Some(HeaderContent {
                title: "AI subtitles".into(),
                meta: format!("{running} running · {queued} waiting"),
                info: if empty {
                    "Nothing yet. Queue jobs from a movie, episode or series page, or with S while playing.".into()
                } else {
                    "Enter on a running or waiting job cancels it · finished ones open the item".into()
                },
                ..Default::default()
            }),
            rows: if empty { Vec::new() } else { vec![RowContent { title: String::new(), style: STYLE_WIDE, cards, start: 0 }] },
            focus: (0, 0),
            paging: None,
        }
    }

    pub fn activate_job(&self, id: u64) {
        let job = self.jobs.borrow().iter().find(|j| j.id == id).map(|j| (j.active(), j.item_id.clone()));
        match job {
            Some((true, _)) => self.cancel_job(id),
            Some((false, item_id)) => self.open(Source::Detail { id: item_id }, Placement::Push),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(json: &str) -> Item {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn plans_translation_when_english_subtitle_exists() {
        let i = item(r#"{"Id":"x","MediaSources":[{"Id":"m","MediaStreams":[
            {"Index":1,"Type":"Audio","Language":"eng","IsDefault":true},
            {"Index":2,"Type":"Subtitle","Language":"eng","IsTextSubtitleStream":true}]}]}"#);
        let nl = Lang::parse("nl").unwrap();
        assert_eq!(plan(&i, &nl), Some(JobKind::TranslateSub { sub_index: 2, from: Lang::parse("en").unwrap() }));
    }

    #[test]
    fn plans_whisper_on_default_audio_otherwise() {
        let i = item(r#"{"Id":"x","MediaSources":[{"Id":"m","MediaStreams":[
            {"Index":1,"Type":"Audio","Language":"fre"},
            {"Index":2,"Type":"Audio","Language":"jpn","IsDefault":true},
            {"Index":3,"Type":"Subtitle","Language":"jpn","IsTextSubtitleStream":false}]}]}"#);
        let nl = Lang::parse("nl").unwrap();
        assert_eq!(plan(&i, &nl), Some(JobKind::Transcribe { audio_index: Some(2), spoken: Lang::parse("ja") }));
    }

    #[test]
    fn nothing_to_do_when_target_exists() {
        let i = item(r#"{"Id":"x","MediaSources":[{"Id":"m","MediaStreams":[
            {"Index":2,"Type":"Subtitle","Language":"dut","IsTextSubtitleStream":true}]}]}"#);
        assert_eq!(plan(&i, &Lang::parse("nl").unwrap()), None);
    }
}
