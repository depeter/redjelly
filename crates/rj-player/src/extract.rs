//! Audio extraction for AI subtitles: a second, headless mpv core decodes
//! one audio track to a 16 kHz mono 16-bit WAV as fast as it can.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use libmpv2::Mpv;
use libmpv2::events::Event;

/// `aid`: mpv audio track id (`None` = default track). `progress` gets 0..1.
pub fn extract_audio(
    url: &str,
    auth_header: Option<&str>,
    aid: Option<i64>,
    out_wav: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let out = out_wav.to_string_lossy().into_owned();
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "null")?;
        init.set_option("vid", "no")?;
        init.set_option("sid", "no")?;
        init.set_option("ao", "pcm")?;
        init.set_option("ao-pcm-file", out.as_str())?;
        init.set_option("ao-pcm-waveheader", "yes")?;
        init.set_option("audio-samplerate", "16000")?;
        init.set_option("audio-channels", "mono")?;
        init.set_option("audio-format", "s16")?;
        // Decode as fast as possible instead of in real time.
        init.set_option("untimed", "yes")?;
        init.set_option("audio-display", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("cache", "yes")?;
        Ok(())
    })
    .map_err(|e| format!("starting mpv: {e}"))?;
    if let Some(a) = aid {
        mpv.set_property("aid", a).map_err(|e| e.to_string())?;
    }
    if let Some(h) = auth_header {
        mpv.set_property("http-header-fields", format!("Authorization: {h}")).map_err(|e| e.to_string())?;
    }
    mpv.command("loadfile", &[url]).map_err(|e| format!("loading {url}: {e}"))?;

    let mut started = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = mpv.command("stop", &[]);
            return Err("cancelled".into());
        }
        match mpv.wait_event(0.25) {
            Some(Ok(Event::StartFile)) => started = true,
            Some(Ok(Event::EndFile(_))) => break,
            Some(Err(e)) => return Err(format!("decoding failed: {e}")),
            _ => {}
        }
        if started {
            let pos: f64 = mpv.get_property("time-pos").unwrap_or(0.0);
            let dur: f64 = mpv.get_property("duration").unwrap_or(0.0);
            if dur > 0.0 {
                progress((pos / dur).clamp(0.0, 1.0) as f32);
            }
        }
    }
    drop(mpv);
    let size = std::fs::metadata(out_wav).map(|m| m.len()).unwrap_or(0);
    // A WAV header alone is 44 bytes: anything that small has no audio.
    if size < 16_000 {
        return Err("no audio was decoded".into());
    }
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RJ_TEST_AUDIO=<any media file> cargo test -p rj-player -- --ignored`
    #[test]
    #[ignore]
    fn extracts_wav() {
        let input = std::env::var("RJ_TEST_AUDIO").expect("set RJ_TEST_AUDIO");
        let out = std::env::temp_dir().join(format!("rj-extract-{}.wav", std::process::id()));
        let start = std::time::Instant::now();
        let mut last = 0.0;
        extract_audio(&input, None, None, &out, &mut |p| last = p, &AtomicBool::new(false)).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        std::fs::remove_file(&out).unwrap();
        println!("{} bytes in {:?}", bytes.len(), start.elapsed());
        assert_eq!(&bytes[..4], b"RIFF");
        // 16 kHz mono s16 in the header.
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 16_000);
        assert_eq!(last, 1.0);
    }
}
