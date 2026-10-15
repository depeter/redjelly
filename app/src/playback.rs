//! Turning a Jellyfin item into something mpv can play, and reporting
//! progress back to the server.

use std::time::Instant;

use anyhow::Context as _;
use rj_jellyfin::Client;
use rj_jellyfin::models::{Item, MediaSource, PlaybackReport, StreamType, TICKS_PER_SECOND};

/// Everything needed to start playback; built off the UI thread.
#[derive(Debug, Clone)]
pub struct PlayRequest {
    pub title: String,
    pub url: String,
    pub start_secs: f64,
    pub item: Option<Item>,
    pub media_source: Option<MediaSource>,
    pub play_session_id: Option<String>,
    pub play_method: &'static str,
    /// (url, title, lang) of external text subtitles stored on the server.
    pub external_subs: Vec<(String, String, String)>,
}

impl PlayRequest {
    /// `--play <file-or-url>`: no server involved.
    pub fn standalone(target: &str) -> Self {
        let title = target.rsplit(['/', '\\']).next().unwrap_or(target).to_owned();
        Self {
            title,
            url: target.to_owned(),
            start_secs: 0.0,
            item: None,
            media_source: None,
            play_session_id: None,
            play_method: "DirectPlay",
            external_subs: Vec::new(),
        }
    }
}

pub fn display_title(item: &Item) -> String {
    match (&item.series_name, item.parent_index_number, item.index_number) {
        (Some(series), Some(s), Some(e)) => format!("{series} · S{s:02}E{e:02} · {}", item.name),
        _ => item.name.clone(),
    }
}

fn subtitle_ext(codec: Option<&str>) -> &'static str {
    match codec {
        Some("ass") => "ass",
        Some("ssa") => "ssa",
        Some("webvtt" | "vtt") => "vtt",
        _ => "srt",
    }
}

pub async fn prepare(client: &Client, item_id: &str, from_start: bool) -> anyhow::Result<PlayRequest> {
    let item = client.item(item_id).await.context("loading item")?;
    let start_ticks = if from_start { 0 } else { item.user_data.as_ref().map_or(0, |u| u.playback_position_ticks) };
    let info = client.playback_info(item_id, start_ticks).await.context("requesting playback info")?;
    if let Some(code) = &info.error_code {
        anyhow::bail!("server refused playback: {code}");
    }
    let ms = info.media_sources.into_iter().next().context("item has no playable media")?;

    let (url, play_method) = match (&ms.transcoding_url, ms.supports_direct_play) {
        (Some(t), false) => (client.url(t)?, "Transcode"),
        _ => (client.direct_stream_url(item_id, &ms.id)?, "DirectPlay"),
    };

    let external_subs = ms
        .media_streams
        .iter()
        .filter(|s| s.kind == StreamType::Subtitle && s.is_external && s.is_text_subtitle_stream)
        .filter_map(|s| {
            let url = match &s.delivery_url {
                Some(u) => client.url(u).ok()?,
                None => client
                    .subtitle_url(item_id, &ms.id, s.index, subtitle_ext(s.codec.as_deref()))
                    .ok()?,
            };
            let title = s.display_title.clone().or_else(|| s.title.clone()).unwrap_or_default();
            Some((url.to_string(), title, s.language.clone().unwrap_or_default()))
        })
        .collect();

    Ok(PlayRequest {
        title: display_title(&item),
        url: url.to_string(),
        start_secs: start_ticks as f64 / TICKS_PER_SECOND as f64,
        item: Some(item),
        media_source: Some(ms),
        play_session_id: info.play_session_id,
        play_method,
        external_subs,
    })
}

/// A skippable part of the item (intro, recap, credits…), in seconds.
#[derive(Debug, Clone)]
pub struct Segment {
    pub kind: rj_jellyfin::models::SegmentType,
    pub start: f64,
    pub end: f64,
}

/// A playback in progress.
pub struct Session {
    pub req: PlayRequest,
    pub segments: Vec<Segment>,
    /// Segments already auto-skipped (don't skip again after seeking back).
    pub skipped: Vec<usize>,
    pub next: Option<Item>,
    pub prev: Option<Item>,
    /// Next-episode countdown deadline, when running.
    pub countdown: Option<std::time::Instant>,
    pub countdown_cancelled: bool,
    /// Item to play once this one has stopped (next/previous episode).
    pub chain_to: Option<String>,
    /// Track-memory key: the series for episodes, else the item.
    pub memory_key: Option<String>,
    /// mpv has started the file; idle after this means playback ended.
    pub started: bool,
    pub last_report: Instant,
}

impl Session {
    pub fn report(&self, position_secs: f64, paused: bool) -> Option<PlaybackReport> {
        let item = self.req.item.as_ref()?;
        Some(PlaybackReport {
            item_id: item.id.clone(),
            media_source_id: self.req.media_source.as_ref().map(|m| m.id.clone()).unwrap_or_default(),
            play_session_id: self.req.play_session_id.clone(),
            position_ticks: (position_secs * TICKS_PER_SECOND as f64) as i64,
            is_paused: paused,
            is_muted: false,
            volume_level: 100,
            audio_stream_index: None,
            subtitle_stream_index: None,
            play_method: self.req.play_method,
            can_seek: true,
        })
    }
}

pub fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(format_time(75.4), "1:15");
        assert_eq!(format_time(3600.0 + 62.0), "1:01:02");
    }

    #[test]
    fn standalone_title_is_file_name() {
        assert_eq!(PlayRequest::standalone(r"C:\Videos\film.mkv").title, "film.mkv");
    }
}
