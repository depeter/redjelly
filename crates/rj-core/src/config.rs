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
            hwdec: "auto-safe".into(),
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
    /// mpv `tone-mapping`: auto, bt.2390, spline, mobius, hable, …
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    /// Presets cycled with Ctrl+T during playback (projector tuning).
    pub tone_mapping_presets: Vec<ToneMappingPreset>,
    /// Index of the active preset.
    pub tone_mapping: usize,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            tone_mapping_presets: vec![
                ToneMappingPreset::new("Automatic", "auto", "auto", "auto"),
                ToneMappingPreset::new("Projector · bright (200 nits)", "bt.2390", "200", "yes"),
                ToneMappingPreset::new("Projector · dim room (100 nits)", "bt.2390", "100", "yes"),
                ToneMappingPreset::new("Spline · more contrast", "spline", "auto", "yes"),
            ],
            tone_mapping: 0,
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

pub fn dirs() -> Result<directories::ProjectDirs, ConfigError> {
    directories::ProjectDirs::from("", "", "redjelly").ok_or(ConfigError::NoHome)
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(dirs()?.config_dir().join("config.toml"))
    }

    /// Loads the file, or returns defaults if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|source| ConfigError::Parse { path: path.into(), source }),
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
    fn round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.languages.preferred, ["nl", "en"]);
    }
}
