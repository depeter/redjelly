//! Getting one audio track of a Jellyfin item as a 16 kHz mono WAV, for AI
//! subtitles and audio-based subtitle sync.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rj_jellyfin::Client;
use rj_jellyfin::models::{MediaSource, StreamType};

/// Asks the server for an audio-only transcode (small, fast); if it
/// refuses, decodes the full stream locally. `progress(label, fraction)`.
pub async fn extract_wav(
    client: &Client,
    item_id: &str,
    ms: &MediaSource,
    audio_index: Option<u32>,
    wav: &Path,
    progress: impl Fn(&'static str, f32) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let auth = client.auth_header();
    let server_url = client.audio_only_url(item_id, &ms.id, audio_index)?.to_string();
    let direct_url = client.direct_stream_url(item_id, &ms.id)?.to_string();
    // mpv numbers audio tracks 1.. in container order.
    let mpv_aid = audio_index.and_then(|idx| {
        let mut audio: Vec<u32> =
            ms.media_streams.iter().filter(|s| s.kind == StreamType::Audio && !s.is_external).map(|s| s.index).collect();
        audio.sort_unstable();
        audio.iter().position(|i| *i == idx).map(|p| p as i64 + 1)
    });
    let duration = ms.run_time_ticks.filter(|t| *t > 0).map(|t| t as f64 / 10_000_000.0);
    let wav = wav.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut report = |p: f32| progress("Extracting audio", p);
        match rj_player::extract::extract_audio(&server_url, Some(&auth), None, duration, &wav, &mut report, &cancel) {
            Ok(()) => Ok(()),
            Err(e) if e == "cancelled" => Err(e),
            Err(e) => {
                tracing::warn!("server audio transcode failed ({e}); decoding the full stream locally");
                let mut report = |p: f32| progress("Extracting audio (full download)", p);
                rj_player::extract::extract_audio(&direct_url, Some(&auth), mpv_aid, duration, &wav, &mut report, &cancel)
            }
        }
    })
    .await?
    .map_err(|e| anyhow::anyhow!("audio extraction: {e}"))
}

/// The audio track to align a subtitle in `lang` against: one in the same
/// language, else the default (usually original) track.
pub fn track_for_sync(ms: &MediaSource, lang: Option<&rj_core::lang::Lang>) -> Option<u32> {
    let audio: Vec<_> = ms.media_streams.iter().filter(|s| s.kind == StreamType::Audio).collect();
    let same_lang = lang.and_then(|l| {
        audio.iter().find(|s| s.language.as_deref().and_then(rj_core::lang::Lang::parse).as_ref() == Some(l))
    });
    same_lang.or_else(|| audio.iter().find(|s| s.is_default)).or(audio.first()).map(|s| s.index)
}
