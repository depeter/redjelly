//! Whisper via the official `whisper-cli` binary from whisper.cpp releases
//! (CUDA build on Windows). Running the prebuilt tool avoids compiling
//! whisper.cpp + CUDA into the app, which can't be cross-built from Linux.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rj_core::lang::Lang;
use serde::Deserialize;

use crate::pipeline::Transcriber;
use crate::{Cue, Error};

pub struct WhisperCli {
    pub exe: PathBuf,
    pub model: PathBuf,
    /// Silero VAD model; strongly recommended (suppresses hallucinations).
    pub vad_model: Option<PathBuf>,
    pub use_gpu: bool,
    pub threads: usize,
    /// Directory for whisper's output files.
    pub work_dir: PathBuf,
}

#[derive(Deserialize)]
struct Output {
    transcription: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    offsets: Offsets,
    text: String,
}

#[derive(Deserialize)]
struct Offsets {
    from: u64,
    to: u64,
}

/// "whisper_print_progress_callback: progress =  45%" → 0.45
fn parse_progress(line: &str) -> Option<f32> {
    let rest = line.split("progress =").nth(1)?;
    let pct: f32 = rest.trim().trim_end_matches('%').trim().parse().ok()?;
    Some(pct / 100.0)
}

fn parse_output(json: &str) -> Result<Vec<Cue>, Error> {
    let out: Output = serde_json::from_str(json).map_err(|e| Error::Transcribe(format!("bad whisper output: {e}")))?;
    Ok(out
        .transcription
        .into_iter()
        .map(|s| Cue { start_ms: s.offsets.from, end_ms: s.offsets.to, text: s.text.trim().to_owned() })
        .collect())
}

impl Transcriber for WhisperCli {
    fn transcribe(
        &mut self,
        audio: &Path,
        spoken: Option<&Lang>,
        translate_to_english: bool,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<Cue>, Error> {
        for (what, p) in [("whisper-cli", &self.exe), ("whisper model", &self.model)] {
            if !p.exists() {
                return Err(Error::ModelMissing(format!("{what} at {}", p.display())));
            }
        }
        std::fs::create_dir_all(&self.work_dir).map_err(|e| Error::Transcribe(e.to_string()))?;
        let out_base = self.work_dir.join(format!("whisper-{}", std::process::id()));
        let out_json = out_base.with_extension("json");
        let _ = std::fs::remove_file(&out_json);

        let mut cmd = Command::new(&self.exe);
        cmd.arg("-m").arg(&self.model)
            .arg("-f").arg(audio)
            .arg("-l").arg(spoken.map_or("auto", Lang::as_str))
            .arg("-t").arg(self.threads.max(1).to_string())
            .arg("-oj").arg("-of").arg(&out_base)
            .arg("-pp").arg("-np");
        if translate_to_english {
            cmd.arg("-tr");
        }
        if !self.use_gpu {
            cmd.arg("-ng");
        }
        if let Some(vad) = self.vad_model.as_ref().filter(|p| p.exists()) {
            cmd.arg("--vad").arg("-vm").arg(vad);
        }
        // The DLLs (ggml, CUDA) live next to the exe.
        if let Some(dir) = self.exe.parent() {
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
        cmd.stdout(Stdio::null()).stderr(Stdio::piped());
        tracing::info!("running {cmd:?}");
        let mut child = cmd.spawn().map_err(|e| Error::Transcribe(format!("starting whisper-cli: {e}")))?;

        // Progress arrives on stderr; read it on a thread so we can poll cancel.
        let stderr = child.stderr.take().expect("piped");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut tail: Vec<String> = Vec::new();
        let status = loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Cancelled);
            }
            while let Ok(line) = rx.try_recv() {
                match parse_progress(&line) {
                    Some(p) => progress(p),
                    None => {
                        tail.push(line);
                        if tail.len() > 20 {
                            tail.remove(0);
                        }
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => return Err(Error::Transcribe(e.to_string())),
            }
        };
        if !status.success() {
            return Err(Error::Transcribe(format!("whisper-cli failed ({status}): {}", tail.join(" | "))));
        }
        let json = std::fs::read_to_string(&out_json).map_err(|e| Error::Transcribe(format!("reading output: {e}")))?;
        let _ = std::fs::remove_file(&out_json);
        progress(1.0);
        parse_output(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_lines() {
        assert_eq!(parse_progress("whisper_print_progress_callback: progress =  45%"), Some(0.45));
        assert_eq!(parse_progress("whisper_init: loading model"), None);
    }

    #[test]
    fn parses_json_output() {
        let json = r#"{"transcription":[{"timestamps":{"from":"00:00:00,290","to":"00:00:08,000"},
            "offsets":{"from":290,"to":8000},"text":" And so, my fellow Americans,"}]}"#;
        let cues = parse_output(json).unwrap();
        assert_eq!(cues, [Cue { start_ms: 290, end_ms: 8000, text: "And so, my fellow Americans,".into() }]);
    }

    /// Runs the real binary: `RJ_WHISPER_CLI=… RJ_WHISPER_MODEL=… RJ_WHISPER_WAV=… cargo test -- --ignored`
    #[test]
    #[ignore]
    fn real_whisper_cli() {
        let env = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")));
        let mut w = WhisperCli {
            exe: env("RJ_WHISPER_CLI"),
            model: env("RJ_WHISPER_MODEL"),
            vad_model: std::env::var("RJ_WHISPER_VAD").ok().map(PathBuf::from),
            use_gpu: false,
            threads: 4,
            work_dir: std::env::temp_dir(),
        };
        let cancel = AtomicBool::new(false);
        let mut last = 0.0;
        let cues = w.transcribe(&env("RJ_WHISPER_WAV"), Lang::parse("en").as_ref(), false, &mut |p| last = p, &cancel).unwrap();
        println!("{cues:#?}");
        assert!(cues.iter().any(|c| c.text.contains("country")));
        assert_eq!(last, 1.0);
    }
}
