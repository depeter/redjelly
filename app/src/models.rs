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

/// An Opus-MT model we know how to fetch. The original repos only ship
/// legacy torch weights; `model.safetensors` comes from the official
/// conversion bot's pull request (`st_rev`).
#[derive(Debug, PartialEq)]
pub struct OpusModel {
    /// `None`: multilingual source (any language → `to`).
    pub from: Option<&'static str>,
    pub to: &'static str,
    repo: &'static str,
    rev: &'static str,
    st_rev: &'static str,
}

const OPUS: &[OpusModel] = &[
    OpusModel {
        from: Some("en"),
        to: "nl",
        repo: "Helsinki-NLP/opus-mt-en-nl",
        rev: "8aad73b34ff36c090e7fc8a2eb7e2e7cca235d31",
        st_rev: "cb106147f56946f00045d2f9768539949e660489",
    },
    OpusModel {
        from: Some("es"),
        to: "nl",
        repo: "Helsinki-NLP/opus-mt-es-nl",
        rev: "f04335f1ef73b36571b410a68112dcf61d26994a",
        st_rev: "a58e172a49dbedcb0893bb22ec01fe5e70d3a0bb",
    },
    OpusModel {
        from: Some("de"),
        to: "nl",
        repo: "Helsinki-NLP/opus-mt-de-nl",
        rev: "cff6d195ed0708e0ca965c4401372af990462e82",
        st_rev: "5730380ccd37c893eae517cb17f997951926cc68",
    },
    // Everything else reaches Dutch via English.
    OpusModel {
        from: None,
        to: "en",
        repo: "Helsinki-NLP/opus-mt-mul-en",
        rev: "848eae0c1676cfce9bb791c200e8228e5a6396ff",
        st_rev: "f5c88837d58e937af4fa5a77015027305e4226f5",
    },
];

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
        self.runtime_dir_for(&self.runtime)
    }

    fn runtime_dir_for(&self, runtime: &str) -> PathBuf {
        self.root.join(format!("whisper-{WHISPER_RELEASE}-{runtime}"))
    }

    /// whisper.cpp's voice-detection tool: from the installed package, or
    /// the small CPU package (VAD doesn't need the GPU).
    pub fn vad_tool(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "whisper-vad-speech-segments.exe" } else { "whisper-vad-speech-segments" };
        find_file(&self.runtime_dir(), name, 3).or_else(|| find_file(&self.runtime_dir_for("cpu"), name, 3))
    }

    /// Downloads what audio sync needs (~10 MB): the CPU package and the VAD model.
    pub async fn ensure_vad(
        &self,
        http: &reqwest::Client,
        status: &(dyn Fn(String, f32) + Send + Sync),
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        if self.vad_tool().is_none() {
            fetch(http, &self.package_download("cpu"), status, cancel).await?;
        }
        if !self.vad_model().exists() {
            fetch(http, &self.vad_download(), status, cancel).await?;
        }
        Ok(())
    }

    fn vad_download(&self) -> Download {
        Download {
            label: "voice detection model".into(),
            url: format!("https://huggingface.co/ggml-org/whisper-vad/resolve/main/{VAD_MODEL}"),
            dest: self.vad_model(),
            archive: None,
        }
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

    pub fn opus_dir(&self, m: &OpusModel) -> PathBuf {
        self.root.join(format!("opus-mt-{}-{}", m.from.unwrap_or("mul"), m.to))
    }

    /// Direct language pairs (for choosing to translate an existing
    /// subtitle instead of running Whisper).
    pub fn opus_pairs() -> Vec<(Lang, Lang)> {
        OPUS.iter().filter_map(|m| Some((Lang::parse(m.from?)?, Lang::parse(m.to)?))).collect()
    }

    /// Models needed to bring each of `langs` to `target`: a direct model,
    /// else via English (mul-en + en→target).
    pub fn models_for(langs: &[Lang], target: &Lang) -> Vec<&'static OpusModel> {
        let en = Lang::parse("en").expect("valid");
        let exact = |f: &Lang, t: &Lang| OPUS.iter().find(|m| m.from == Some(f.as_str()) && m.to == t.as_str());
        let to_en = || OPUS.iter().find(|m| m.from.is_none() && m.to == "en");
        let mut out: Vec<&'static OpusModel> = Vec::new();
        for lang in langs.iter().filter(|l| *l != target) {
            let chain: Vec<&'static OpusModel> = match exact(lang, target) {
                Some(m) => vec![m],
                None if *lang == en => vec![],
                None => {
                    let first = exact(lang, &en).or_else(to_en);
                    match (first, *target == en) {
                        (Some(a), true) => vec![a],
                        (Some(a), false) => exact(&en, target).map(|b| vec![a, b]).unwrap_or_default(),
                        (None, _) => vec![],
                    }
                }
            };
            for m in chain {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Loads the given models into one translator (blocking; ~1 s each).
    pub fn load_translator(&self, models: &[&OpusModel]) -> anyhow::Result<rj_ai::translate::OpusMt> {
        let pairs = models
            .iter()
            .map(|m| {
                let to = Lang::parse(m.to).ok_or_else(|| anyhow::anyhow!("bad language {}", m.to))?;
                Ok(rj_ai::translate::MarianPair::load(&self.opus_dir(m), m.from.and_then(Lang::parse), to)?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(rj_ai::translate::OpusMt::new(pairs))
    }

    fn runtime_download(&self) -> Download {
        self.package_download(&self.runtime.clone())
    }

    fn package_download(&self, runtime: &str) -> Download {
        let base = format!("https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_RELEASE}");
        let (file, archive, label) = if cfg!(windows) {
            match runtime {
                "cuda11" => ("whisper-cublas-11.8.0-bin-x64.zip", Archive::Zip, "Whisper (CUDA 11, ~270 MB)"),
                "cpu" => ("whisper-bin-x64.zip", Archive::Zip, "Whisper (CPU, ~9 MB)"),
                _ => ("whisper-cublas-12.4.0-bin-x64.zip", Archive::Zip, "Whisper (CUDA 12, ~675 MB)"),
            }
        } else {
            ("whisper-bin-ubuntu-x64.tar.gz", Archive::TarGz, "Whisper (CPU, ~10 MB)")
        };
        let runtime = if cfg!(windows) { runtime } else { "cpu" };
        Download { label: label.into(), url: format!("{base}/{file}"), dest: self.runtime_dir_for(runtime), archive: Some(archive) }
    }

    /// Files missing for a job: whisper (if `need_whisper`) and the given
    /// translation models.
    fn missing(&self, need_whisper: bool, opus: &[&OpusModel]) -> Vec<Download> {
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
                out.push(self.vad_download());
            }
        }
        for m in opus {
            let dir = self.opus_dir(m);
            let name = format!("{}→{}", m.from.unwrap_or("any"), m.to);
            for (file, revision) in
                [("config.json", m.rev), ("source.spm", m.rev), ("vocab.json", m.rev), ("model.safetensors", m.st_rev)]
            {
                if !dir.join(file).exists() {
                    out.push(Download {
                        label: format!("translation model {name} ({file})"),
                        url: format!("https://huggingface.co/{}/resolve/{revision}/{file}", m.repo),
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
        opus: &[&OpusModel],
        status: &(dyn Fn(String, f32) + Send + Sync),
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        for d in self.missing(need_whisper, opus) {
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
        use rj_ai::pipeline::{self, Transcriber};
        let input = std::env::var("RJ_TEST_AUDIO").expect("set RJ_TEST_AUDIO");
        let root = std::env::temp_dir().join("rj-models-e2e");
        let model = std::env::var("RJ_TEST_MODEL").unwrap_or_else(|_| "base".into());
        let cfg = AiConfig { whisper_model: model, ..AiConfig::default() };
        let m = Models::new(root.clone(), &cfg);
        let (en, nl) = (Lang::parse("en").unwrap(), Lang::parse("nl").unwrap());
        let cancel = AtomicBool::new(false);
        let http = reqwest::Client::new();
        m.ensure(&http, true, &[], &|s, p| println!("{s} {:.0}%", p * 100.0), &cancel).await.unwrap();
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
        let cues = whisper.transcribe(&wav, Some(&en), false, &mut |_| {}, &cancel).unwrap();
        let texts: Vec<String> = cues.iter().map(|c| c.text.clone()).collect();
        let langs = rj_ai::langid::detect_lines(&texts, Some(&en));
        let mut found: Vec<Lang> = langs.iter().flatten().cloned().collect();
        found.sort();
        found.dedup();
        let needed = Models::models_for(&found, &nl);
        m.ensure(&http, false, &needed, &|s, p| println!("{s} {:.0}%", p * 100.0), &cancel).await.unwrap();
        let mut t = m.load_translator(&needed).unwrap();
        let out = pipeline::translate_mixed(cues, &langs, &en, &nl, &mut t, &mut |_| {}, &cancel).unwrap();
        let srt = rj_ai::srt::to_srt(&out.cues);
        println!("languages: {found:?}\n{srt}");
        assert!(srt.contains("land"), "expected Dutch output");
    }

    #[test]
    fn plans_downloads_for_a_fresh_install() {
        let root = std::env::temp_dir().join(format!("rj-models-{}", std::process::id()));
        let m = Models::new(root.clone(), &AiConfig::default());
        let (en, nl) = (Lang::parse("en").unwrap(), Lang::parse("nl").unwrap());
        let en_nl = Models::models_for(std::slice::from_ref(&en), &nl);
        let plan = m.missing(true, &en_nl);
        let labels: Vec<_> = plan.iter().map(|d| d.label.as_str()).collect();
        assert!(labels[0].starts_with("Whisper ("), "{labels:?}");
        assert!(plan.iter().any(|d| d.url.ends_with("ggml-large-v3-turbo-q5_0.bin")));
        assert!(plan.iter().any(|d| d.url.contains("cb106147") && d.url.ends_with("model.safetensors")));
        assert_eq!(plan.len(), 3 + 4);
        // Translation only (existing English subtitle): no whisper downloads.
        assert_eq!(m.missing(false, &en_nl).len(), 4);
    }

    #[test]
    fn picks_models_per_language() {
        let l = |s| Lang::parse(s).unwrap();
        let names = |ms: Vec<&OpusModel>| ms.iter().map(|m| format!("{}-{}", m.from.unwrap_or("mul"), m.to)).collect::<Vec<_>>();
        assert_eq!(names(Models::models_for(&[l("en"), l("es")], &l("nl"))), ["en-nl", "es-nl"]);
        assert_eq!(names(Models::models_for(&[l("fr"), l("nl")], &l("nl"))), ["mul-en", "en-nl"]);
        assert_eq!(names(Models::models_for(&[l("ja")], &l("en"))), ["mul-en"]);
        assert!(Models::models_for(&[l("nl")], &l("nl")).is_empty());
    }
}
