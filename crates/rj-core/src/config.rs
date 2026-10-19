//! `config.toml` — user settings. Every field has a default so partial files
//! and files from older versions always load.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("parsing {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("serialising config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("no home directory found")]
    NoHome,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub languages: LanguageConfig,
    pub playback: PlaybackConfig,
    pub subtitles: SubtitleConfig,
    pub providers: ProviderConfig,
    pub ai: AiConfig,
    pub video: VideoConfig,
    pub appearance: AppearanceConfig,
    pub update: UpdateConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub url: Option<String>,
    /// Generated once per install; Jellyfin uses it to identify the session.
    pub device_id: Option<String>,
    pub last_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageConfig {
    /// Ordered preference list for both audio and subtitles.
    pub preferred: Vec<String>,
    pub prefer_sdh: bool,
}

impl Default for LanguageConfig {
    fn default() -> Self {
        Self { preferred: vec!["nl".into(), "en".into()], prefer_sdh: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentAction {
    Ask,
    Auto,
    Off,
}

/// mpv `hwdec`. With embedded output video goes through the host's OpenGL
/// context, where on Windows d3d11va/dxva2 have no zero-copy interop and
/// `auto-safe` can end up decoding on the CPU (stutter in high-bitrate
/// scenes). nvdec has a CUDA to GL interop; everything else decodes on the
/// GPU and copies back. See [`Config::effective_hwdec`] for window output.
pub const DEFAULT_HWDEC: &str = if cfg!(windows) { "nvdec,auto-copy-safe" } else { "auto-safe" };

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackConfig {
    pub hwdec: String,
    pub intro: SegmentAction,
    pub recap: SegmentAction,
    pub outro: SegmentAction,
    pub preview: SegmentAction,
    pub commercial: SegmentAction,
    /// Seconds of next-episode countdown; 0 disables autoplay.
    pub next_episode_countdown: u32,
    pub progress_report_secs: u32,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            hwdec: DEFAULT_HWDEC.into(),
            intro: SegmentAction::Ask,
            recap: SegmentAction::Auto,
            outro: SegmentAction::Ask,
            preview: SegmentAction::Ask,
            commercial: SegmentAction::Ask,
            next_episode_countdown: 10,
            progress_report_secs: 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssOverride {
    No,
    Scale,
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleConfig {
    pub auto_search: bool,
    pub auto_sync_non_hash: bool,
    pub upload_to_server: bool,
    pub font: String,
    pub font_size: u32,
    pub color: String,
    pub border_size: f32,
    pub margin_y: u32,
    pub ass_override: AssOverride,
}

impl Default for SubtitleConfig {
    fn default() -> Self {
        Self {
            auto_search: true,
            auto_sync_non_hash: true,
            upload_to_server: true,
            font: "Segoe UI Semibold".into(),
            font_size: 46,
            color: "#FFFFFFFF".into(),
            border_size: 2.5,
            margin_y: 34,
            ass_override: AssOverride::No,
        }
    }
}

/// Subtitle providers. A password typed here is moved into the OS
/// credential store on the next start and removed from this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// The Jellyfin server's own subtitle plugins (free, no keys needed).
    pub jellyfin_remote: bool,
    /// subdl.com, free API key from your subdl account.
    pub subdl: bool,
    pub subdl_api_key: Option<String>,
    /// opensubtitles.com: off by default because downloads use a small
    /// daily quota (more costs money). Searching is free.
    pub opensubtitles: bool,
    pub opensubtitles_api_key: Option<String>,
    pub opensubtitles_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opensubtitles_password: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            jellyfin_remote: true,
            subdl: true,
            subdl_api_key: None,
            opensubtitles: false,
            opensubtitles_api_key: None,
            opensubtitles_username: None,
            opensubtitles_password: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// whisper.cpp model name (ggml-<name>.bin), e.g. large-v3-turbo-q5_0,
    /// medium-q5_0, small.
    pub whisper_model: String,
    /// Prebuilt whisper.cpp package: "cuda12", "cuda11" (older drivers) or
    /// "cpu". Linux always uses the CPU build.
    pub whisper_runtime: String,
    pub use_gpu: bool,
    pub vad: bool,
    /// CPU threads for whisper; 0 = all cores.
    pub threads: usize,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            whisper_model: "large-v3-turbo-q5_0".into(),
            whisper_runtime: "cuda12".into(),
            use_gpu: true,
            vad: true,
            threads: 0,
        }
    }
}

/// HDR → SDR tone-mapping settings (mpv options), tuned for the screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToneMappingPreset {
    pub name: String,
    /// mpv `tone-mapping`: auto, bt.2390, hable, mobius, reinhard, …
    /// (the embedded player is vo_gpu: gpu-next-only curves like spline fail).
    pub tone_mapping: String,
    /// mpv `target-peak` in nits, or "auto".
    pub target_peak: String,
    /// mpv `hdr-compute-peak`: auto, yes, no.
    pub compute_peak: String,
}

impl ToneMappingPreset {
    fn new(name: &str, tone_mapping: &str, target_peak: &str, compute_peak: &str) -> Self {
        Self { name: name.into(), tone_mapping: tone_mapping.into(), target_peak: target_peak.into(), compute_peak: compute_peak.into() }
    }
}

/// How much GPU work mpv spends on scaling and debanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PictureQuality {
    /// Bilinear scaling, no debanding: for weak GPUs or 4K HDR stutter.
    Fast,
    /// Sharp scalers, no debanding.
    Balanced,
    /// EWA scaling, debanding, linear-light downscaling: heavy at 4K, fine
    /// on a decent GPU once decoding is on the GPU too (see DEFAULT_HWDEC).
    #[default]
    High,
}

impl PictureQuality {
    pub const ALL: [Self; 3] = [Self::Fast, Self::Balanced, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::High => "High (heavy on the GPU)",
        }
    }
}

/// Where mpv draws the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoOutput {
    /// mpv's own native window under the (transparent) UI window: D3D11,
    /// gpu-next and display-synced timing, like standalone mpv. Windows only.
    Window,
    /// mpv's render API inside the UI's OpenGL context. Frame timing is at
    /// the mercy of the UI toolkit's redraws, so motion can judder.
    Embedded,
}

impl VideoOutput {
    pub const ALL: [Self; 2] = [Self::Window, Self::Embedded];

    pub fn label(self) -> &'static str {
        match self {
            Self::Window => "Own window (smoothest)",
            Self::Embedded => "Embedded (OpenGL)",
        }
    }
}

impl Default for VideoOutput {
    fn default() -> Self {
        if cfg!(windows) { Self::Window } else { Self::Embedded }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    /// Applies on restart.
    pub output: VideoOutput,
    pub quality: PictureQuality,
    /// Presets cycled with Ctrl+T during playback (projector tuning).
    pub tone_mapping_presets: Vec<ToneMappingPreset>,
    /// Index of the active preset.
    pub tone_mapping: usize,
}

impl VideoConfig {
    /// Defaults written to config files before the presets were reworked;
    /// a file still holding exactly these gets the current ones.
    fn old_default_presets() -> Vec<ToneMappingPreset> {
        vec![
            ToneMappingPreset::new("Automatic", "auto", "auto", "auto"),
            ToneMappingPreset::new("Projector · bright (200 nits)", "bt.2390", "200", "yes"),
            ToneMappingPreset::new("Projector · dim room (100 nits)", "bt.2390", "100", "yes"),
            ToneMappingPreset::new("Spline · more contrast", "spline", "auto", "yes"),
        ]
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            tone_mapping_presets: vec![
                ToneMappingPreset::new("Automatic", "auto", "auto", "auto"),
                ToneMappingPreset::new("Projector · bright (200 nits)", "bt.2390", "200", "yes"),
                ToneMappingPreset::new("Projector · dim room (100 nits)", "bt.2390", "100", "yes"),
                // Filmic curve: more contrast and colour, less grey.
                ToneMappingPreset::new("Punchy · more contrast", "hable", "auto", "yes"),
                ToneMappingPreset::new("Punchy · projector (150 nits)", "hable", "150", "yes"),
                // Keeps mid-tones as mastered, only rolls off highlights.
                ToneMappingPreset::new("Natural · keeps colours", "mobius", "auto", "yes"),
            ],
            tone_mapping: 0,
            output: VideoOutput::default(),
            quality: PictureQuality::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// UI zoom for 10-foot viewing (1.0 = 100 %). Applies on restart.
    pub ui_scale: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self { ui_scale: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// Look for a new release at startup and every 12 hours.
    pub auto_check: bool,
    /// A release the user chose to skip (e.g. "0.1.5"): no reminder for it.
    pub skipped: Option<String>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { auto_check: true, skipped: None }
    }
}

pub fn dirs() -> Result<directories::ProjectDirs, ConfigError> {
    directories::ProjectDirs::from("", "", "redjelly").ok_or(ConfigError::NoHome)
}

impl Config {
    /// Video output actually used: window output only exists on Windows.
    pub fn video_output(&self) -> VideoOutput {
        if cfg!(windows) { self.video.output } else { VideoOutput::Embedded }
    }

    /// The `hwdec` to give mpv. In its own D3D11 window mpv has zero-copy
    /// d3d11va, so the default's OpenGL workaround becomes plain `auto-safe`.
    pub fn effective_hwdec(&self) -> &str {
        if self.video_output() == VideoOutput::Window && self.playback.hwdec == DEFAULT_HWDEC {
            "auto-safe"
        } else {
            &self.playback.hwdec
        }
    }

    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(dirs()?.config_dir().join("config.toml"))
    }

    /// Loads the file, or returns defaults if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg: Self =
                    toml::from_str(&text).map_err(|source| ConfigError::Parse { path: path.into(), source })?;
                // The old Windows default; see DEFAULT_HWDEC.
                if cfg!(windows) && cfg.playback.hwdec == "auto-safe" {
                    cfg.playback.hwdec = DEFAULT_HWDEC.into();
                }
                if cfg.video.tone_mapping_presets == VideoConfig::old_default_presets() {
                    cfg.video.tone_mapping_presets = VideoConfig::default().tone_mapping_presets;
                }
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io { path: path.into(), source }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io = |source| ConfigError::Io { path: path.into(), source };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?).map_err(io)
    }

    pub fn track_preferences(&self) -> crate::tracks::Preferences {
        crate::tracks::Preferences {
            languages: self.languages.preferred.iter().filter_map(|s| crate::lang::Lang::parse(s)).collect(),
            prefer_sdh: self.languages.prefer_sdh,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_defaults() {
        let cfg: Config = toml::from_str("[languages]\npreferred = [\"en\"]\n").unwrap();
        assert_eq!(cfg.languages.preferred, ["en"]);
        assert_eq!(cfg.playback.intro, SegmentAction::Ask);
        assert!(!cfg.providers.opensubtitles);
        assert!(cfg.providers.subdl);
    }

    #[test]
    fn old_default_presets_are_upgraded() {
        let mut old = Config::default();
        old.video.tone_mapping_presets = VideoConfig::old_default_presets();
        old.video.tone_mapping = 2;
        let path = std::env::temp_dir().join(format!("rj-config-{}.toml", std::process::id()));
        old.save(&path).unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cfg.video.tone_mapping_presets, VideoConfig::default().tone_mapping_presets);
        assert_eq!(cfg.video.tone_mapping, 2);
        assert!(cfg.video.tone_mapping_presets.iter().all(|p| p.tone_mapping != "spline"));
    }

    #[test]
    fn round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.languages.preferred, ["nl", "en"]);
    }
}
