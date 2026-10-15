//! Picture presets: GLSL shader chains (Ctrl+S, remembered per show) and
//! HDR tone-mapping presets for the projector (Ctrl+T).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::App;

/// FSR 1.0 for mpv (MIT, AMD), pinned gist revision.
const FSR_URL: &str = "https://gist.githubusercontent.com/agyild/82219c545228d70c5604f865ce0b0ce5/raw/2623d743b9c23f500ba086f05b385dcb1557e15d/FSR.glsl";
/// Anime4K v4.0.1 (MIT).
const ANIME4K_URL: &str = "https://github.com/bloc97/Anime4K/releases/download/v4.0.1/Anime4K_v4.0.zip";

/// (name, shader files). Anime4K chains are the project's recommended
/// 1080p "HQ" modes.
pub const SHADER_PRESETS: &[(&str, &[&str])] = &[
    ("Off", &[]),
    ("FSR (sharper upscaling)", &["FSR.glsl"]),
    (
        "Anime4K A (blurry sources)",
        &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_VL.glsl",
            "Anime4K_Upscale_CNN_x2_VL.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        ],
    ),
    (
        "Anime4K B (aliased sources)",
        &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_Soft_VL.glsl",
            "Anime4K_Upscale_CNN_x2_VL.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        ],
    ),
    (
        "Anime4K C (clean sources)",
        &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Upscale_Denoise_CNN_x2_VL.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        ],
    ),
];

/// Downloads the shader packs if any of `files` is missing.
async fn ensure_shaders(http: &reqwest::Client, dir: &Path, files: &[&str]) -> anyhow::Result<()> {
    if files.iter().all(|f| dir.join(f).exists()) {
        return Ok(());
    }
    tokio::fs::create_dir_all(dir).await?;
    if files.contains(&"FSR.glsl") && !dir.join("FSR.glsl").exists() {
        let body = http.get(FSR_URL).send().await?.error_for_status()?.bytes().await?;
        tokio::fs::write(dir.join("FSR.glsl"), &body).await?;
    }
    if files.iter().any(|f| f.starts_with("Anime4K") && !dir.join(f).exists()) {
        let body = http.get(ANIME4K_URL).send().await?.error_for_status()?.bytes().await?;
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body))?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i)?;
                // Flatten: only the .glsl file names, never their paths.
                let Some(name) = f.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_owned())) else { continue };
                if !name.to_string_lossy().ends_with(".glsl") {
                    continue;
                }
                let mut data = Vec::new();
                f.read_to_end(&mut data)?;
                std::fs::write(dir.join(name), data)?;
            }
            Ok(())
        })
        .await??;
    }
    Ok(())
}

fn is_hdr(app: &App) -> bool {
    let gamma = app.player.mpv().get_property::<String>("video-params/gamma").unwrap_or_default();
    matches!(gamma.as_str(), "pq" | "hlg")
}

impl App {
    fn shader_dir(&self) -> PathBuf {
        self.models_dir.join("shaders")
    }

    /// Applies a shader preset by name, downloading it first if needed.
    pub fn apply_shader(&self, name: &str, announce: bool) {
        let Some((name, files)) = SHADER_PRESETS.iter().find(|(n, _)| *n == name).copied() else { return };
        let dir = self.shader_dir();
        let http = self.http.clone();
        if announce && !files.iter().all(|f| dir.join(f).exists()) {
            self.toast(format!("Downloading {name} shaders…"));
        }
        let dir2 = dir.clone();
        self.spawn_ui(async move { ensure_shaders(&http, &dir2, files).await }, move |app, r| {
            if let Err(e) = r {
                return app.toast(format!("Couldn't get shaders: {e:#}"));
            }
            let mpv = app.player.mpv();
            let _ = mpv.command("change-list", &["glsl-shaders", "clr", ""]);
            for f in files {
                let _ = mpv.command("change-list", &["glsl-shaders", "append", &dir.join(f).to_string_lossy()]);
            }
            if announce {
                app.toast(format!("Shader: {name}"));
            }
        });
    }

    /// Ctrl+S: next shader preset, remembered for this show.
    pub fn cycle_shader(&self) {
        let key = self.session.borrow().as_ref().and_then(|s| s.memory_key.clone());
        let current = key.as_ref().and_then(|k| self.state.borrow().shaders.get(k).cloned()).unwrap_or_else(|| "Off".into());
        let i = SHADER_PRESETS.iter().position(|(n, _)| *n == current).unwrap_or(0);
        let next = SHADER_PRESETS[(i + 1) % SHADER_PRESETS.len()].0;
        if let Some(k) = key {
            let mut state = self.state.borrow_mut();
            if next == "Off" {
                state.shaders.remove(&k);
            } else {
                state.shaders.insert(k, next.into());
            }
            let _ = state.save(&self.state_path);
        }
        self.apply_shader(next, true);
    }

    /// The show's remembered shader (or none) for a newly loaded file.
    pub fn apply_remembered_shader(&self) {
        let key = self.session.borrow().as_ref().and_then(|s| s.memory_key.clone());
        let name = key.and_then(|k| self.state.borrow().shaders.get(&k).cloned()).unwrap_or_else(|| "Off".into());
        self.apply_shader(&name, false);
    }

    pub fn apply_tone_mapping(&self) {
        let cfg = self.config.borrow();
        let Some(p) = cfg.video.tone_mapping_presets.get(cfg.video.tone_mapping) else { return };
        let mpv = self.player.mpv();
        for (prop, value) in
            [("tone-mapping", &p.tone_mapping), ("target-peak", &p.target_peak), ("hdr-compute-peak", &p.compute_peak)]
        {
            if let Err(e) = mpv.set_property(prop, value.as_str()) {
                tracing::warn!("{prop}={value}: {e}");
            }
        }
    }

    pub fn tone_mapping_name(&self) -> String {
        let cfg = self.config.borrow();
        cfg.video.tone_mapping_presets.get(cfg.video.tone_mapping).map(|p| p.name.clone()).unwrap_or_default()
    }

    /// Ctrl+T: next tone-mapping preset (global, saved).
    pub fn cycle_tone_mapping(&self) {
        {
            let mut cfg = self.config.borrow_mut();
            let n = cfg.video.tone_mapping_presets.len().max(1);
            cfg.video.tone_mapping = (cfg.video.tone_mapping + 1) % n;
            let _ = cfg.save(&self.config_path);
        }
        self.apply_tone_mapping();
        let note = if is_hdr(self) { "" } else { " (only affects HDR video)" };
        self.toast(format!("Tone mapping: {}{note}", self.tone_mapping_name()));
    }
}
