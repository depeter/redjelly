//! Voice activity detection with whisper.cpp's `whisper-vad-speech-segments`
//! tool and the Silero model: where people speak in an audio file. Used as
//! the reference for syncing subtitles to the audio (SPEC §6.4).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::Error;

/// "Speech segment 3: start = 538.00, end = 765.00" (centiseconds) → ms.
fn parse_segments(stdout: &str) -> Vec<(i64, i64)> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.split("start =").nth(1)?;
            let (start, end) = rest.split_once(", end =")?;
            let cs = |s: &str| s.trim().parse::<f64>().ok();
            Some(((cs(start)? * 10.0) as i64, (cs(end)? * 10.0) as i64))
        })
        .collect()
}

/// Speech spans (ms) in a 16 kHz mono WAV.
pub fn speech_spans(exe: &Path, model: &Path, wav: &Path, threads: usize, cancel: &AtomicBool) -> Result<Vec<(i64, i64)>, Error> {
    let mut cmd = Command::new(exe);
    cmd.arg("-f").arg(wav).arg("-vm").arg(model).arg("-t").arg(threads.max(1).to_string()).arg("-np");
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
        #[cfg(unix)]
        cmd.env("LD_LIBRARY_PATH", dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| Error::Audio(format!("starting voice detection: {e}")))?;
    // Collect stdout on a thread so the pipe never fills while we poll.
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(Error::Audio(e.to_string())),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::Audio(format!("voice detection failed ({status})")));
    }
    Ok(parse_segments(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_output() {
        let out = "Detected 2 speech segments:\nSpeech segment 0: start = 29.00, end = 221.00\nSpeech segment 1: start = 330.50, end = 377.00\n";
        assert_eq!(parse_segments(out), [(290, 2210), (3305, 3770)]);
    }

    /// `RJ_VAD_TOOL=… RJ_WHISPER_VAD=… RJ_WHISPER_WAV=… cargo test -p rj-ai -- --ignored`
    #[test]
    #[ignore]
    fn real_vad_tool() {
        let env = |k: &str| std::path::PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")));
        let spans =
            speech_spans(&env("RJ_VAD_TOOL"), &env("RJ_WHISPER_VAD"), &env("RJ_WHISPER_WAV"), 4, &AtomicBool::new(false)).unwrap();
        println!("{spans:?}");
        assert!(spans.len() >= 3 && spans[0].0 < 1000);
    }
}
