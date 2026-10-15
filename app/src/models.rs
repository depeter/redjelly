//! AI model files, downloaded on first use into the data dir (SPEC §7):
//! the prebuilt whisper.cpp package, a whisper model, the Silero VAD model
//! and Opus-MT translation models. Versions are pinned so results are
//! reproducible.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rj_core::config::AiConfig;
use rj_core::lang::Lang;

/// whisper.cpp release whose prebuilt binaries we use (2026-09-11).
const WHISPER_RELEASE: &str = "b5130";
const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";

/// Opus-MT pairs we know how to fetch: (from, to, repo, files revision,
/// safetensors revision). The original repos only ship legacy torch
/// weights; the safetensors file comes from the conversion PR.
const OPUS_PAIRS: &[(&str, &str, &str, &str, &str)] = &[(
    "en",
    "nl",
    "Helsinki-NLP/opus-mt-en-nl",
    "8aad73b34ff36c090e7fc8a2eb7e2e7cca235d31",
    "cb106147f56946f00045d2f9768539949e660489",
)];

#[derive(Debug, Clone)]
pub struct Models {
    root: PathBuf,
    runtime: String,
    whisper_model: String,
}

/// One file (or archive) to fetch.
struct Download {
    label: String,
    url: String,
    dest: PathBuf,
    /// Unpack into `dest` (a directory) instead of saving as a file.
    archive: Option<Archive>,
}

#[derive(Clone, Copy)]
enum Archive {
    Zip,
    #[cfg_attr(windows, allow(dead_code))]
    TarGz,
}

impl Models {
    pub fn new(root: PathBuf, cfg: &AiConfig) -> Self {
        let runtime = if cfg!(windows) { cfg.whisper_runtime.clone() } else { "cpu".into() };
        Self { root, runtime, whisper_model: cfg.whisper_model.clone() }
    }

    fn runtime_dir(&self) -> PathBuf {
        self.root.join(format!("whisper-{WHISPER_RELEASE}-{}", self.runtime))
    }

    /// whisper-cli inside the unpacked package (its folder layout varies
    /// between packages, so search for it).
    pub fn whisper_cli(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" };
        find_file(&self.runtime_dir(), name, 3)
    }

    pub fn whisper_model(&self) -> PathBuf {
        self.root.join(format!("ggml-{}.bin", self.whisper_model))
    }

    pub fn vad_model(&self) -> PathBuf {
        self.root.join(VAD_MODEL)
    }

    pub fn opus_dir(&self, from: &Lang, to: &Lang) -> PathBuf {
        self.root.join(format!("opus-mt-{from}-{to}"))
    }

    /// Language pairs we can translate (downloaded or downloadable).
    pub fn opus_pairs() -> Vec<(Lang, Lang)> {
        OPUS_PAIRS.iter().filter_map(|(f, t, ..)| Some((Lang::parse(f)?, Lang::parse(t)?))).collect()
    }

    fn runtime_download(&self) -> Download {
        let base = format!("https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_RELEASE}");
        let (file, archive, label) = if cfg!(windows) {
            match self.runtime.as_str() {
                "cuda11" => ("whisper-cublas-11.8.0-bin-x64.zip", Archive::Zip, "Whisper (CUDA 11, ~270 MB)"),
                "cpu" => ("whisper-bin-x64.zip", Archive::Zip, "Whisper (CPU, ~9 MB)"),
                _ => ("whisper-cublas-12.4.0-bin-x64.zip", Archive::Zip, "Whisper (CUDA 12, ~675 MB)"),
            }
        } else {
            ("whisper-bin-ubuntu-x64.tar.gz", Archive::TarGz, "Whisper (CPU, ~10 MB)")
        };
        Download { label: label.into(), url: format!("{base}/{file}"), dest: self.runtime_dir(), archive: Some(archive) }
    }

    /// Files missing for a job: whisper (if `need_whisper`) and the
    /// translation pair (if any).
    fn missing(&self, need_whisper: bool, pair: Option<(&Lang, &Lang)>) -> Vec<Download> {
        let mut out = Vec::new();
        if need_whisper {
            if self.whisper_cli().is_none() {
                out.push(self.runtime_download());
            }
            if !self.whisper_model().exists() {
                out.push(Download {
                    label: format!("Whisper model {}", self.whisper_model),
                    url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin", self.whisper_model),
                    dest: self.whisper_model(),
                    archive: None,
                });
            }
            if !self.vad_model().exists() {
                out.push(Download {
                    label: "voice detection model".into(),
                    url: format!("https://huggingface.co/ggml-org/whisper-vad/resolve/main/{VAD_MODEL}"),
                    dest: self.vad_model(),
                    archive: None,
                });
            }
        }
        if let Some((from, to)) = pair
            && let Some((_, _, repo, rev, st_rev)) =
                OPUS_PAIRS.iter().find(|(f, t, ..)| *f == from.as_str() && *t == to.as_str())
            {
                let dir = self.opus_dir(from, to);
                for (file, revision) in
                    [("config.json", rev), ("source.spm", rev), ("vocab.json", rev), ("model.safetensors", st_rev)]
                {
                    if !dir.join(file).exists() {
                        out.push(Download {
                            label: format!("translation model {from}→{to} ({file})"),
                            url: format!("https://huggingface.co/{repo}/resolve/{revision}/{file}"),
                            dest: dir.join(file),
                            archive: None,
                        });
                    }
                }
            }
        out
    }

    /// Downloads whatever is missing. `status` gets human-readable progress.
    pub async fn ensure(
        &self,
        http: &reqwest::Client,
        need_whisper: bool,
        pair: Option<(&Lang, &Lang)>,
        status: &(dyn Fn(String, f32) + Send + Sync),
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        for d in self.missing(need_whisper, pair) {
            fetch(http, &d, status, cancel).await?;
        }
        Ok(())
    }
}

fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs.iter().find_map(|d| find_file(d, name, depth - 1))
}

async fn fetch(
    http: &reqwest::Client,
    d: &Download,
    status: &(dyn Fn(String, f32) + Send + Sync),
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    tracing::info!("downloading {} from {}", d.label, d.url);
    let parent = d.dest.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let part = d.dest.with_extension("part");
    let mut resp = http.get(&d.url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(&part)?;
    let mut done = 0u64;
    let mut last_report = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("cancelled");
        }
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        if done - last_report > 2_000_000 || done == total {
            last_report = done;
            let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            let size = if total > 0 {
                format!("{} / {} MB", done / 1_000_000, total / 1_000_000)
            } else {
                format!("{} MB", done / 1_000_000)
            };
            status(format!("Downloading {} · {size}", d.label), frac);
        }
    }
    file.flush()?;
    drop(file);

    match d.archive {
        None => tokio::fs::rename(&part, &d.dest).await?,
        Some(kind) => {
            status(format!("Unpacking {}", d.label), 1.0);
            let (part2, dest) = (part.clone(), d.dest.clone());
            tokio::task::spawn_blocking(move || unpack(&part2, &dest, kind)).await??;
            let _ = tokio::fs::remove_file(&part).await;
        }
    }
    Ok(())
}

fn unpack(archive: &Path, dest: &Path, kind: Archive) -> anyhow::Result<()> {
    // Unpack next to the target and swap in, so a failed unpack never leaves
    // a half-filled runtime that looks installed.
    let tmp = dest.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    match kind {
        Archive::Zip => {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
            // `extract` rejects entries that would escape `tmp`.
            zip.extract(&tmp)?;
        }
        Archive::TarGz => {
            #[cfg(not(windows))]
            {
                let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
                tar::Archive::new(gz).unpack(&tmp)?;
            }
            #[cfg(windows)]
            anyhow::bail!("tar.gz packages are not used on Windows");
        }
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full local chain with real downloads (no Jellyfin):
    /// `RJ_TEST_AUDIO=<media file> cargo test -p redjelly models -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn downloads_and_runs_the_whole_pipeline() {
        use rj_ai::pipeline::{self, Translator};
        let input = std::env::var("RJ_TEST_AUDIO").expect("set RJ_TEST_AUDIO");
        let root = std::env::temp_dir().join("rj-models-e2e");
        let cfg = AiConfig { whisper_model: "base".into(), ..AiConfig::default() };
        let m = Models::new(root.clone(), &cfg);
        let (en, nl) = (Lang::parse("en").unwrap(), Lang::parse("nl").unwrap());
        let cancel = AtomicBool::new(false);
        let http = reqwest::Client::new();
        m.ensure(&http, true, Some((&en, &nl)), &|s, p| println!("{s} {:.0}%", p * 100.0), &cancel).await.unwrap();
        assert!(m.whisper_cli().is_some(), "whisper-cli unpacked");

        let wav = root.join("audio.wav");
        rj_player::extract::extract_audio(&input, None, None, &wav, &mut |_| {}, &cancel).unwrap();
        let mut whisper = rj_ai::whisper_cli::WhisperCli {
            exe: m.whisper_cli().unwrap(),
            model: m.whisper_model(),
            vad_model: Some(m.vad_model()),
            use_gpu: false,
            threads: 4,
            work_dir: root.join("work"),
        };
        let mut t: Box<dyn Translator> = Box::new(rj_ai::translate::OpusMt::new(vec![
            rj_ai::translate::MarianPair::load(&m.opus_dir(&en, &nl), en.clone(), nl.clone()).unwrap(),
        ]));
        let cues = pipeline::generate(&wav, Some(&en), &nl, &mut whisper, t.as_mut(), &mut |_| {}, &cancel).unwrap();
        let srt = rj_ai::srt::to_srt(&cues);
        println!("{srt}");
        assert!(srt.contains("land"), "expected Dutch output");
    }

    #[test]
    fn plans_downloads_for_a_fresh_install() {
        let root = std::env::temp_dir().join(format!("rj-models-{}", std::process::id()));
        let m = Models::new(root.clone(), &AiConfig::default());
        let (en, nl) = (Lang::parse("en").unwrap(), Lang::parse("nl").unwrap());
        let plan = m.missing(true, Some((&en, &nl)));
        let labels: Vec<_> = plan.iter().map(|d| d.label.as_str()).collect();
        assert!(labels[0].starts_with("Whisper ("), "{labels:?}");
        assert!(plan.iter().any(|d| d.url.ends_with("ggml-large-v3-turbo-q5_0.bin")));
        assert!(plan.iter().any(|d| d.url.contains("cb106147") && d.url.ends_with("model.safetensors")));
        assert_eq!(plan.len(), 3 + 4);
        // Translation only (existing English subtitle): no whisper downloads.
        assert_eq!(m.missing(false, Some((&en, &nl))).len(), 4);
    }
}
